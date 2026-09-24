//! Adaptive presentation where no native relaxed FIFO exists: measured throughput with
//! asymmetric recovery chooses between synchronized and immediate presentation.
use mulciber_platform::DisplayTiming;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct AdaptiveSync {
    timing: DisplayTiming,
    previous_start: Option<Instant>,
    gpu_time: Option<Duration>,
    synchronized: bool,
    ready_frames: u32,
    settling: u8,
}

impl AdaptiveSync {
    pub(super) fn record_gpu_time(&mut self, duration: Duration) {
        self.gpu_time = Some(duration);
    }

    pub(super) fn update(&mut self, now: Instant, timing: DisplayTiming) -> bool {
        if self.timing != timing {
            *self = Self {
                timing,
                ..Self::default()
            };
        }
        let elapsed = self
            .previous_start
            .replace(now)
            .map(|last| now.saturating_duration_since(last));
        match timing {
            DisplayTiming::Variable { .. } => self.synchronized = true,
            DisplayTiming::Fixed(period) if !period.is_zero() => {
                let Some(elapsed) = elapsed else {
                    return false;
                };
                // Release on an actual missed interval or a GPU frame over budget. Recovery
                // needs 45 consecutive starts at refresh rate and 10% GPU headroom, preventing
                // repeated mode switches around the threshold. Explicit sub-refresh caps never
                // satisfy this recovery condition. No artificial catch-up/30 FPS ceiling exists.
                if self.synchronized {
                    if (self.settling == 0 && elapsed > period.mul_f64(1.15))
                        || self.gpu_time.is_some_and(|gpu| gpu > period)
                    {
                        self.synchronized = false;
                        self.ready_frames = 0;
                    }
                } else if elapsed <= period.mul_f64(1.03)
                    && self.gpu_time.is_some_and(|gpu| gpu <= period.mul_f64(0.9))
                {
                    self.ready_frames += 1;
                    if self.ready_frames >= 45 {
                        self.synchronized = true;
                        self.settling = 4;
                    }
                } else {
                    self.ready_frames = 0;
                }
            }
            _ => self.synchronized = false,
        }
        self.settling = self.settling.saturating_sub(1);
        self.synchronized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PERIOD: Duration = Duration::from_nanos(16_666_667);

    #[test]
    fn sustained_sub_refresh_work_and_caps_stay_unsynchronized() {
        for fps in [30, 35, 40, 45, 50, 55] {
            let mut policy = AdaptiveSync::default();
            let mut now = Instant::now();
            for _ in 0..240 {
                now += Duration::from_secs_f64(1.0 / f64::from(fps));
                policy.record_gpu_time(Duration::from_millis(5));
                assert!(!policy.update(now, DisplayTiming::Fixed(PERIOD)));
            }
        }
    }

    #[test]
    fn misses_release_immediately_but_recovery_requires_sustained_headroom() {
        let mut policy = AdaptiveSync::default();
        let mut now = Instant::now();
        assert!(!policy.update(now, DisplayTiming::Fixed(PERIOD)));
        for i in 1..=45 {
            now += PERIOD;
            policy.record_gpu_time(Duration::from_millis(8));
            assert_eq!(policy.update(now, DisplayTiming::Fixed(PERIOD)), i == 45);
        }
        // Three previously queued frames may retain the old cadence.
        for _ in 0..3 {
            now += PERIOD * 2;
            assert!(policy.update(now, DisplayTiming::Fixed(PERIOD)));
        }
        now += PERIOD * 2;
        assert!(!policy.update(now, DisplayTiming::Fixed(PERIOD)));
        for _ in 0..100 {
            now += PERIOD;
            assert!(!policy.update(now, DisplayTiming::Fixed(PERIOD)));
            now += Duration::from_millis(20);
            assert!(!policy.update(now, DisplayTiming::Fixed(PERIOD)));
        }
        // A GPU bottleneck is not hidden by briefly fast CPU starts.
        for _ in 0..100 {
            now += Duration::from_millis(10);
            policy.record_gpu_time(Duration::from_millis(20));
            assert!(!policy.update(now, DisplayTiming::Fixed(PERIOD)));
        }
    }

    #[test]
    fn display_changes_and_unknown_timing_do_not_reuse_fixed_cadence() {
        let mut policy = AdaptiveSync::default();
        let now = Instant::now();
        let variable = DisplayTiming::from_intervals(1.0 / 120.0, 1.0 / 48.0, 0.0);
        assert!(policy.update(now, variable));
        assert!(!policy.update(now + PERIOD, DisplayTiming::Unknown));
        assert!(!policy.update(now + PERIOD * 2, DisplayTiming::Fixed(PERIOD)));
    }
}
