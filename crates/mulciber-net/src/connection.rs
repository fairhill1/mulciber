//! One end of an established connection: packet acknowledgement, round-trip time and loss, and the
//! reliable and unreliable channels on top. Knows nothing of sockets or handshakes; the server and
//! client feed it the payloads they receive and send the packets it builds.

use std::collections::VecDeque;
use std::mem;
use std::time::{Duration, Instant};

use crate::packet::{
    FRAGMENT_OVERHEAD, PAYLOAD, PAYLOAD_HEADER, PAYLOAD_UNACKED, Part, RELIABLE_OVERHEAD, Reader,
    UNRELIABLE_OVERHEAD, Writer,
};
use crate::sequence::{SequenceBuffer, newer};
use crate::{Channel, Config, MAX_PACKET_SIZE, MAX_UNRELIABLE_SIZE, Message, SendError};

/// The data in one fragment of a split message.
pub(crate) const FRAGMENT_SIZE: usize = 1024;
/// How many reliable fragments may be in flight past the oldest unacknowledged one. Divides 65536,
/// so ids within a window map to distinct slots.
const WINDOW: usize = 1024;
/// How many packets each way are remembered, for acks and for spotting duplicates.
const PACKETS: usize = 1024;
/// The unit of an ack's delay: 100 microseconds, up to 6.5 seconds.
const ACK_DELAY_MICROS: u64 = 100;
/// How many earlier packets an ack covers besides the one it names.
const ACK_BITS: u16 = 32;
/// The longest a connection goes without sending, so the other end knows it's still there.
const KEEPALIVE: Duration = Duration::from_secs(1);
/// How many split unreliable messages are put back together at once, and for how long.
const REASSEMBLIES: usize = 8;
const REASSEMBLY_TIME: Duration = Duration::from_secs(1);
/// How fast the round-trip time and loss follow new samples.
const RTT_SMOOTHING: f32 = 0.1;
const LOSS_SMOOTHING: f32 = 0.05;

/// How a connection is doing.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Stats {
    /// The round-trip time, smoothed: from sending a packet to hearing it arrived, less the time
    /// the other end held it before replying (until its next flush). Time on the network alone.
    pub rtt: Duration,
    /// The fraction of packets lost lately, 0 to 1.
    pub packet_loss: f32,
    /// Bytes sent and received a second, over the last whole second.
    pub sent_bytes_per_second: f32,
    /// See `sent_bytes_per_second`.
    pub received_bytes_per_second: f32,
}

/// The other end broke the protocol.
#[derive(Debug)]
pub(crate) struct Misbehaved;

struct SentPacket {
    sent: Instant,
    acked: bool,
    /// The reliable fragments it carried.
    reliable: Vec<u16>,
}

/// A reliable fragment waiting to be acknowledged.
struct Pending {
    data: Box<[u8]>,
    last: bool,
    sent: Option<Instant>,
    acked: bool,
}

/// A split unreliable message coming together.
struct Reassembly {
    id: u16,
    started: Instant,
    parts: Vec<Option<Box<[u8]>>>,
    missing: usize,
}

struct Traffic {
    since: Instant,
    sent: usize,
    received: usize,
    sent_rate: f32,
    received_rate: f32,
}

pub(crate) struct Connection {
    config: Config,
    session: u64,
    // Packets.
    next_sequence: u16,
    sent: SequenceBuffer<SentPacket>,
    /// When each packet arrived.
    received: SequenceBuffer<Instant>,
    latest_received: Option<u16>,
    /// Packets with messages received since the last flush, all of which that flush acknowledges.
    unacked: Vec<u16>,
    /// Sent packets with messages whose fate is not yet counted toward the loss.
    outstanding: VecDeque<(u16, Instant)>,
    // Reliable fragments out: `queue[i]` has id `oldest + i`.
    next_id: u16,
    oldest: u16,
    queue: VecDeque<Pending>,
    backlog: usize,
    // Reliable fragments in: `window[id % WINDOW]` for ids from `expected` on.
    expected: u16,
    window: Vec<Option<(Box<[u8]>, bool)>>,
    assembling: Vec<u8>,
    // Unreliable messages.
    unreliable: Vec<Box<[u8]>>,
    next_split: u16,
    reassemblies: Vec<Reassembly>,
    incoming: VecDeque<Message>,
    // Timing.
    last_received: Instant,
    last_sent: Instant,
    rtt: Option<f32>,
    loss: f32,
    traffic: Traffic,
}

