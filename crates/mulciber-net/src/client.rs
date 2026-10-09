//! A client: connects to one server and keeps the connection.

use std::io;
use std::time::{Duration, Instant};

use crate::connection::{Connection, Stats};
use crate::packet::{
    ACCEPTED, CHALLENGE, CHALLENGE_RESPONSE, CONNECT_REQUEST, DENIED, DISCONNECT, PAYLOAD,
    PAYLOAD_UNACKED, Reader, Writer, reason_from_code,
};
use crate::random::unpredictable;
use crate::server::GOODBYES;
use crate::transport::Transport;
use crate::{
    Channel, Config, DisconnectReason, MAX_CONNECT_PAYLOAD, MAX_PACKET_SIZE, Message, SendError,
};

/// How often a request or response goes again while the server hasn't answered.
const RETRY: Duration = Duration::from_millis(100);

/// Where a client stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClientState {
    /// Asking the server for a slot.
    Connecting,
    /// Connected: messages flow.
    Connected,
    /// Done, for good; connect anew to try again.
    Disconnected(DisconnectReason),
}

enum Phase {
    Requesting,
    Responding {
        cookie: u64,
    },
    Connected(Connection),
    /// With the connection it ended, if any, for the messages still unread.
    Disconnected(DisconnectReason, Option<Connection>),
}

/// A client of a [`crate::Server`].
///
/// Each tick: [`Client::update`] to take in packets, [`Client::receive`] to read them, then
/// [`Client::send`] and finally [`Client::flush`], which also sends the connection requests.
pub struct Client<T: Transport> {
    transport: T,
    server: T::Address,
    config: Config,
    nonce: u64,
    payload: Box<[u8]>,
    phase: Phase,
    /// When the server was last heard from, or the connection began.
    heard: Instant,
    last_attempt: Option<Instant>,
    buffer: Box<[u8]>,
}

impl<T: Transport> Client<T> {
    /// Starts connecting to `server`, sending it `payload` (a name, an auth ticket) to read in its
    /// [`crate::ServerEvent::Connected`]. Nothing is sent until the first [`Client::flush`].
    ///
    /// # Errors
    /// [`SendError::TooLarge`] for a payload over [`MAX_CONNECT_PAYLOAD`].
    pub fn connect(
        transport: T,
        server: T::Address,
        config: Config,
        payload: &[u8],
        now: Instant,
    ) -> Result<Client<T>, SendError> {
        if payload.len() > MAX_CONNECT_PAYLOAD {
            return Err(SendError::TooLarge);
        }
        Ok(Client {
            transport,
            server,
            config,
            nonce: unpredictable(),
            payload: payload.into(),
            phase: Phase::Requesting,
            heard: now,
            last_attempt: None,
            buffer: vec![0; MAX_PACKET_SIZE + 1].into_boxed_slice(),
        })
    }

    /// Where the client stands.
    #[must_use]
    pub fn state(&self) -> ClientState {
        match self.phase {
            Phase::Requesting | Phase::Responding { .. } => ClientState::Connecting,
            Phase::Connected(_) => ClientState::Connected,
            Phase::Disconnected(reason, _) => ClientState::Disconnected(reason),
        }
    }

    /// The transport, as for its bound address.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Takes in every packet waiting from the server, and gives up on a server silent for
    /// [`Config::timeout`].
    ///
    /// # Errors
    /// When the transport fails.
    pub fn update(&mut self, now: Instant) -> io::Result<()> {
        while let Some(received) = self.transport.receive(now, &mut self.buffer)? {
            let (size, at) = (received.length, received.at);
            if received.from == self.server && size <= MAX_PACKET_SIZE {
                let buffer = std::mem::take(&mut self.buffer);
                self.packet(at, Reader::new(&buffer[..size]), size);
                self.buffer = buffer;
            }
        }
        let silent = match &self.phase {
            Phase::Requesting | Phase::Responding { .. } => {
                now.saturating_duration_since(self.heard) > self.config.timeout
            }
            Phase::Connected(connection) => connection.timed_out(now),
            Phase::Disconnected(..) => false,
        };
        if silent {
            self.end(DisconnectReason::TimedOut);
        }
        Ok(())
    }

