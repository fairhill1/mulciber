//! Automatic full/half refresh based on workload, never on the already-limited FPS.
use std::time::{Duration, Instant};

const OVERLOAD_SAMPLES: u32 = 3;
const RECOVERY_SAMPLES: u32 = 90;

#[derive(Default)]
pub(super) struct StrictPacing {
    period: Option<Duration>,
    started: Option<Instant>,
    cpu: Option<Duration>,
    gpu: Option<Duration>,
    fresh_gpu: bool,
    half: bool,
    overload: u32,
    recovery: u32,
}

impl StrictPacing {
    pub(super) fn begin_frame(&mut self, now: Instant) {
        self.started = Some(now);
        self.cpu = None;
    }

    /// Stop CPU work measurement before a native queue submission can apply backpressure.
    pub(super) fn end_frame(&mut self, now: Instant) {
        self.cpu = self
            .started
            .map(|start| now.saturating_duration_since(start));
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
            self.half = false;
            self.overload = 0;
            self.recovery = 0;
        }
        let cpu = self.cpu.or_else(|| {
            self.started
                .map(|start| now.saturating_duration_since(start))
        });
        // GPU timestamps arrive a few frames late. Absence is not evidence of overload,
        // and replaying one old result must not count as several slow samples.
        let gpu = self.gpu.filter(|_| self.fresh_gpu);
        if !self.half {
            if cpu.is_some_and(|work| work > period) || gpu.is_some_and(|work| work > period) {
                self.overload += 1;
                if self.overload >= OVERLOAD_SAMPLES {
                    self.half = true;
                    self.overload = 0;
                    self.recovery = 0;
                }
            } else if gpu.is_some() {
                self.overload = 0;
            }
        } else if let (Some(cpu), Some(gpu)) = (cpu, gpu) {
            // Five percent headroom avoids flapping, without demanding ~94 FPS worth
            // of capacity before permitting 75 Hz as the old twenty percent rule did.
            if cpu <= period.mul_f64(0.95) && gpu <= period.mul_f64(0.95) {
                self.recovery += 1;
                if self.recovery >= RECOVERY_SAMPLES {
                    self.half = false;
                    self.recovery = 0;
                }
            } else {
                self.recovery = 0;
            }
        }
        self.fresh_gpu = false;
        self.divisor()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(policy: &mut StrictPacing, now: Instant, period: Duration, work: Duration) -> u32 {
        policy.begin_frame(now);
        policy.record_gpu_time(work);
        policy.update(now + work, period)
    }

    #[test]
    fn capacity_for_eighty_five_or_ninety_fps_runs_at_full_seventy_five_hz() {
        let period = Duration::from_secs_f64(1.0 / 75.0);
        for fps in [85.0, 90.0] {
            let mut policy = StrictPacing::default();
            let mut now = Instant::now();
            let work = Duration::from_secs_f64(1.0 / fps);
            assert_eq!(policy.divisor(), 1);
            for _ in 0..120 {
                now += period;
                assert_eq!(frame(&mut policy, now, period, work), 1);
            }
            for i in 1..=OVERLOAD_SAMPLES {
                now += period;
                assert_eq!(
                    frame(&mut policy, now, period, period * 2),
                    if i == OVERLOAD_SAMPLES { 2 } else { 1 }
                );
            }
            for i in 1..=RECOVERY_SAMPLES {
                now += period * 2;
                assert_eq!(
                    frame(&mut policy, now, period, work),
                    if i == RECOVERY_SAMPLES { 1 } else { 2 }
                );
            }
            // Queued half-rate presentations and idle gaps are not rendering work.
            for _ in 0..10 {
                now += period * 2;
                assert_eq!(frame(&mut policy, now, period, work), 1);
            }
        }
    }

    #[test]
    fn isolated_hitches_do_not_halve_the_rate_but_sustained_overload_does() {
        for hz in [59.94, 60.0, 75.0, 120.0, 144.0] {
            let period = Duration::from_secs_f64(1.0 / hz);
            let mut policy = StrictPacing::default();
            let mut now = Instant::now();
            for i in 0..180 {
                now += period;
                let work = if i % 20 == 0 {
                    period * 2
                } else {
                    period.mul_f64(0.98)
                };
                assert_eq!(frame(&mut policy, now, period, work), 1);
            }
            for _ in 0..OVERLOAD_SAMPLES {
                now += period;
                frame(&mut policy, now, period, period.mul_f64(1.1));
            }
            assert_eq!(policy.divisor(), 2);
        }
    }

    #[test]
    fn missing_or_stale_gpu_samples_do_not_manufacture_overload_or_recovery() {
        let period = Duration::from_millis(16);
        let mut policy = StrictPacing::default();
        let mut now = Instant::now();
        for _ in 0..100 {
            now += period;
            policy.begin_frame(now);
            assert_eq!(policy.update(now + period / 2, period), 1);
        }
        policy.record_gpu_time(period * 2);
        for _ in 0..100 {
            now += period;
            policy.begin_frame(now);
            assert_eq!(policy.update(now + period / 2, period), 1);
        }
        for _ in 0..OVERLOAD_SAMPLES {
            now += period;
            frame(&mut policy, now, period, period * 2);
        }
        policy.record_gpu_time(period / 2);
        for _ in 0..100 {
            now += period * 2;
            policy.begin_frame(now);
            assert_eq!(policy.update(now + period / 2, period), 2);
        }
    }

    #[test]
    fn native_submission_wait_is_not_cpu_work() {
        let period = Duration::from_secs_f64(1.0 / 75.0);
        let mut policy = StrictPacing::default();
        let mut now = Instant::now();
        for _ in 0..100 {
            now += period * 3;
            policy.begin_frame(now);
            policy.record_gpu_time(period / 2);
            policy.end_frame(now + period / 2);
            assert_eq!(policy.update(now + period * 2, period), 1);
        }
    }
}
