//! The dedicated server: hands out slots to clients that prove their address, and keeps a
//! connection to each.

use std::collections::VecDeque;
use std::hash::{BuildHasher, RandomState};
use std::io;
use std::time::Instant;

use crate::connection::{Connection, Stats};
use crate::packet::{
    ACCEPTED, CHALLENGE, CHALLENGE_RESPONSE, CONNECT_REQUEST, DENIED, DISCONNECT, PAYLOAD,
    PAYLOAD_UNACKED, Reader, Writer, reason_code,
};
use crate::random::unpredictable;
use crate::transport::Transport;
use crate::{Channel, Config, DisconnectReason, MAX_PACKET_SIZE, Message, SendError};

/// How long a challenge's cookie stays good: between one and two of these.
const COOKIE_SECONDS: u64 = 10;
/// How many times a goodbye is sent, in case some are lost.
pub(crate) const GOODBYES: usize = 3;

/// A connected client, unique for the server's life: ids aren't reused when clients leave.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ClientId(pub u64);

/// Settings for a [`Server`].
#[derive(Clone, Copy, Debug)]
pub struct ServerConfig {
    /// What each connection uses.
    pub config: Config,
    /// How many clients may be connected at once.
    pub max_clients: usize,
}

impl ServerConfig {
    /// `config` for every connection, up to `max_clients` at once.
    #[must_use]
    pub const fn new(config: Config, max_clients: usize) -> ServerConfig {
        ServerConfig {
            config,
            max_clients,
        }
    }
}

/// Something that happened to a client, from [`Server::poll_event`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ServerEvent {
    /// A client joined, sending `payload` with its request (a name, an auth ticket). To refuse it,
    /// [`Server::disconnect`] it.
    Connected {
        /// Its id from now on.
        client: ClientId,
        /// What it passed to [`crate::Client::connect`].
        payload: Box<[u8]>,
    },
    /// A client left, or was dropped. Messages it sent before are still read with
    /// [`Server::receive`] until this event is polled.
    Disconnected {
        /// Who.
        client: ClientId,
        /// Why.
        reason: DisconnectReason,
    },
}

struct Slot<A> {
    id: ClientId,
    address: A,
    /// The client's nonce, telling a resent response from a new client at the same address.
    nonce: u64,
    connection: Connection,
}

/// A dedicated server.
///
/// Each tick: [`Server::update`] to take in packets, [`Server::poll_event`] and
/// [`Server::receive`] to read what came, then [`Server::send`] and finally [`Server::flush`].
pub struct Server<T: Transport> {
    transport: T,
    config: ServerConfig,
    secret: RandomState,
    started: Instant,
    slots: Vec<Slot<T::Address>>,
    /// Clients gone whose departure hasn't been polled yet, kept for their last messages.
    leaving: Vec<Slot<T::Address>>,
    next_id: u64,
    events: VecDeque<ServerEvent>,
    buffer: Box<[u8]>,
}

impl<T: Transport> Server<T> {
    /// A server taking packets from `transport`.
    pub fn new(transport: T, config: ServerConfig) -> Server<T> {
        Server {
            transport,
            config,
            secret: RandomState::new(),
            started: Instant::now(),
            slots: Vec::new(),
            leaving: Vec::new(),
            next_id: 0,
            events: VecDeque::new(),
            buffer: vec![0; MAX_PACKET_SIZE + 1].into_boxed_slice(),
        }
    }

    /// The transport, as for its bound address.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Takes in every packet waiting, and drops clients not heard from in a while.
    ///
    /// # Errors
    /// When the transport fails.
    pub fn update(&mut self, now: Instant) -> io::Result<()> {
        while let Some(received) = self.transport.receive(now, &mut self.buffer)? {
            if received.length <= MAX_PACKET_SIZE {
                self.packet(received.at, received.length, received.from)?;
            }
        }
        let mut k = 0;
        while k < self.slots.len() {
            if self.slots[k].connection.timed_out(now) {
                self.drop_slot(k, DisconnectReason::TimedOut);
            } else {
                k += 1;
            }
        }
        Ok(())
    }

