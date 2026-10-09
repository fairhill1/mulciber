//! Unpredictable numbers for nonces and session tokens, and a seeded generator for simulations.

use std::hash::{BuildHasher, RandomState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A number an observer can't guess: the counter and clock hashed with the standard library's
/// randomly keyed `SipHash`. Good enough to stop spoofed packets guessing a session; not a key for
/// encryption.
pub(crate) fn unpredictable() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    RandomState::new().hash_one((COUNTER.fetch_add(1, Ordering::Relaxed), nanos))
}

/// xorshift64*: fast, repeatable from its seed.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Uniform in [0, 1).
    pub(crate) fn unit(&mut self) -> f64 {
        #[allow(clippy::cast_precision_loss, reason = "53 bits fit an f64 exactly")]
        let (value, scale) = ((self.next_u64() >> 11) as f64, (1u64 << 53) as f64);
        value / scale
    }
}
