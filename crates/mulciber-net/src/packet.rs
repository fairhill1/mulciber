//! The wire format. Every packet starts with a kind byte; numbers are little-endian.
//!
//! Before a connection:
//! - `ConnectRequest`: protocol u64, nonce u64, zeros up to [`MAX_PACKET_SIZE`].
//! - `Challenge`: nonce u64, cookie u64.
//! - `ChallengeResponse`: protocol u64, nonce u64, cookie u64, payload length u16, payload.
//! - `Accepted`: nonce u64, session u64.
//! - `Denied`: nonce u64, reason u8.
//!
//! On one, each packet carries the session u64 after its kind:
//! - `Payload`: sequence u16, ack u16, ack bits u32, ack delay u16 (how long the ack was held, in
//!   units of 100 µs), then messages to the end.
//! - `PayloadUnacked`: as `Payload` without the ack fields, before anything has been received.
//! - `Disconnect`: nothing more.
//!
//! A message is a tag and its fields:
//! - reliable fragment (tag 0, or 1 when it ends its message): id u16, length u16, bytes;
//! - unreliable message (tag 2): length u16, bytes;
//! - unreliable fragment (tag 3): message id u16, index u8, count u8, length u16, bytes.

use crate::{DisconnectReason, MAX_PACKET_SIZE};

pub(crate) const CONNECT_REQUEST: u8 = 1;
pub(crate) const CHALLENGE: u8 = 2;
pub(crate) const CHALLENGE_RESPONSE: u8 = 3;
pub(crate) const ACCEPTED: u8 = 4;
pub(crate) const DENIED: u8 = 5;
pub(crate) const PAYLOAD: u8 = 6;
pub(crate) const PAYLOAD_UNACKED: u8 = 7;
pub(crate) const DISCONNECT: u8 = 8;

const RELIABLE: u8 = 0;
const RELIABLE_LAST: u8 = 1;
const UNRELIABLE: u8 = 2;
const UNRELIABLE_FRAGMENT: u8 = 3;

/// Bytes ahead of a payload's messages: kind, session, sequence, ack, ack bits and ack delay.
pub(crate) const PAYLOAD_HEADER: usize = 1 + 8 + 2 + 2 + 4 + 2;
/// Each message's bytes besides its data.
pub(crate) const RELIABLE_OVERHEAD: usize = 1 + 2 + 2;
pub(crate) const UNRELIABLE_OVERHEAD: usize = 1 + 2;
pub(crate) const FRAGMENT_OVERHEAD: usize = 1 + 2 + 1 + 1 + 2;

/// Builds a packet.
#[derive(Default)]
pub(crate) struct Writer {
    pub(crate) bytes: Vec<u8>,
}

impl Writer {
    pub(crate) fn new(kind: u8) -> Writer {
        let mut bytes = Vec::with_capacity(MAX_PACKET_SIZE);
        bytes.push(kind);
        Writer { bytes }
    }

    pub(crate) fn u8(&mut self, value: u8) -> &mut Writer {
        self.bytes.push(value);
        self
    }

    pub(crate) fn u16(&mut self, value: u16) -> &mut Writer {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub(crate) fn u32(&mut self, value: u32) -> &mut Writer {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub(crate) fn u64(&mut self, value: u64) -> &mut Writer {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    /// A length-prefixed run of bytes, at most 65535.
    pub(crate) fn data(&mut self, data: &[u8]) -> &mut Writer {
        let length = u16::try_from(data.len()).expect("message parts are cut to fit a packet");
        self.u16(length);
        self.bytes.extend_from_slice(data);
        self
    }

    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    pub(crate) fn reliable(&mut self, id: u16, last: bool, data: &[u8]) {
        self.u8(if last { RELIABLE_LAST } else { RELIABLE })
            .u16(id)
            .data(data);
    }

    pub(crate) fn unreliable(&mut self, data: &[u8]) {
        self.u8(UNRELIABLE).data(data);
    }

    pub(crate) fn fragment(&mut self, id: u16, index: u8, count: u8, data: &[u8]) {
        self.u8(UNRELIABLE_FRAGMENT)
            .u16(id)
            .u8(index)
            .u8(count)
            .data(data);
    }
}

/// Reads a packet; every read is `None` past the end.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.bytes.len() < n {
            return None;
        }
        let (taken, rest) = self.bytes.split_at(n);
        self.bytes = rest;
        Some(taken)
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_le_bytes(b.try_into().expect("eight bytes")))
    }

    pub(crate) fn data(&mut self) -> Option<&'a [u8]> {
        let length = self.u16()?;
        self.take(usize::from(length))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The messages in the rest of a payload, or `None` if any is malformed.
    pub(crate) fn parts(mut self) -> Option<Vec<Part<'a>>> {
        let mut parts = Vec::new();
        while !self.is_empty() {
            parts.push(match self.u8()? {
                tag @ (RELIABLE | RELIABLE_LAST) => Part::Reliable {
                    id: self.u16()?,
                    last: tag == RELIABLE_LAST,
                    data: self.data()?,
                },
                UNRELIABLE => Part::Unreliable(self.data()?),
                UNRELIABLE_FRAGMENT => {
                    let (id, index, count) = (self.u16()?, self.u8()?, self.u8()?);
                    if index >= count {
                        return None;
                    }
                    Part::Fragment {
                        id,
                        index,
                        count,
                        data: self.data()?,
                    }
                }
                _ => return None,
            });
        }
        Some(parts)
    }
}

/// One message read from a payload.
pub(crate) enum Part<'a> {
    Reliable {
        id: u16,
        last: bool,
        data: &'a [u8],
    },
    Unreliable(&'a [u8]),
    Fragment {
        id: u16,
        index: u8,
        count: u8,
        data: &'a [u8],
    },
}

pub(crate) fn reason_code(reason: DisconnectReason) -> u8 {
    match reason {
        DisconnectReason::ServerFull => 1,
        DisconnectReason::WrongProtocol => 2,
        _ => 0,
    }
}

pub(crate) fn reason_from_code(code: u8) -> DisconnectReason {
    match code {
        1 => DisconnectReason::ServerFull,
        2 => DisconnectReason::WrongProtocol,
        _ => DisconnectReason::ClosedByPeer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_read_back_as_written() {
        let mut writer = Writer::new(PAYLOAD);
        writer.reliable(7, true, b"door");
        writer.unreliable(b"");
        writer.fragment(65535, 2, 3, b"pos");
        let mut reader = Reader::new(&writer.bytes);
        assert_eq!(reader.u8(), Some(PAYLOAD));
        let parts = reader.parts().unwrap();
        assert!(matches!(
            parts[0],
            Part::Reliable {
                id: 7,
                last: true,
                data: b"door"
            }
        ));
        assert!(matches!(parts[1], Part::Unreliable(b"")));
        assert!(matches!(
            parts[2],
            Part::Fragment {
                id: 65535,
                index: 2,
                count: 3,
                data: b"pos"
            }
        ));
    }

    #[test]
    fn a_cut_or_nonsense_payload_reads_as_nothing() {
        let mut writer = Writer::new(PAYLOAD);
        writer.reliable(1, false, b"abcdef");
        let bytes = &writer.bytes[1..writer.bytes.len() - 1];
        assert!(Reader::new(bytes).parts().is_none());
        assert!(Reader::new(&[9]).parts().is_none());
        // A fragment past its message's end.
        let mut writer = Writer::new(PAYLOAD);
        writer.fragment(0, 3, 3, b"x");
        assert!(Reader::new(&writer.bytes[1..]).parts().is_none());
    }
}
