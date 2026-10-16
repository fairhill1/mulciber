//! Several recordings of one sound, played one at a time at random, never the same one twice
//! running.

use std::ops::RangeInclusive;

use crate::sound::Sound;

/// A take of `count` chosen by `random` that isn't `last`, unless there is only one: `None` when
/// there are none. Any `u32` of a game's random source will do; one keeping its own state per
/// emitter or per set calls this with each set's length and the take that set played last.
#[must_use]
pub fn other_take(count: usize, last: Option<usize>, random: u32) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let random = random as usize;
    let take = random % count;
    if count > 1 && last == Some(take) {
        // Any of the others alike, from the bits the first choice didn't use.
        Some((take + 1 + (random / count) % (count - 1)) % count)
    } else {
        Some(take)
    }
}

/// The takes of one sound: footsteps on a floor, a door's knocks, a weapon's swings. Each
/// [`pick`](Self::pick) is one of them at random, never the one picked last unless it is the only
/// one. It keeps its own small random source (xorshift), seeded from the clock or by
/// [`with_seed`](Self::with_seed) for a run that repeats.
#[derive(Clone, Debug)]
pub struct Takes {
    sounds: Vec<Sound>,
    last: Option<usize>,
    random: u32,
}

impl Takes {
    /// The takes, seeded from the clock.
    #[must_use]
    pub fn new(sounds: Vec<Sound>) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |since| since.subsec_nanos());
        Self::with_seed(sounds, seed)
    }

    /// The takes, with their random source seeded by `seed`.
    #[must_use]
    pub fn with_seed(sounds: Vec<Sound>, seed: u32) -> Self {
        Self {
            sounds,
            last: None,
            // xorshift never leaves zero.
            random: seed | 1,
        }
    }

    /// Every take, in the order given.
    #[must_use]
    pub fn sounds(&self) -> &[Sound] {
        &self.sounds
    }

    /// How many takes there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sounds.len()
    }

    /// Whether there are none, so [`pick`](Self::pick) never plays anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sounds.is_empty()
    }

    /// The take to play next: one at random other than the last, or `None` without any.
    pub fn pick(&mut self) -> Option<&Sound> {
        let random = self.next_random();
        let take = other_take(self.sounds.len(), self.last, random)?;
        self.last = Some(take);
        Some(&self.sounds[take])
    }

    /// The take to play next, as [`pick`](Self::pick) chooses it, and a playback rate drawn
    /// evenly from `rates` to vary its pitch (`0.95..=1.05`, say), for [`Play::rate`](crate::Play).
    pub fn pick_at_rate(&mut self, rates: RangeInclusive<f64>) -> Option<(&Sound, f64)> {
        self.pick()?;
        let (low, high) = rates.into_inner();
        let rate = low + (high - low) * f64::from(self.next_random()) / f64::from(u32::MAX);
        Some((&self.sounds[self.last?], rate))
    }

    /// xorshift32.
    fn next_random(&mut self) -> u32 {
        let mut x = self.random;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.random = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::{Takes, other_take};
    use crate::Sound;

    fn clip(level: f32) -> Sound {
        Sound::from_interleaved(vec![level; 4], 1, 48_000, 48_000)
    }

    #[test]
    fn another_take_is_never_the_last_and_any_other_comes_up() {
        assert_eq!(other_take(0, None, 7), None);
        assert_eq!(other_take(1, Some(0), 7), Some(0));
        let mut seen = [0_u32; 4];
        for random in 0..4000_u32 {
            let take = other_take(4, Some(2), random.wrapping_mul(2_654_435_761)).unwrap();
            assert_ne!(take, 2);
            seen[take] += 1;
        }
        // The other three about evenly.
        assert_eq!(seen[2], 0);
        for count in [seen[0], seen[1], seen[3]] {
            assert!((1100..1567).contains(&count), "{seen:?}");
        }
    }

    #[test]
    fn takes_never_repeat_and_vary_their_rate_within_its_range() {
        let mut takes = Takes::with_seed(vec![clip(0.1), clip(0.2), clip(0.3)], 12_345);
        assert_eq!(takes.len(), 3);
        let mut last = None;
        for _ in 0..200 {
            let (sound, rate) = takes.pick_at_rate(0.95..=1.05).unwrap();
            let level = sound.samples()[0];
            assert_ne!(Some(level), last);
            last = Some(level);
            assert!((0.95..=1.05).contains(&rate), "{rate}");
        }
        let mut one = Takes::with_seed(vec![clip(0.5)], 1);
        assert!(one.pick().is_some() && one.pick().is_some());
        let mut none = Takes::new(Vec::new());
        assert!(none.is_empty() && none.pick().is_none());
        assert!(none.pick_at_rate(1.0..=1.0).is_none());
    }
}
