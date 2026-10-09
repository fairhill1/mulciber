//! 16-bit sequence numbers that wrap, and a ring buffer indexed by them.

/// Whether `a` comes after `b`, treating the two as wrapping 16-bit counters less than half the
/// range apart.
pub(crate) fn newer(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}

/// Entries keyed by sequence number, the latest `N` kept.
pub(crate) struct SequenceBuffer<T> {
    entries: Vec<Option<(u16, T)>>,
}

impl<T> SequenceBuffer<T> {
    pub(crate) fn new(size: usize) -> SequenceBuffer<T> {
        SequenceBuffer {
            entries: (0..size).map(|_| None).collect(),
        }
    }

    fn slot(&self, sequence: u16) -> usize {
        usize::from(sequence) % self.entries.len()
    }

    pub(crate) fn get(&self, sequence: u16) -> Option<&T> {
        match &self.entries[self.slot(sequence)] {
            Some((s, value)) if *s == sequence => Some(value),
            _ => None,
        }
    }

    pub(crate) fn get_mut(&mut self, sequence: u16) -> Option<&mut T> {
        let slot = self.slot(sequence);
        match &mut self.entries[slot] {
            Some((s, value)) if *s == sequence => Some(value),
            _ => None,
        }
    }

    /// Stores `value` under `sequence`, returning whatever held its slot.
    pub(crate) fn insert(&mut self, sequence: u16, value: T) -> Option<(u16, T)> {
        let slot = self.slot(sequence);
        self.entries[slot].replace((sequence, value))
    }

    /// Empties the slots of the sequence numbers after `from` up to and including `to`, so entries
    /// a whole buffer old can't be mistaken for new ones.
    pub(crate) fn clear_after(&mut self, from: u16, to: u16) {
        let span = usize::from(to.wrapping_sub(from)).min(self.entries.len());
        for k in 1..=span {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "k is at most the buffer's size, below 65536"
            )]
            let slot = self.slot(from.wrapping_add(k as u16));
            self.entries[slot] = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_numbers_compare_across_the_wrap() {
        assert!(newer(1, 0));
        assert!(newer(0, 65535));
        assert!(newer(10, 65530));
        assert!(!newer(65530, 10));
        assert!(!newer(5, 5));
    }

    #[test]
    fn a_ring_forgets_what_a_whole_ring_ago_held() {
        let mut buffer = SequenceBuffer::new(4);
        buffer.insert(65534, 'a');
        buffer.insert(1, 'b');
        assert_eq!(buffer.get(65534), Some(&'a'));
        // 65534 + 4 wraps to 2: the same slot, a different number.
        assert_eq!(buffer.get(2), None);
        buffer.clear_after(1, 3);
        assert_eq!(buffer.get(65534), None);
        assert_eq!(buffer.get(1), Some(&'b'));
    }
}
