//! A network inside one process, for tests and for running a server and its clients together.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use crate::transport::{Received, Transport};

/// An endpoint's address on a [`MemoryNetwork`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MemoryAddress(pub u32);

type Inboxes = HashMap<MemoryAddress, VecDeque<(MemoryAddress, Box<[u8]>)>>;

/// A perfect network between [`MemoryTransport`]s: every packet arrives, at once and in order.
/// Wrap endpoints in [`crate::Conditioned`] for anything worse. Clones share the network.
#[derive(Clone, Default)]
pub struct MemoryNetwork {
    inboxes: Arc<Mutex<Inboxes>>,
}

impl MemoryNetwork {
    /// An empty network.
    #[must_use]
    pub fn new() -> MemoryNetwork {
        MemoryNetwork::default()
    }

    /// An endpoint at `address`, replacing any already there. Packets to an address with no
    /// endpoint are lost.
    #[must_use]
    pub fn endpoint(&self, address: MemoryAddress) -> MemoryTransport {
        self.lock().insert(address, VecDeque::new());
        MemoryTransport {
            network: self.clone(),
            address,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inboxes> {
        self.inboxes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One endpoint on a [`MemoryNetwork`]. Dropping it takes its address off the network.
pub struct MemoryTransport {
    network: MemoryNetwork,
    address: MemoryAddress,
}

impl MemoryTransport {
    /// This endpoint's address.
    #[must_use]
    pub fn address(&self) -> MemoryAddress {
        self.address
    }
}

impl Transport for MemoryTransport {
    type Address = MemoryAddress;

    fn send(&mut self, _now: Instant, to: MemoryAddress, packet: &[u8]) -> io::Result<()> {
        if let Some(inbox) = self.network.lock().get_mut(&to) {
            inbox.push_back((self.address, packet.into()));
        }
        Ok(())
    }

    fn receive(
        &mut self,
        now: Instant,
        buffer: &mut [u8],
    ) -> io::Result<Option<Received<MemoryAddress>>> {
        let packet = self
            .network
            .lock()
            .get_mut(&self.address)
            .and_then(VecDeque::pop_front);
        Ok(packet.map(|(from, packet)| {
            let length = packet.len().min(buffer.len());
            buffer[..length].copy_from_slice(&packet[..length]);
            Received {
                length,
                from,
                at: now,
            }
        }))
    }
}

impl Drop for MemoryTransport {
    fn drop(&mut self) {
        self.network.lock().remove(&self.address);
    }
}