    fn packet(&mut self, now: Instant, mut reader: Reader<'_>, size: usize) {
        let kind = reader.u8();
        match (&mut self.phase, kind) {
            (Phase::Requesting, Some(CHALLENGE)) => {
                if let (Some(nonce), Some(cookie)) = (reader.u64(), reader.u64())
                    && nonce == self.nonce
                {
                    self.phase = Phase::Responding { cookie };
                    self.heard = now;
                    self.last_attempt = None;
                }
            }
            (Phase::Requesting | Phase::Responding { .. }, Some(ACCEPTED)) => {
                if let (Some(nonce), Some(session)) = (reader.u64(), reader.u64())
                    && nonce == self.nonce
                {
                    self.phase = Phase::Connected(Connection::new(self.config, session, now));
                }
            }
            (Phase::Requesting | Phase::Responding { .. }, Some(DENIED)) => {
                if let (Some(nonce), Some(code)) = (reader.u64(), reader.u8())
                    && nonce == self.nonce
                {
                    self.end(reason_from_code(code));
                }
            }
            (
                Phase::Connected(connection),
                Some(kind @ (PAYLOAD | PAYLOAD_UNACKED | DISCONNECT)),
            ) => {
                if reader.u64() != Some(connection.session()) {
                    return;
                }
                if kind == DISCONNECT {
                    self.end(DisconnectReason::ClosedByPeer);
                } else if connection.process(now, kind, reader, size).is_err() {
                    self.disconnect_for(now, DisconnectReason::Misbehaved);
                }
            }
            _ => {}
        }
    }

    /// The next message from the server, in the order received (reliable ones in the order sent).
    /// Messages that arrived before a disconnect are still read here.
    pub fn receive(&mut self) -> Option<Message> {
        match &mut self.phase {
            Phase::Connected(connection) | Phase::Disconnected(_, Some(connection)) => {
                connection.receive()
            }
            _ => None,
        }
    }

    /// Queues a message for the next flush.
    ///
    /// # Errors
    /// See [`SendError`].
    pub fn send(&mut self, channel: Channel, data: &[u8]) -> Result<(), SendError> {
        match &mut self.phase {
            Phase::Connected(connection) => connection.send(channel, data),
            _ => Err(SendError::NotConnected),
        }
    }

    /// Sends what's queued, with acks and keepalives; while connecting, the request or response
    /// when it's due.
    ///
    /// # Errors
    /// When the transport fails.
    pub fn flush(&mut self, now: Instant) -> io::Result<()> {
        let due = self
            .last_attempt
            .is_none_or(|last| now.saturating_duration_since(last) >= RETRY);
        match &mut self.phase {
            Phase::Requesting if due => {
                let mut request = Writer::new(CONNECT_REQUEST);
                request.u64(self.config.protocol).u64(self.nonce);
                request.bytes.resize(MAX_PACKET_SIZE, 0);
                self.last_attempt = Some(now);
                self.transport.send(now, self.server, &request.bytes)
            }
            Phase::Responding { cookie } if due => {
                let mut response = Writer::new(CHALLENGE_RESPONSE);
                response
                    .u64(self.config.protocol)
                    .u64(self.nonce)
                    .u64(*cookie)
                    .data(&self.payload);
                self.last_attempt = Some(now);
                self.transport.send(now, self.server, &response.bytes)
            }
            Phase::Connected(connection) => {
                for packet in connection.flush(now) {
                    self.transport.send(now, self.server, &packet)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Hangs up, telling the server so.
    pub fn disconnect(&mut self, now: Instant) {
        self.disconnect_for(now, DisconnectReason::ClosedLocally);
    }

    fn disconnect_for(&mut self, now: Instant, reason: DisconnectReason) {
        if let Phase::Connected(connection) = &self.phase {
            let mut goodbye = Writer::new(DISCONNECT);
            goodbye.u64(connection.session());
            for _ in 0..GOODBYES {
                // A goodbye that can't be sent is no worse than one lost.
                let _ = self.transport.send(now, self.server, &goodbye.bytes);
            }
        }
        self.end(reason);
    }

    /// Ends the connection, unless it already has.
    fn end(&mut self, reason: DisconnectReason) {
        self.phase = match std::mem::replace(&mut self.phase, Phase::Requesting) {
            Phase::Connected(connection) => Phase::Disconnected(reason, Some(connection)),
            Phase::Requesting | Phase::Responding { .. } => Phase::Disconnected(reason, None),
            ended @ Phase::Disconnected(..) => ended,
        };
    }

    /// How the connection is doing; `None` unless connected.
    #[must_use]
    pub fn stats(&self) -> Option<Stats> {
        match &self.phase {
            Phase::Connected(connection) => Some(connection.stats()),
            _ => None,
        }
    }
}