impl Connection {
    pub(crate) fn new(config: Config, session: u64, now: Instant) -> Connection {
        Connection {
            config,
            session,
            next_sequence: 0,
            sent: SequenceBuffer::new(PACKETS),
            received: SequenceBuffer::new(PACKETS),
            latest_received: None,
            unacked: Vec::new(),
            outstanding: VecDeque::new(),
            next_id: 0,
            oldest: 0,
            queue: VecDeque::new(),
            backlog: 0,
            expected: 0,
            window: (0..WINDOW).map(|_| None).collect(),
            assembling: Vec::new(),
            unreliable: Vec::new(),
            next_split: 0,
            reassemblies: Vec::new(),
            incoming: VecDeque::new(),
            last_received: now,
            last_sent: now,
            rtt: None,
            loss: 0.0,
            traffic: Traffic {
                since: now,
                sent: 0,
                received: 0,
                sent_rate: 0.0,
                received_rate: 0.0,
            },
        }
    }

    pub(crate) fn session(&self) -> u64 {
        self.session
    }

    pub(crate) fn send(&mut self, channel: Channel, data: &[u8]) -> Result<(), SendError> {
        match channel {
            Channel::Reliable => {
                if data.len() > self.config.max_message_size {
                    return Err(SendError::TooLarge);
                }
                if self.backlog + data.len() > self.config.max_backlog {
                    return Err(SendError::Backlogged);
                }
                let count = data.len().div_ceil(FRAGMENT_SIZE).max(1);
                for (i, chunk) in data
                    .chunks(FRAGMENT_SIZE)
                    .chain(data.is_empty().then_some(&[][..]))
                    .enumerate()
                {
                    self.queue.push_back(Pending {
                        data: chunk.into(),
                        last: i + 1 == count,
                        sent: None,
                        acked: false,
                    });
                    self.next_id = self.next_id.wrapping_add(1);
                }
                self.backlog += data.len();
            }
            Channel::Unreliable => {
                if data.len() > MAX_UNRELIABLE_SIZE {
                    return Err(SendError::TooLarge);
                }
                self.unreliable.push(data.into());
            }
        }
        Ok(())
    }

    pub(crate) fn receive(&mut self) -> Option<Message> {
        self.incoming.pop_front()
    }

