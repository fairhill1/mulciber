//! Automatic full/half refresh based on workload, never on the already-limited FPS.
use std::time::{Duration, Instant};

pub(super) struct StrictPacing {
    period: Option<Duration>,
    started: Option<Instant>,
    gap: Duration,
    gpu: Option<Duration>,
    fresh_gpu: bool,
    half: bool,
    recovery: u32,
    settling: u8,
}
impl Default for StrictPacing {
    fn default() -> Self {
        Self {
            period: None,
            started: None,
            gap: Duration::ZERO,
            gpu: None,
            fresh_gpu: false,
            half: true,
            recovery: 0,
            settling: 0,
        }
    }
}
impl StrictPacing {
    pub(super) fn begin_frame(&mut self, now: Instant) {
        self.gap = self
            .started
            .replace(now)
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last));
    }
    pub(super) fn record_gpu_time(&mut self, duration: Duration) {
        self.gpu = Some(duration);
        self.fresh_gpu = true;
    }
    pub(super) fn divisor(&self) -> u32 {
        if self.half { 2 } else { 1 }
    }
    pub(super) fn update(&mut self, now: Instant, period: Duration) -> u32 {
        if self.period != Some(period) {
            self.period = Some(period);
            self.settling = 0;
            self.half = true;
            self.recovery = 0;
        }
        let cpu = self
            .started
            .map_or(Duration::MAX, |start| now.saturating_duration_since(start));
        let gpu = self.gpu.unwrap_or(Duration::MAX);
        if !self.half {
            if cpu > period.mul_f64(0.97)
                || gpu > period.mul_f64(0.97)
                || (self.settling == 0 && self.gap > period.mul_f64(1.75))
            {
                self.half = true;
                self.recovery = 0;
            }
        } else if self.fresh_gpu {
            if cpu <= period.mul_f64(0.8) && gpu <= period.mul_f64(0.8) {
                self.recovery += 1;
                if self.recovery >= 90 {
                    self.half = false;
                    self.recovery = 0;
                    self.settling = 4;
                }
            } else {
                self.recovery = 0;
            }
        }
        self.settling = self.settling.saturating_sub(1);
        self.fresh_gpu = false;
        self.divisor()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_work_steps_down_and_sustained_headroom_recovers_without_probing() {
        for hz in [60.0, 75.0, 120.0, 144.0] {
            let period = Duration::from_secs_f64(1.0 / hz);
            let mut policy = StrictPacing::default();
            let mut now = Instant::now();
            for _ in 0..100 {
                now += period * 2;
                policy.begin_frame(now);
                policy.record_gpu_time(period.mul_f64(1.2));
                assert_eq!(policy.update(now + period / 10, period), 2);
            }
            for i in 1..=90 {
                now += period * 2;
                policy.begin_frame(now);
                policy.record_gpu_time(period / 2);
                assert_eq!(
                    policy.update(now + period / 10, period),
                    if i == 90 { 1 } else { 2 }
                );
            }
            now += period;
            policy.begin_frame(now);
            policy.record_gpu_time(period);
            assert_eq!(policy.update(now + period / 10, period), 2);
            for i in 0..180 {
                now += period * 2;
                policy.begin_frame(now);
                policy.record_gpu_time(if i % 20 == 0 { period } else { period / 2 });
                assert_eq!(policy.update(now + period / 10, period), 2);
            }
        }
    }
    #[test]
    fn recovery_ignores_old_queued_half_rate_frames_then_detects_new_misses() {
        let period = Duration::from_nanos(16_666_667);
        let mut policy = StrictPacing::default();
        let mut now = Instant::now();
        for _ in 0..90 {
            now += period * 2;
            policy.begin_frame(now);
            policy.record_gpu_time(period / 2);
            policy.update(now + period / 10, period);
        }
        assert_eq!(policy.divisor(), 1);
        for _ in 0..3 {
            now += period * 2;
            policy.begin_frame(now);
            policy.record_gpu_time(period / 2);
            assert_eq!(policy.update(now + period / 10, period), 1);
        }
        now += period * 2;
        policy.begin_frame(now);
        assert_eq!(policy.update(now + period / 10, period), 2);
    }

    #[test]
    fn unknown_gpu_cost_never_claims_full_rate_capacity() {
        let mut policy = StrictPacing::default();
        let period = Duration::from_millis(16);
        let mut now = Instant::now();
        for _ in 0..300 {
            now += period;
            policy.begin_frame(now);
            assert_eq!(policy.update(now + period / 10, period), 2);
        }
    }
}