    fn packet(&mut self, now: Instant, size: usize, from: T::Address) -> io::Result<()> {
        let buffer = std::mem::take(&mut self.buffer);
        let mut reader = Reader::new(&buffer[..size]);
        let result = match reader.u8() {
            Some(CONNECT_REQUEST) => self.connect_request(now, size, from, reader),
            Some(CHALLENGE_RESPONSE) => self.challenge_response(now, from, reader),
            Some(kind @ (PAYLOAD | PAYLOAD_UNACKED | DISCONNECT)) => {
                self.payload(now, kind, from, reader, size);
                Ok(())
            }
            _ => Ok(()),
        };
        self.buffer = buffer;
        result
    }

    fn cookie(&self, now: Instant, address: T::Address, nonce: u64, age: u64) -> u64 {
        let window = now.saturating_duration_since(self.started).as_secs() / COOKIE_SECONDS;
        self.secret
            .hash_one((address, nonce, window.wrapping_sub(age)))
    }

    fn connect_request(
        &mut self,
        now: Instant,
        size: usize,
        from: T::Address,
        mut reader: Reader<'_>,
    ) -> io::Result<()> {
        // Padded to a full packet, so no answer is larger than what asked for it.
        let (Some(protocol), Some(nonce)) = (reader.u64(), reader.u64()) else {
            return Ok(());
        };
        if size < MAX_PACKET_SIZE {
            return Ok(());
        }
        if protocol != self.config.config.protocol {
            return self.deny(now, from, nonce, DisconnectReason::WrongProtocol);
        }
        let mut challenge = Writer::new(CHALLENGE);
        challenge.u64(nonce).u64(self.cookie(now, from, nonce, 0));
        self.transport.send(now, from, &challenge.bytes)
    }

    fn deny(
        &mut self,
        now: Instant,
        to: T::Address,
        nonce: u64,
        reason: DisconnectReason,
    ) -> io::Result<()> {
        let mut denied = Writer::new(DENIED);
        denied.u64(nonce).u8(reason_code(reason));
        self.transport.send(now, to, &denied.bytes)
    }

    fn accept(&mut self, now: Instant, to: T::Address, nonce: u64, session: u64) -> io::Result<()> {
        let mut accepted = Writer::new(ACCEPTED);
        accepted.u64(nonce).u64(session);
        self.transport.send(now, to, &accepted.bytes)
    }

    fn challenge_response(
        &mut self,
        now: Instant,
        from: T::Address,
        mut reader: Reader<'_>,
    ) -> io::Result<()> {
        let (Some(protocol), Some(nonce), Some(cookie), Some(payload)) =
            (reader.u64(), reader.u64(), reader.u64(), reader.data())
        else {
            return Ok(());
        };
        if protocol != self.config.config.protocol
            || (cookie != self.cookie(now, from, nonce, 0)
                && cookie != self.cookie(now, from, nonce, 1))
        {
            return Ok(());
        }
        if let Some(k) = self.slots.iter().position(|slot| slot.address == from) {
            if self.slots[k].nonce == nonce {
                // The client never got its acceptance.
                let session = self.slots[k].connection.session();
                return self.accept(now, from, nonce, session);
            }
            // The client at this address started over.
            self.drop_slot(k, DisconnectReason::ClosedByPeer);
        }
        if self.slots.len() >= self.config.max_clients {
            return self.deny(now, from, nonce, DisconnectReason::ServerFull);
        }
        let id = ClientId(self.next_id);
        self.next_id += 1;
        let session = unpredictable();
        self.slots.push(Slot {
            id,
            address: from,
            nonce,
            connection: Connection::new(self.config.config, session, now),
        });
        self.events.push_back(ServerEvent::Connected {
            client: id,
            payload: payload.into(),
        });
        self.accept(now, from, nonce, session)
    }

