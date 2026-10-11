//! Probes: a packet anyone may send a server, connected or not, that it answers at once, so a
//! server list can time each server it shows (its ping) and tell whether it runs the same
//! protocol, without connecting. The answer is smaller than the probe and the server keeps nothing
//! for it, so probes can't be turned against a third party.
//!
//! ```
//! use std::time::Instant;
//! use mulciber_net::{Config, MemoryAddress, MemoryNetwork, Server, ServerConfig, Transport, probe, probe_answer};
//!
//! let network = MemoryNetwork::new();
//! let config = Config::new(0x5348_4950_0001);
//! let mut server = Server::new(network.endpoint(MemoryAddress(1)), ServerConfig::new(config, 32));
//! let mut asker = network.endpoint(MemoryAddress(2));
//!
//! let now = Instant::now();
//! asker.send(now, MemoryAddress(1), &probe(config.protocol, 42)).unwrap();
//! server.update(now).unwrap();
//! let mut buffer = [0; 64];
//! let received = asker.receive(now, &mut buffer).unwrap().unwrap();
//! let answer = probe_answer(&buffer[..received.length]).unwrap();
//! assert_eq!((answer.token, answer.same_protocol), (42, true));
//! ```

use crate::packet::{PROBE, PROBE_ANSWER, Reader, Writer};

/// How many bytes a probe is.
pub const PROBE_SIZE: usize = 1 + 8 + 8;

/// A probe of a server, from a client running `protocol`; `token` comes back in the answer, to
/// tell which probe it answers (and so when it was sent).
#[must_use]
pub fn probe(protocol: u64, token: u64) -> [u8; PROBE_SIZE] {
    let mut probe = [0; PROBE_SIZE];
    probe[0] = PROBE;
    probe[1..9].copy_from_slice(&protocol.to_le_bytes());
    probe[9..].copy_from_slice(&token.to_le_bytes());
    probe
}

/// A server's answer to a probe, read by [`probe_answer`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProbeAnswer {
    /// The probe's token.
    pub token: u64,
    /// Whether the server runs the protocol the probe was sent for: whether the client could join.
    pub same_protocol: bool,
}

/// `packet` read as a server's answer to a probe; `None` if it's anything else.
#[must_use]
pub fn probe_answer(packet: &[u8]) -> Option<ProbeAnswer> {
    let mut reader = Reader::new(packet);
    if reader.u8()? != PROBE_ANSWER {
        return None;
    }
    let (token, same) = (reader.u64()?, reader.u8()?);
    (reader.is_empty() && same <= 1).then_some(ProbeAnswer {
        token,
        same_protocol: same == 1,
    })
}

/// A server's answer to a probe it read the rest of (after the kind) from `reader`, if it's one.
pub(crate) fn answer(reader: &mut Reader<'_>, protocol: u64) -> Option<Vec<u8>> {
    let (theirs, token) = (reader.u64()?, reader.u64()?);
    if !reader.is_empty() {
        return None;
    }
    let mut answer = Writer::new(PROBE_ANSWER);
    answer.u64(token).u8(u8::from(theirs == protocol));
    Some(answer.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_smaller_than_its_probe_and_reads_back() {
        let sent = probe(7, 0xfeed);
        let mut reader = Reader::new(&sent);
        assert_eq!(reader.u8(), Some(PROBE));
        let answer_bytes = answer(&mut reader, 7).unwrap();
        assert!(answer_bytes.len() < sent.len());
        assert_eq!(
            probe_answer(&answer_bytes),
            Some(ProbeAnswer {
                token: 0xfeed,
                same_protocol: true
            })
        );
        let mut reader = Reader::new(&sent[1..]);
        assert!(
            !probe_answer(&answer(&mut reader, 8).unwrap())
                .unwrap()
                .same_protocol
        );
    }

    #[test]
    fn anything_else_is_no_answer() {
        assert_eq!(probe_answer(&[]), None);
        assert_eq!(probe_answer(&probe(7, 1)), None);
        let mut long = answer(&mut Reader::new(&probe(7, 1)[1..]), 7).unwrap();
        long.push(0);
        assert_eq!(probe_answer(&long), None);
        // A cut or padded probe isn't answered.
        assert_eq!(
            answer(&mut Reader::new(&probe(7, 1)[1..PROBE_SIZE - 1]), 7),
            None
        );
        assert_eq!(
            answer(&mut Reader::new(&[&probe(7, 1)[1..], &[0]].concat()), 7),
            None
        );
    }
}
