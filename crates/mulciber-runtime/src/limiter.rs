//! Frame-start pacing with explicit caps or the native display period.

use std::time::{Duration, Instant};

const SPIN: Duration = Duration::from_micros(300);

/// Spaces frame starts before fresh input is sampled. An explicit cap is independent of
/// display cadence; the optional native-refresh cap uses the display's own period.
#[derive(Debug)]
pub struct FrameStartLimiter {
    enabled: bool,
    refresh_interval: Option<Duration>,
    next_start: Option<Instant>,
    cadence: Option<Duration>,
    frame_interval: Option<Duration>,
    sleep_margin: Duration,
    display_timing: Option<mulciber_platform::DisplayTiming>,
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
            frame_interval: None,
            sleep_margin: SPIN,
            display_timing: None,
        }
    }

    /// Sets an explicit frame-rate ceiling independent of the native refresh rate.
    ///
    /// `None` removes the explicit cap. A cap works even when the native-refresh limiter
    /// is disabled, and takes precedence over it: 50 FPS remains 50, never rounded to 30.
    /// Changing the cap resets the deadline, so the previous cap cannot delay fresh input.
    pub fn set_frame_rate_limit(&mut self, fps: Option<std::num::NonZeroU16>) {
        let interval = fps.map(|fps| Duration::from_secs_f64(1.0 / f64::from(fps.get())));
        if self.frame_interval != interval {
            self.frame_interval = interval;
            self.next_start = None;
            self.cadence = None;
        }
    }

    fn target_interval(&self) -> Option<Duration> {
        self.frame_interval.or_else(|| {
            if !self.enabled {
                return None;
            }
            match self.display_timing {
                Some(mulciber_platform::DisplayTiming::Fixed(period)) if !period.is_zero() => {
                    Some(period)
                }
                Some(_) => None,
                None => self.refresh_interval,
            }
        })
    }

    /// Supplies native display timing. Variable/unknown timing disables the implicit refresh
    /// ceiling; an explicit user cap still applies. This overrides nominal-period feedback.
    pub fn set_display_timing(&mut self, timing: mulciber_platform::DisplayTiming) {
        if self.display_timing != Some(timing) {
            self.display_timing = Some(timing);
            self.next_start = None;
            self.cadence = None;
        }
    }

    /// Supplies the native display period, independent of skipped frames.
    pub fn set_refresh_interval(&mut self, interval: Option<Duration>) {
        self.refresh_interval = interval
            .filter(|value| (Duration::from_millis(2)..=Duration::from_millis(50)).contains(value));
        if self.refresh_interval.is_none() && self.frame_interval.is_none() {
            self.next_start = None;
            self.cadence = None;
        }
    }

    /// Forgets the display period and pending deadline after suspension.
    pub const fn reset(&mut self) {
        self.refresh_interval = None;
        if self.display_timing.is_some() {
            self.display_timing = Some(mulciber_platform::DisplayTiming::Unknown);
        }
        self.next_start = None;
        self.cadence = None;
    }

    /// Waits before pumping input. A short spin tail avoids coarse sleep rounding.
    /// An explicit cap works without display feedback. Otherwise the native limiter is opt-in.
    pub fn wait(&mut self) {
        let Some(cadence) = self.target_interval() else {
            return;
        };
        let deadline = self.schedule(Instant::now(), cadence);
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if remaining > self.sleep_margin {
                let sleep_for = remaining.saturating_sub(self.sleep_margin);
                let started = Instant::now();
                std::thread::sleep(sleep_for);
                // Timer coalescing can exceed the nominal spin tail. Learn the wakeup
                // error so subsequent waits sleep less, bounding busy work to 3 ms.
                let margin = started.elapsed().saturating_sub(sleep_for) + SPIN;
                self.sleep_margin = self.sleep_margin.max(margin.min(Duration::from_millis(3)));
            } else {
                std::hint::spin_loop();
            }
        }
        if self.frame_interval.is_some() {
            // Cap actual frame starts, including OS wakeup lateness. Never repay a missed
            // deadline with a shorter following frame.
            self.next_start = Some(Instant::now() + cadence);
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
        let mut next = if self.frame_interval.is_some() {
            deadline.max(now) + cadence
        } else {
            deadline + cadence
        };
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
    fn variable_or_unknown_display_has_no_implicit_cap_but_keeps_user_caps() {
        use mulciber_platform::DisplayTiming;
        let mut limiter = FrameStartLimiter::new(true);
        let native = Duration::from_nanos(16_666_667);
        limiter.set_refresh_interval(Some(native));
        limiter.set_display_timing(DisplayTiming::Fixed(native));
        assert_eq!(limiter.target_interval(), Some(native));
        for timing in [
            DisplayTiming::Unknown,
            DisplayTiming::from_intervals(1.0 / 144.0, 1.0 / 48.0, 0.0),
        ] {
            limiter.schedule(Instant::now(), native);
            limiter.set_display_timing(timing);
            assert!(limiter.next_start.is_none());
            assert_eq!(limiter.target_interval(), None);
            limiter.set_refresh_interval(Some(native));
            assert_eq!(limiter.target_interval(), None);
            limiter.set_frame_rate_limit(std::num::NonZeroU16::new(50));
            assert_eq!(limiter.target_interval(), Some(Duration::from_millis(20)));
            limiter.reset();
            assert_eq!(limiter.target_interval(), Some(Duration::from_millis(20)));
            limiter.set_frame_rate_limit(None);
            assert_eq!(limiter.target_interval(), None);
        }
    }

    #[test]
    fn fifty_fps_is_not_rounded_to_a_refresh_divisor() {
        let mut limiter = FrameStartLimiter::new(false);
        limiter.set_frame_rate_limit(std::num::NonZeroU16::new(50));
        limiter.set_refresh_interval(Some(Duration::from_nanos(16_666_667)));
        let period = limiter.target_interval().unwrap();
        assert_eq!(period, Duration::from_millis(20));
        let base = Instant::now();
        for i in 0..1000 {
            assert_eq!(
                limiter.schedule(base + period * i, period),
                base + period * i
            );
        }
        limiter.reset();
        assert_eq!(limiter.target_interval(), Some(period));
        limiter.set_refresh_interval(None);
        assert_eq!(limiter.target_interval(), Some(period));
        limiter.set_frame_rate_limit(None);
        assert_eq!(limiter.target_interval(), None);
    }

    #[test]
    fn explicit_caps_do_not_catch_up_after_an_overloaded_frame() {
        let mut limiter = FrameStartLimiter::new(false);
        limiter.set_frame_rate_limit(std::num::NonZeroU16::new(50));
        let period = limiter.target_interval().unwrap();
        let base = Instant::now();
        assert_eq!(limiter.schedule(base, period), base);
        let late = base + Duration::from_millis(27);
        assert_eq!(limiter.schedule(late, period), late);
        assert_eq!(
            limiter.schedule(late + Duration::from_millis(10), period),
            late + period
        );
    }

    #[test]
    fn a_cap_change_discards_the_previous_deadline() {
        let mut limiter = FrameStartLimiter::new(false);
        limiter.set_frame_rate_limit(std::num::NonZeroU16::new(30));
        let now = Instant::now();
        limiter.schedule(now, limiter.target_interval().unwrap());
        limiter.set_frame_rate_limit(std::num::NonZeroU16::new(50));
        assert_eq!(
            limiter.schedule(now, limiter.target_interval().unwrap()),
            now
        );
    }

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
