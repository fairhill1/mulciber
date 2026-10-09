//! Game networking over UDP: a dedicated [`Server`] and its [`Client`]s, exchanging reliable ordered
//! messages and unreliable ones over a lossy, reordering, duplicating network.
//!
//! The design follows the client/server model of Quake 3 and Source, and Glenn Fiedler's articles
//! on game networking (`docs/net-contract.md`):
//!
//! - **Connecting.** A client asks; the server answers with a cookie that proves the client owns its
//!   address, and only then gives it a slot. A request is padded to a full packet, so the server
//!   never answers with more than it was sent. Peers whose [`Config::protocol`] differs refuse each
//!   other.
//! - **Packets.** Every packet has a sequence number and acknowledges the latest the sender has
//!   received plus the 32 before it, so each side learns which of its packets arrived. Duplicate and
//!   very old packets are dropped whole.
//! - **Reliable messages** ([`Channel::Reliable`]) are split into fragments that are resent until a
//!   packet carrying them is acknowledged, and delivered once each, in the order they were sent.
//! - **Unreliable messages** ([`Channel::Unreliable`]) go out once, in the next flush. One larger
//!   than a packet is split into fragments and delivered only if every fragment arrives.
//! - **Time** is the caller's: every call that does anything takes `now`, so the same code runs in
//!   a game loop and in a test stepping a fake clock.
//!
//! What a message means, how often to send, and what to do with stale state are the game's. A
//! [`Transport`] carries the packets: [`UdpTransport`] for real sockets, [`MemoryTransport`] inside
//! one process, and [`Conditioned`] around either to add latency, jitter, loss and duplication.
//!
//! ```
//! use std::time::{Duration, Instant};
//! use mulciber_net::{Channel, Client, ClientState, Config, MemoryAddress, MemoryNetwork, Server, ServerConfig, ServerEvent};
//!
//! let network = MemoryNetwork::new();
//! let config = Config::new(0x5348_4950_0001);
//! let mut server = Server::new(network.endpoint(MemoryAddress(1)), ServerConfig::new(config, 32));
//! let mut client = Client::connect(network.endpoint(MemoryAddress(2)), MemoryAddress(1), config, b"name=Ann", Instant::now()).unwrap();
//!
//! let mut now = Instant::now();
//! while client.state() == ClientState::Connecting {
//!     now += Duration::from_millis(16);
//!     server.update(now).unwrap();
//!     client.update(now).unwrap();
//!     server.flush(now).unwrap();
//!     client.flush(now).unwrap();
//! }
//! let Some(ServerEvent::Connected { client: id, payload }) = server.poll_event() else { panic!() };
//! assert_eq!(&*payload, b"name=Ann");
//!
//! server.send(id, Channel::Reliable, b"welcome aboard").unwrap();
//! server.flush(now).unwrap();
//! client.update(now).unwrap();
//! assert_eq!(&*client.receive().unwrap().data, b"welcome aboard");
//! ```

mod client;
mod conditioner;
mod connection;
mod memory;
mod packet;
mod random;
mod sequence;
mod server;
mod transport;

use std::time::Duration;

pub use client::{Client, ClientState};
pub use conditioner::{Conditioned, Conditions};
pub use connection::Stats;
pub use memory::{MemoryAddress, MemoryNetwork, MemoryTransport};
pub use server::{ClientId, Server, ServerConfig, ServerEvent};
pub use transport::{Received, Transport, UdpTransport};

/// The largest packet either side sends, in bytes: small enough to cross the internet unfragmented
/// (IPv6 guarantees 1280 bytes, less its own and UDP's headers).
pub const MAX_PACKET_SIZE: usize = 1200;
/// The largest connect payload a client can send, in bytes: it rides in one packet.
pub const MAX_CONNECT_PAYLOAD: usize = 1024;
/// The largest unreliable message, in bytes: 255 fragments.
pub const MAX_UNRELIABLE_SIZE: usize = 255 * connection::FRAGMENT_SIZE;

/// How a message travels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    /// Delivered exactly once, in the order sent. For events: a door opened, a player joined.
    Reliable,
    /// Sent once; may arrive late, out of order or not at all. For state that the next update
    /// replaces: positions, inputs.
    Unreliable,
}

/// A message received from the other end.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
    /// The channel it was sent on.
    pub channel: Channel,
    /// Its bytes, exactly as sent.
    pub data: Box<[u8]>,
}

/// Settings both ends of a connection need.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// The game and its network version. A server and a client with different protocols refuse
    /// each other, so bump it whenever the messages change.
    pub protocol: u64,
    /// How long without hearing from the other end before giving up on it, connecting or connected.
    pub timeout: Duration,
    /// The largest reliable message the other end may send. A peer that sends a larger one is
    /// disconnected as [`DisconnectReason::Misbehaved`].
    pub max_message_size: usize,
    /// How many bytes of reliable messages may wait unacknowledged before [`SendError::Backlogged`].
    pub max_backlog: usize,
    /// How many bytes of reliable messages each flush sends at most, new and resent together.
    pub reliable_bytes_per_flush: usize,
}

impl Config {
    /// Defaults: a 10 second timeout, reliable messages up to 1 MiB, an 8 MiB backlog and 8 KiB of
    /// reliable data per flush (512 KiB a second at 64 flushes a second).
    #[must_use]
    pub const fn new(protocol: u64) -> Config {
        Config {
            protocol,
            timeout: Duration::from_secs(10),
            max_message_size: 1 << 20,
            max_backlog: 8 << 20,
            reliable_bytes_per_flush: 8 << 10,
        }
    }
}

/// Why a connection ended, or never began.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DisconnectReason {
    /// Nothing heard from the other end for [`Config::timeout`].
    TimedOut,
    /// The other end said goodbye.
    ClosedByPeer,
    /// This end hung up.
    ClosedLocally,
    /// The server had no free slot.
    ServerFull,
    /// The other end runs a different [`Config::protocol`].
    WrongProtocol,
    /// The other end sent something it never should, such as an oversized message.
    Misbehaved,
}

/// Why a message was not queued.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SendError {
    /// The message is larger than the channel carries: [`MAX_UNRELIABLE_SIZE`], or the
    /// [`Config::max_message_size`] for reliable ones.
    TooLarge,
    /// Too many reliable bytes are waiting to be acknowledged ([`Config::max_backlog`]): the other
    /// end isn't keeping up.
    Backlogged,
    /// There's no such connection, or it isn't connected (yet, or any more).
    NotConnected,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SendError::TooLarge => "message too large",
            SendError::Backlogged => "too many reliable messages waiting",
            SendError::NotConnected => "not connected",
        })
    }
}

impl std::error::Error for SendError {}
