//! What carries the packets.

use std::fmt::Debug;
use std::hash::Hash;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::MAX_PACKET_SIZE;

/// Sends and receives whole datagrams, unreliably, without blocking.
///
/// Both calls take the caller's clock so a wrapper such as [`crate::Conditioned`] can hold packets
/// back; a real socket ignores it.
pub trait Transport {
    /// Who a packet comes from or goes to.
    type Address: Copy + Eq + Hash + Debug;

    /// Sends one packet to `to`. Like any datagram it may be lost; a transport drops rather than
    /// blocks when it can't send now.
    ///
    /// # Errors
    /// Whatever the transport can't recover from, such as a closed socket.
    fn send(&mut self, now: Instant, to: Self::Address, packet: &[u8]) -> io::Result<()>;

    /// The next packet waiting, written to the start of `buffer`, or `None` when nothing is
    /// waiting. A packet longer than `buffer` may be cut short.
    ///
    /// # Errors
    /// Whatever the transport can't recover from.
    fn receive(
        &mut self,
        now: Instant,
        buffer: &mut [u8],
    ) -> io::Result<Option<Received<Self::Address>>>;
}

/// A packet taken from a [`Transport`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Received<A> {
    /// How many bytes of the buffer it filled.
    pub length: usize,
    /// Who sent it.
    pub from: A,
    /// When it arrived, which may be well before it was taken: round-trip times count from here,
    /// so they measure the network rather than how often the receiver looks.
    pub at: Instant,
}

/// A UDP socket. A thread of its own waits on it and stamps each packet with the moment it
/// arrives; the game takes them when it's ready, without blocking.
pub struct UdpTransport {
    socket: UdpSocket,
    arrivals: Receiver<io::Result<Arrival>>,
    stop: Arc<AtomicBool>,
    /// An error the receiving thread hit, kept to return again.
    failed: Option<io::ErrorKind>,
}

/// A packet as the receiving thread hands it over: when it came, from whom, and its bytes.
type Arrival = (Instant, SocketAddr, Box<[u8]>);

/// How long the receiving thread waits for a packet before checking whether it should stop.
const POLL: Duration = Duration::from_millis(50);
/// How long a send may wait for room before the packet is dropped.
const SEND_WAIT: Duration = Duration::from_millis(1);

impl UdpTransport {
    /// Binds a socket: `"0.0.0.0:27015"` for a server on a known port, `"0.0.0.0:0"` for a client
    /// on any.
    ///
    /// # Errors
    /// When the address doesn't resolve, the port is taken, or the receiving thread can't start.
    pub fn bind(address: impl ToSocketAddrs) -> io::Result<UdpTransport> {
        let socket = UdpSocket::bind(address)?;
        socket.set_read_timeout(Some(POLL))?;
        socket.set_write_timeout(Some(SEND_WAIT))?;
        let listener = socket.try_clone()?;
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, arrivals) = mpsc::channel();
        let stopping = Arc::clone(&stop);
        thread::Builder::new()
            .name("mulciber-net receive".into())
            .spawn(move || {
                let mut buffer = [0; MAX_PACKET_SIZE + 1];
                while !stopping.load(Ordering::Relaxed) {
                    let arrival = match listener.recv_from(&mut buffer) {
                        Ok((length, from)) => Ok((Instant::now(), from, buffer[..length].into())),
                        Err(error)
                            if error.kind() == io::ErrorKind::TimedOut || transient(&error) =>
                        {
                            continue;
                        }
                        Err(error) => Err(error),
                    };
                    let failed = arrival.is_err();
                    if sender.send(arrival).is_err() || failed {
                        break;
                    }
                }
            })?;
        Ok(UdpTransport {
            socket,
            arrivals,
            stop,
            failed: None,
        })
    }

    /// The address the socket is bound to, with the port the system chose for port 0.
    ///
    /// # Errors
    /// When the system can't say.
    pub fn local_address(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}

impl Drop for UdpTransport {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Errors a datagram socket reports that concern one packet, not the socket: no data or no room
/// yet, a signal arriving mid-wait (a debugger, the terminal stopping and continuing the process),
/// or on Windows an earlier packet's ICMP "port unreachable" or an oversized datagram.
fn transient(error: &io::Error) -> bool {
    #[cfg(windows)]
    const WSAEMSGSIZE: i32 = 10040;
    #[cfg(windows)]
    if error.raw_os_error() == Some(WSAEMSGSIZE) {
        return true;
    }
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock
            | io::ErrorKind::Interrupted
            | io::ErrorKind::TimedOut
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
    )
}

impl Transport for UdpTransport {
    type Address = SocketAddr;

    fn send(&mut self, _now: Instant, to: SocketAddr, packet: &[u8]) -> io::Result<()> {
        match self.socket.send_to(packet, to) {
            Err(error) if !transient(&error) => Err(error),
            _ => Ok(()),
        }
    }

    fn receive(
        &mut self,
        _now: Instant,
        buffer: &mut [u8],
    ) -> io::Result<Option<Received<SocketAddr>>> {
        if let Some(kind) = self.failed {
            return Err(kind.into());
        }
        match self.arrivals.try_recv() {
            Ok(Ok((at, from, packet))) => {
                let length = packet.len().min(buffer.len());
                buffer[..length].copy_from_slice(&packet[..length]);
                Ok(Some(Received { length, from, at }))
            }
            Ok(Err(error)) => {
                self.failed = Some(error.kind());
                Err(error)
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_or_a_lost_packet_is_not_a_broken_socket() {
        for kind in [
            io::ErrorKind::WouldBlock,
            io::ErrorKind::Interrupted,
            io::ErrorKind::TimedOut,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionRefused,
        ] {
            assert!(transient(&kind.into()), "{kind:?}");
        }
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::AddrNotAvailable,
        ] {
            assert!(!transient(&kind.into()), "{kind:?}");
        }
    }
}