    fn payload(
        &mut self,
        now: Instant,
        kind: u8,
        from: T::Address,
        mut reader: Reader<'_>,
        size: usize,
    ) {
        let Some(k) = self.slots.iter().position(|slot| slot.address == from) else {
            return;
        };
        if reader.u64() != Some(self.slots[k].connection.session()) {
            return;
        }
        if kind == DISCONNECT {
            self.drop_slot(k, DisconnectReason::ClosedByPeer);
        } else if self.slots[k]
            .connection
            .process(now, kind, reader, size)
            .is_err()
        {
            self.goodbye(now, k);
            self.drop_slot(k, DisconnectReason::Misbehaved);
        }
    }

    /// Removes slot `k`, keeping what the client sent for [`Server::receive`] until the event is
    /// polled.
    fn drop_slot(&mut self, k: usize, reason: DisconnectReason) {
        let slot = self.slots.swap_remove(k);
        self.events.push_back(ServerEvent::Disconnected {
            client: slot.id,
            reason,
        });
        self.leaving.push(slot);
    }

    fn goodbye(&mut self, now: Instant, k: usize) {
        let slot = &self.slots[k];
        let mut goodbye = Writer::new(DISCONNECT);
        goodbye.u64(slot.connection.session());
        for _ in 0..GOODBYES {
            // A goodbye that can't be sent is no worse than one lost.
            let _ = self.transport.send(now, slot.address, &goodbye.bytes);
        }
    }

    /// What happened since the last call, oldest first.
    pub fn poll_event(&mut self) -> Option<ServerEvent> {
        let event = self.events.pop_front();
        if let Some(ServerEvent::Disconnected { client, .. }) = &event {
            self.leaving.retain(|slot| slot.id != *client);
        }
        event
    }

    /// The next message from `client`, in the order received (reliable ones in the order sent).
    pub fn receive(&mut self, client: ClientId) -> Option<Message> {
        self.slots
            .iter_mut()
            .chain(&mut self.leaving)
            .find(|slot| slot.id == client)?
            .connection
            .receive()
    }

    /// Queues a message to `client` for the next flush.
    ///
    /// # Errors
    /// See [`SendError`].
    pub fn send(
        &mut self,
        client: ClientId,
        channel: Channel,
        data: &[u8],
    ) -> Result<(), SendError> {
        self.slot_mut(client)
            .ok_or(SendError::NotConnected)?
            .connection
            .send(channel, data)
    }

    /// Queues a message to every connected client. Stops at the first that fails.
    ///
    /// # Errors
    /// See [`SendError`].
    pub fn broadcast(&mut self, channel: Channel, data: &[u8]) -> Result<(), SendError> {
        self.slots
            .iter_mut()
            .try_for_each(|slot| slot.connection.send(channel, data))
    }

    /// Sends what's queued to every client, with acks and keepalives.
    ///
    /// # Errors
    /// The first error the transport gave; every client is still flushed.
    pub fn flush(&mut self, now: Instant) -> io::Result<()> {
        let mut result = Ok(());
        for slot in &mut self.slots {
            for packet in slot.connection.flush(now) {
                if let Err(error) = self.transport.send(now, slot.address, &packet) {
                    result = result.and(Err(error));
                }
            }
        }
        result
    }

    /// Drops `client`, telling it so. No event follows: the caller knows.
    pub fn disconnect(&mut self, client: ClientId, now: Instant) {
        if let Some(k) = self.slots.iter().position(|slot| slot.id == client) {
            self.goodbye(now, k);
            self.slots.swap_remove(k);
        }
    }

    /// The connected clients.
    pub fn clients(&self) -> impl Iterator<Item = ClientId> + '_ {
        self.slots.iter().map(|slot| slot.id)
    }

    /// Where `client` connects from.
    #[must_use]
    pub fn address(&self, client: ClientId) -> Option<T::Address> {
        self.slots
            .iter()
            .find(|slot| slot.id == client)
            .map(|slot| slot.address)
    }

    /// How `client`'s connection is doing.
    #[must_use]
    pub fn stats(&self, client: ClientId) -> Option<Stats> {
        self.slots
            .iter()
            .find(|slot| slot.id == client)
            .map(|slot| slot.connection.stats())
    }

    fn slot_mut(&mut self, client: ClientId) -> Option<&mut Slot<T::Address>> {
        self.slots.iter_mut().find(|slot| slot.id == client)
    }
}
