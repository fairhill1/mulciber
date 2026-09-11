//! Opt-in frame-start pacing using the native display period.

use std::time::{Duration, Instant};

const SPIN: Duration = Duration::from_micros(300);

/// Spaces frame starts before fresh input is sampled. The period must come from
/// the display, not the interval between rendered or successfully shown frames.
#[derive(Debug)]
pub struct FrameStartLimiter {
    enabled: bool,
    refresh_interval: Option<Duration>,
    next_start: Option<Instant>,
    cadence: Option<Duration>,
}

impl FrameStartLimiter {
    /// Enables limiting on application-selected presentation paths.
    #[must_use]
    pub const fn new(enabled: bool) -> Self {
        Self {
            enabled,
            refresh_interval: None,
            next_start: None,
            cadence: None,
        }
    }

    /// Supplies the native display period, independent of skipped frames.
    pub fn set_refresh_interval(&mut self, interval: Option<Duration>) {
        self.refresh_interval = interval
            .filter(|value| (Duration::from_millis(2)..=Duration::from_millis(50)).contains(value));
        if self.refresh_interval.is_none() {
            self.next_start = None;
            self.cadence = None;
        }
    }

    /// Forgets the display period and pending deadline after suspension.
    pub const fn reset(&mut self) {
        self.refresh_interval = None;
        self.next_start = None;
        self.cadence = None;
    }

    /// Waits before pumping input. A short spin tail avoids coarse sleep rounding.
    /// Without a native display period, or when disabled, this returns immediately.
    pub fn wait(&mut self) {
        if !self.enabled {
            return;
        }
        let Some(cadence) = self.refresh_interval else {
            return;
        };
        let deadline = self.schedule(Instant::now(), cadence);
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if remaining > SPIN {
                std::thread::sleep(remaining.saturating_sub(SPIN));
            } else {
                std::hint::spin_loop();
            }
        }
    }

    fn schedule(&mut self, now: Instant, cadence: Duration) -> Instant {
        if self
            .cadence
            .is_some_and(|old| old.abs_diff(cadence) > old / 20)
        {
            self.next_start = None;
        }
        self.cadence = Some(cadence);
        let deadline = self.next_start.unwrap_or(now);
        // Preserve the grid through short overruns, but never repay a long stall
        // with a burst of frames or wait a whole extra refresh for a late frame.
        let mut next = deadline + cadence;
        if next <= now {
            next = now + cadence;
        }
        self.next_start = Some(next);
        deadline.max(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_overruns_preserve_the_grid_and_long_stalls_do_not_burst() {
        let mut limiter = FrameStartLimiter::new(true);
        let start = Instant::now();
        let period = Duration::from_micros(13_338);
        assert_eq!(limiter.schedule(start, period), start);
        assert_eq!(
            limiter.schedule(start + Duration::from_millis(10), period),
            start + period
        );
        let late = start + period * 2 + Duration::from_millis(2);
        assert_eq!(limiter.schedule(late, period), late);
        assert_eq!(
            limiter.schedule(late + Duration::from_millis(8), period),
            start + period * 3
        );
        let stalled = start + Duration::from_secs(5);
        assert_eq!(limiter.schedule(stalled, period), stalled);
        assert_eq!(limiter.schedule(stalled, period), stalled + period);
    }

    #[test]
    fn display_changes_and_reset_forget_the_old_deadline() {
        let mut limiter = FrameStartLimiter::new(true);
        let now = Instant::now();
        limiter.schedule(now, Duration::from_millis(16));
        assert_eq!(limiter.schedule(now, Duration::from_millis(8)), now);
        limiter.reset();
        assert_eq!(limiter.schedule(now, Duration::from_millis(8)), now);
    }

    #[test]
    fn missing_or_invalid_refresh_clears_stale_schedule() {
        let mut limiter = FrameStartLimiter::new(true);
        let now = Instant::now();
        for period in [None, Some(Duration::ZERO), Some(Duration::from_secs(1))] {
            limiter.schedule(now, Duration::from_millis(13));
            limiter.set_refresh_interval(period);
            assert!(limiter.refresh_interval.is_none());
            assert!(limiter.next_start.is_none());
        }
    }
}