    pub(crate) fn timed_out(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_received) > self.config.timeout
    }

    pub(crate) fn stats(&self) -> Stats {
        Stats {
            rtt: Duration::from_secs_f32(self.rtt.unwrap_or(0.0)),
            packet_loss: self.loss,
            sent_bytes_per_second: self.traffic.sent_rate,
            received_bytes_per_second: self.traffic.received_rate,
        }
    }

    /// Takes in a payload packet whose kind and session have been read. Malformed, duplicate and
    /// stale packets are dropped whole.
    pub(crate) fn process(
        &mut self,
        now: Instant,
        kind: u8,
        mut reader: Reader<'_>,
        size: usize,
    ) -> Result<(), Misbehaved> {
        let Some(sequence) = reader.u16() else {
            return Ok(());
        };
        let ack = if kind == PAYLOAD {
            let (Some(ack), Some(bits), Some(delay)) = (reader.u16(), reader.u32(), reader.u16())
            else {
                return Ok(());
            };
            Some((
                ack,
                bits,
                Duration::from_micros(u64::from(delay) * ACK_DELAY_MICROS),
            ))
        } else {
            None
        };
        let Some(parts) = reader.parts() else {
            return Ok(());
        };
        match self.latest_received {
            Some(latest) if !newer(sequence, latest) => {
                if usize::from(latest.wrapping_sub(sequence)) >= PACKETS
                    || self.received.get(sequence).is_some()
                {
                    return Ok(());
                }
            }
            Some(latest) => {
                self.received.clear_after(latest, sequence);
                self.latest_received = Some(sequence);
            }
            None => self.latest_received = Some(sequence),
        }
        self.received.insert(sequence, now);
        self.last_received = now;
        self.traffic.received += size;
        if !parts.is_empty() {
            self.unacked.push(sequence);
        }
        if let Some((ack, bits, delay)) = ack {
            self.acknowledge(now, ack, bits, delay);
        }
        for part in parts {
            match part {
                Part::Reliable { id, last, data } => self.reliable_in(id, last, data)?,
                Part::Unreliable(data) => self.incoming.push_back(Message {
                    channel: Channel::Unreliable,
                    data: data.into(),
                }),
                Part::Fragment {
                    id,
                    index,
                    count,
                    data,
                } => self.fragment_in(now, id, index, count, data),
            }
        }
        Ok(())
    }

    /// The other end has `ack` and those of the 32 before it whose bits are set, and held its ack
    /// of `ack` for `delay` before sending it.
    fn acknowledge(&mut self, now: Instant, ack: u16, bits: u32, delay: Duration) {
        for back in 0..=ACK_BITS {
            if back > 0 && bits & (1 << (back - 1)) == 0 {
                continue;
            }
            let Some(packet) = self.sent.get_mut(ack.wrapping_sub(back)) else {
                continue;
            };
            if packet.acked {
                continue;
            }
            packet.acked = true;
            let (sent, ids) = (packet.sent, mem::take(&mut packet.reliable));
            // Only the packet the ack names says how long the other end held it; the time on
            // the network is the rest.
            if back == 0 {
                let sample = now
                    .saturating_duration_since(sent)
                    .saturating_sub(delay)
                    .as_secs_f32();
                self.rtt = Some(
                    self.rtt
                        .map_or(sample, |rtt| rtt + (sample - rtt) * RTT_SMOOTHING),
                );
            }
            for id in ids {
                if let Some(pending) = self
                    .queue
                    .get_mut(usize::from(id.wrapping_sub(self.oldest)))
                {
                    pending.acked = true;
                }
            }
        }
        while self.queue.front().is_some_and(|pending| pending.acked) {
            let pending = self.queue.pop_front().expect("checked");
            self.backlog -= pending.data.len();
            self.oldest = self.oldest.wrapping_add(1);
        }
    }

    fn reliable_in(&mut self, id: u16, last: bool, data: &[u8]) -> Result<(), Misbehaved> {
        // Behind the window is a fragment already delivered; an honest sender never gets ahead of it.
        if usize::from(id.wrapping_sub(self.expected)) >= WINDOW {
            return Ok(());
        }
        let slot = &mut self.window[usize::from(id) % WINDOW];
        if slot.is_none() {
            *slot = Some((data.into(), last));
        }
        while let Some((data, last)) = self.window[usize::from(self.expected) % WINDOW].take() {
            if self.assembling.len() + data.len() > self.config.max_message_size {
                return Err(Misbehaved);
            }
            self.assembling.extend_from_slice(&data);
            self.expected = self.expected.wrapping_add(1);
            if last {
                let data = mem::take(&mut self.assembling).into_boxed_slice();
                self.incoming.push_back(Message {
                    channel: Channel::Reliable,
                    data,
                });
            }
        }
        Ok(())
    }

    fn fragment_in(&mut self, now: Instant, id: u16, index: u8, count: u8, data: &[u8]) {
        if data.len() > FRAGMENT_SIZE {
            return;
        }
        let at = if let Some(at) = self.reassemblies.iter().position(|r| r.id == id) {
            at
        } else {
            if self.reassemblies.len() == REASSEMBLIES {
                let oldest = (0..REASSEMBLIES)
                    .min_by_key(|&i| self.reassemblies[i].started)
                    .expect("full");
                self.reassemblies.swap_remove(oldest);
            }
            let count = usize::from(count);
            self.reassemblies.push(Reassembly {
                id,
                started: now,
                parts: vec![None; count],
                missing: count,
            });
            self.reassemblies.len() - 1
        };
        let reassembly = &mut self.reassemblies[at];
        if reassembly.parts.len() != usize::from(count) {
            return;
        }
        let part = &mut reassembly.parts[usize::from(index)];
        if part.is_some() {
            return;
        }
        *part = Some(data.into());
        reassembly.missing -= 1;
        if reassembly.missing == 0 {
            let reassembly = self.reassemblies.swap_remove(at);
            let data = reassembly.parts.into_iter().flatten().flatten().collect();
            self.incoming.push_back(Message {
                channel: Channel::Unreliable,
                data,
            });
        }
    }

    /// How long a reliable fragment waits for its ack before going again.
    fn resend_after(&self) -> Duration {
        Duration::from_secs_f32((self.rtt.unwrap_or(0.1) * 1.5 + 0.03).min(1.0))
    }

    /// Counts the packets that have had time to be acknowledged toward the loss.
    fn settle_loss(&mut self, now: Instant) {
        let patience = Duration::from_secs_f32((self.rtt.unwrap_or(0.1) * 2.0 + 0.1).min(2.0));
        while let Some(&(sequence, sent)) = self.outstanding.front() {
            if now.saturating_duration_since(sent) < patience {
                break;
            }
            self.outstanding.pop_front();
            if let Some(packet) = self.sent.get(sequence) {
                let lost = if packet.acked { 0.0 } else { 1.0 };
                self.loss += (lost - self.loss) * LOSS_SMOOTHING;
            }
        }
    }

    fn count_traffic(&mut self, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.traffic.since)
            .as_secs_f32();
        if elapsed >= 1.0 {
            #[allow(clippy::cast_precision_loss, reason = "byte counts over a second")]
            let (sent, received) = (self.traffic.sent as f32, self.traffic.received as f32);
            self.traffic.sent_rate = sent / elapsed;
            self.traffic.received_rate = received / elapsed;
            self.traffic = Traffic {
                since: now,
                sent: 0,
                received: 0,
                ..self.traffic
            };
        }
    }

    /// The bits for the 32 packets received before `ack`.
    fn ack_bits(&self, ack: u16) -> u32 {
        (1..=ACK_BITS)
            .filter(|&back| self.received.get(ack.wrapping_sub(back)).is_some())
            .fold(0, |bits, back| bits | 1 << (back - 1))
    }

    /// The packets to send now: the reliable fragments due, every unreliable message queued, and
    /// acks for everything received since the last flush.
    pub(crate) fn flush(&mut self, now: Instant) -> Vec<Vec<u8>> {
        self.settle_loss(now);
        self.count_traffic(now);
        self.reassemblies
            .retain(|r| now.saturating_duration_since(r.started) < REASSEMBLY_TIME);

        let mut bodies = Bodies::default();
        let resend_after = self.resend_after();
        let mut budget = self.config.reliable_bytes_per_flush;
        for (offset, pending) in self.queue.iter_mut().enumerate().take(WINDOW) {
            if pending.acked
                || pending
                    .sent
                    .is_some_and(|sent| now.saturating_duration_since(sent) < resend_after)
            {
                continue;
            }
            if pending.data.len() > budget {
                break;
            }
            budget -= pending.data.len();
            #[allow(clippy::cast_possible_truncation, reason = "offset is below WINDOW")]
            let id = self.oldest.wrapping_add(offset as u16);
            bodies
                .room(RELIABLE_OVERHEAD + pending.data.len())
                .reliable(id, pending.last, &pending.data);
            bodies.ids.last_mut().expect("room made a body").push(id);
            pending.sent = Some(now);
        }
        for message in mem::take(&mut self.unreliable) {
            if PAYLOAD_HEADER + UNRELIABLE_OVERHEAD + message.len() <= MAX_PACKET_SIZE {
                bodies
                    .room(UNRELIABLE_OVERHEAD + message.len())
                    .unreliable(&message);
                continue;
            }
            let count = u8::try_from(message.len().div_ceil(FRAGMENT_SIZE))
                .expect("checked against MAX_UNRELIABLE_SIZE");
            for (index, chunk) in (0..count).zip(message.chunks(FRAGMENT_SIZE)) {
                bodies.room(FRAGMENT_OVERHEAD + chunk.len()).fragment(
                    self.next_split,
                    index,
                    count,
                    chunk,
                );
            }
            self.next_split = self.next_split.wrapping_add(1);
        }

        let mut packets = Vec::new();
        let latest = self.latest_received;
        let owe_ack = !self.unacked.is_empty();
        for (body, ids) in bodies.bodies.into_iter().zip(bodies.ids) {
            packets.push(self.packet(now, latest, &body.bytes, ids, true));
        }
        if packets.is_empty()
            && (owe_ack || now.saturating_duration_since(self.last_sent) >= KEEPALIVE)
        {
            packets.push(self.packet(now, latest, &[], Vec::new(), false));
        }
        // A packet's ack reaches back 32 packets; anything older received since the last flush gets a
        // bare ack of its own, so a burst from the other end is acknowledged in full.
        let mut ages: Vec<u16> = mem::take(&mut self.unacked)
            .into_iter()
            .filter_map(|s| latest.map(|latest| latest.wrapping_sub(s)))
            .collect();
        ages.sort_unstable();
        let mut covered = 0;
        for age in ages {
            if age > covered + ACK_BITS && usize::from(age) < PACKETS {
                covered = age;
                let base = latest.map(|latest| latest.wrapping_sub(age));
                packets.push(self.packet(now, base, &[], Vec::new(), false));
            }
        }
        packets
    }

    /// One payload packet, recorded as sent. `tracked` packets carry messages: only those count
    /// toward the loss, since a bare ack is only acknowledged when the other end has something
    /// to say.
    fn packet(
        &mut self,
        now: Instant,
        ack: Option<u16>,
        body: &[u8],
        reliable: Vec<u16>,
        tracked: bool,
    ) -> Vec<u8> {
        let sequence = self.next_sequence;
        self.next_sequence = sequence.wrapping_add(1);
        let mut writer = Writer::new(if ack.is_some() {
            PAYLOAD
        } else {
            PAYLOAD_UNACKED
        });
        writer.u64(self.session).u16(sequence);
        if let Some(ack) = ack {
            let held = self
                .received
                .get(ack)
                .map_or(Duration::ZERO, |&at| now.saturating_duration_since(at));
            let delay =
                u16::try_from(held.as_micros() / u128::from(ACK_DELAY_MICROS)).unwrap_or(u16::MAX);
            writer.u16(ack).u32(self.ack_bits(ack)).u16(delay);
        }
        writer.bytes.extend_from_slice(body);
        self.sent.insert(
            sequence,
            SentPacket {
                sent: now,
                acked: false,
                reliable,
            },
        );
        if tracked {
            self.outstanding.push_back((sequence, now));
        }
        self.last_sent = now;
        self.traffic.sent += writer.len();
        writer.bytes
    }
}

