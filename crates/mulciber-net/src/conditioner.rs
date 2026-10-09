//! Bad network conditions on demand: latency, jitter, loss and duplication, applied to one
//! endpoint's packets both ways.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io;
use std::time::{Duration, Instant};

use crate::MAX_PACKET_SIZE;
use crate::random::Rng;
use crate::transport::{Received, Transport};

/// What a [`Conditioned`] transport does to packets. Each applies to outgoing and incoming packets
/// alike, so wrapping only a client with `latency` 50 ms gives it a ping of 100 ms.
#[derive(Clone, Copy, Debug, Default)]
pub struct Conditions {
    /// How long each packet is held, each way.
    pub latency: Duration,
    /// Up to how much longer, at random. More than the time between packets reorders them.
    pub jitter: Duration,
    /// The chance a packet is lost, 0 to 1.
    pub loss: f64,
    /// The chance a packet arrives twice, 0 to 1.
    pub duplicate: f64,
}

struct Held<A> {
    due: Instant,
    order: u64,
    address: A,
    packet: Box<[u8]>,
}

impl<A> PartialEq for Held<A> {
    fn eq(&self, other: &Self) -> bool {
        (self.due, self.order) == (other.due, other.order)
    }
}

impl<A> Eq for Held<A> {}

impl<A> PartialOrd for Held<A> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<A> Ord for Held<A> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.due, self.order).cmp(&(other.due, other.order))
    }
}

/// A transport under [`Conditions`]: packets it sends and receives are delayed, lost and doubled as
/// they say. For testing how a game plays across an ocean without leaving the room.
pub struct Conditioned<T: Transport> {
    inner: T,
    conditions: Conditions,
    rng: Rng,
    order: u64,
    outgoing: BinaryHeap<Reverse<Held<T::Address>>>,
    incoming: BinaryHeap<Reverse<Held<T::Address>>>,
}

impl<T: Transport> Conditioned<T> {
    /// Wraps `inner`; `seed` makes the losses and delays repeatable.
    pub fn new(inner: T, conditions: Conditions, seed: u64) -> Conditioned<T> {
        Conditioned {
            inner,
            conditions,
            rng: Rng::new(seed),
            order: 0,
            outgoing: BinaryHeap::new(),
            incoming: BinaryHeap::new(),
        }
    }

    /// Changes the conditions from now on; packets already held keep their times.
    pub fn set_conditions(&mut self, conditions: Conditions) {
        self.conditions = conditions;
    }

    /// The transport inside.
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Queues `packet` under the conditions: dropped, or held once or twice.
    fn hold(&mut self, now: Instant, address: T::Address, packet: &[u8], outgoing: bool) {
        if self.rng.unit() < self.conditions.loss {
            return;
        }
        let copies = if self.rng.unit() < self.conditions.duplicate {
            2
        } else {
            1
        };
        for _ in 0..copies {
            let due =
                now + self.conditions.latency + self.conditions.jitter.mul_f64(self.rng.unit());
            self.order += 1;
            let held = Reverse(Held {
                due,
                order: self.order,
                address,
                packet: packet.into(),
            });
            if outgoing {
                self.outgoing.push(held);
            } else {
                self.incoming.push(held);
            }
        }
    }

    /// Sends the outgoing packets now due.
    fn release(&mut self, now: Instant) -> io::Result<()> {
        while self.outgoing.peek().is_some_and(|held| held.0.due <= now) {
            let Some(Reverse(held)) = self.outgoing.pop() else {
                break;
            };
            self.inner.send(now, held.address, &held.packet)?;
        }
        Ok(())
    }
}

impl<T: Transport> Transport for Conditioned<T> {
    type Address = T::Address;

    fn send(&mut self, now: Instant, to: T::Address, packet: &[u8]) -> io::Result<()> {
        self.hold(now, to, packet, true);
        self.release(now)
    }

    fn receive(
        &mut self,
        now: Instant,
        buffer: &mut [u8],
    ) -> io::Result<Option<Received<T::Address>>> {
        self.release(now)?;
        let mut arriving = [0; MAX_PACKET_SIZE + 1];
        while let Some(received) = self.inner.receive(now, &mut arriving)? {
            // Held from when it really arrived.
            self.hold(
                received.at,
                received.from,
                &arriving[..received.length],
                false,
            );
        }
        if self.incoming.peek().is_none_or(|held| held.0.due > now) {
            return Ok(None);
        }
        let Some(Reverse(held)) = self.incoming.pop() else {
            return Ok(None);
        };
        let length = held.packet.len().min(buffer.len());
        buffer[..length].copy_from_slice(&held.packet[..length]);
        Ok(Some(Received {
            length,
            from: held.address,
            at: held.due,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryAddress, MemoryNetwork};

    #[test]
    fn packets_wait_out_the_latency_both_ways() {
        let network = MemoryNetwork::new();
        let conditions = Conditions {
            latency: Duration::from_millis(50),
            ..Conditions::default()
        };
        let mut a = Conditioned::new(network.endpoint(MemoryAddress(1)), conditions, 1);
        let mut b = network.endpoint(MemoryAddress(2));
        let start = Instant::now();
        let mut buffer = [0; 16];
        a.send(start, MemoryAddress(2), b"ping").unwrap();
        assert_eq!(
            b.receive(start, &mut buffer)
                .unwrap()
                .map(|r| (r.length, r.from)),
            None
        );
        // Released by the next call on `a` once due.
        assert_eq!(
            a.receive(start + Duration::from_millis(50), &mut buffer)
                .unwrap()
                .map(|r| (r.length, r.from)),
            None
        );
        assert_eq!(
            b.receive(start, &mut buffer)
                .unwrap()
                .map(|r| (r.length, r.from)),
            Some((4, MemoryAddress(1)))
        );
        b.send(start, MemoryAddress(1), b"pong").unwrap();
        assert_eq!(
            a.receive(start + Duration::from_millis(60), &mut buffer)
                .unwrap(),
            None
        );
        let back = a
            .receive(start + Duration::from_millis(110), &mut buffer)
            .unwrap();
        // Arrived at 60 ms, held 50.
        let at = start + Duration::from_millis(110);
        assert_eq!(
            back,
            Some(Received {
                length: 4,
                from: MemoryAddress(2),
                at
            })
        );
        assert_eq!(&buffer[..4], b"pong");
    }

    #[test]
    fn loss_drops_about_its_share() {
        let network = MemoryNetwork::new();
        let conditions = Conditions {
            loss: 0.25,
            ..Conditions::default()
        };
        let mut a = Conditioned::new(network.endpoint(MemoryAddress(1)), conditions, 7);
        let mut b = network.endpoint(MemoryAddress(2));
        let now = Instant::now();
        for _ in 0..4000 {
            a.send(now, MemoryAddress(2), b"x").unwrap();
        }
        let mut buffer = [0; 4];
        let mut arrived = 0;
        while b.receive(now, &mut buffer).unwrap().is_some() {
            arrived += 1;
        }
        assert!((2850..3150).contains(&arrived), "{arrived}");
    }
}