/// Packet bodies being filled with messages, and the reliable fragments in each.
#[derive(Default)]
struct Bodies {
    bodies: Vec<Writer>,
    ids: Vec<Vec<u16>>,
}

impl Bodies {
    /// A body with room for `size` more bytes: the last, or a new one.
    fn room(&mut self, size: usize) -> &mut Writer {
        if self
            .bodies
            .last()
            .is_none_or(|body| PAYLOAD_HEADER + body.len() + size > MAX_PACKET_SIZE)
        {
            self.bodies.push(Writer::default());
            self.ids.push(Vec::new());
        }
        self.bodies.last_mut().expect("just made sure")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: Config = Config::new(1);

    /// Delivers `packets` to `to`, dropping those `lose` picks.
    fn deliver(
        to: &mut Connection,
        now: Instant,
        packets: Vec<Vec<u8>>,
        mut lose: impl FnMut(usize) -> bool,
    ) {
        for (i, packet) in packets.into_iter().enumerate() {
            if lose(i) {
                continue;
            }
            let mut reader = Reader::new(&packet);
            let kind = reader.u8().unwrap();
            assert_eq!(reader.u64(), Some(to.session));
            to.process(now, kind, reader, packet.len()).unwrap();
        }
    }

    #[test]
    fn a_lost_reliable_fragment_goes_again_until_acknowledged() {
        let start = Instant::now();
        let (mut a, mut b) = (
            Connection::new(CONFIG, 9, start),
            Connection::new(CONFIG, 9, start),
        );
        a.send(Channel::Reliable, b"first").unwrap();
        a.send(Channel::Reliable, b"second").unwrap();
        deliver(&mut b, start, a.flush(start), |_| true);
        // Not yet due again.
        assert_eq!(a.flush(start + Duration::from_millis(10)).len(), 0);
        let later = start + Duration::from_millis(200);
        deliver(&mut b, later, a.flush(later), |_| false);
        assert_eq!(&*b.receive().unwrap().data, b"first");
        assert_eq!(&*b.receive().unwrap().data, b"second");
        // b acknowledges; a stops sending and its backlog empties.
        deliver(&mut a, later, b.flush(later), |_| false);
        assert_eq!(a.backlog, 0);
        assert!(
            a.flush(later + Duration::from_secs(5))
                .iter()
                .all(|p| p.len() == PAYLOAD_HEADER)
        );
    }

    #[test]
    fn a_burst_is_acknowledged_in_full() {
        let start = Instant::now();
        let (mut a, mut b) = (
            Connection::new(CONFIG, 9, start),
            Connection::new(CONFIG, 9, start),
        );
        for i in 0..100u8 {
            a.send(Channel::Reliable, &[i; 1000]).unwrap();
        }
        let config = Config {
            reliable_bytes_per_flush: 1 << 20,
            ..CONFIG
        };
        a.config = config;
        let burst = a.flush(start);
        assert_eq!(burst.len(), 100);
        deliver(&mut b, start, burst, |_| false);
        let acks = b.flush(start);
        // One packet acknowledges 33; the rest take bare acks.
        assert_eq!(acks.len(), 4);
        deliver(&mut a, start, acks, |_| false);
        assert!(a.queue.is_empty());
        assert_eq!(b.incoming.len(), 100);
    }

    #[test]
    fn a_split_unreliable_message_arrives_whole_or_not_at_all() {
        let start = Instant::now();
        let (mut a, mut b) = (
            Connection::new(CONFIG, 9, start),
            Connection::new(CONFIG, 9, start),
        );
        let message: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        a.send(Channel::Unreliable, &message).unwrap();
        a.send(Channel::Unreliable, &message).unwrap();
        let packets = a.flush(start);
        assert_eq!(packets.len(), 10);
        // The first copy's third fragment is lost.
        deliver(&mut b, start, packets, |i| i == 2);
        let got = b.receive().unwrap();
        assert_eq!(&*got.data, &message[..]);
        assert!(b.receive().is_none());
    }

    #[test]
    fn duplicate_packets_deliver_once() {
        let start = Instant::now();
        let (mut a, mut b) = (
            Connection::new(CONFIG, 9, start),
            Connection::new(CONFIG, 9, start),
        );
        a.send(Channel::Unreliable, b"step").unwrap();
        let packets = a.flush(start);
        deliver(&mut b, start, packets.clone(), |_| false);
        deliver(&mut b, start, packets, |_| false);
        assert!(b.receive().is_some());
        assert!(b.receive().is_none());
    }

    #[test]
    fn an_oversized_reliable_message_is_misbehaviour() {
        let start = Instant::now();
        let big = Config {
            max_message_size: 10_000,
            ..CONFIG
        };
        let small = Config {
            max_message_size: 2000,
            ..CONFIG
        };
        let (mut a, mut b) = (
            Connection::new(big, 9, start),
            Connection::new(small, 9, start),
        );
        a.send(Channel::Reliable, &[0; 5000]).unwrap();
        let mut misbehaved = false;
        for packet in a.flush(start) {
            let mut reader = Reader::new(&packet);
            let kind = reader.u8().unwrap();
            reader.u64();
            misbehaved |= b.process(start, kind, reader, packet.len()).is_err();
        }
        assert!(misbehaved);
    }

    #[test]
    fn round_trip_time_follows_the_acks() {
        let start = Instant::now();
        let (mut a, mut b) = (
            Connection::new(CONFIG, 9, start),
            Connection::new(CONFIG, 9, start),
        );
        let mut now = start;
        for _ in 0..200 {
            a.send(Channel::Unreliable, b"tick").unwrap();
            let out = a.flush(now);
            deliver(&mut b, now + Duration::from_millis(40), out, |_| false);
            // b holds its reply 12 ms for its next flush; that isn't the network's time.
            let back = b.flush(now + Duration::from_millis(52));
            deliver(&mut a, now + Duration::from_millis(92), back, |_| false);
            now += Duration::from_millis(16);
        }
        let rtt = a.stats().rtt.as_secs_f32();
        assert!((rtt - 0.08).abs() < 0.001, "{rtt}");
        assert!(a.stats().packet_loss < 0.01);
    }
}
