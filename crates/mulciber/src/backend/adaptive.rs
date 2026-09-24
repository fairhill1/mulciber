//! Adaptive presentation where no native relaxed FIFO exists: the same workload judgement
//! Strict steps its divisor on chooses between synchronized and immediate presentation.
//!
//! What a frame costs is the only honest signal. The interval between presents is not: an
//! application capping itself at the refresh rate lands exactly on the period with ordinary
//! jitter either side, so an interval test both releases on a single hitch the release cannot
//! help and refuses the recovery a workload with plenty of headroom has earned.
use super::pacing::StrictPacing;
use mulciber_platform::DisplayTiming;
use std::time::Instant;

/// Whether this present should wait for a vertical blank. `workload` is the backend's Strict
/// measurement, which it feeds in every mode and only Strict otherwise consults; sharing it
/// means one hitch never tears and a sustained overload releases after three samples.
pub(super) fn synchronized(
    workload: &mut StrictPacing,
    now: Instant,
    timing: DisplayTiming,
) -> bool {
    match timing {
        DisplayTiming::Variable { .. } => true,
        DisplayTiming::Fixed(period) if !period.is_zero() => workload.update(now, period) == 1,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    const PERIOD: Duration = Duration::from_nanos(13_338_669);

    fn present(policy: &mut StrictPacing, now: Instant, cpu: Duration, gpu: Duration) -> bool {
        policy.begin_frame(now);
        policy.end_frame(now + cpu);
        policy.record_gpu_time(gpu);
        synchronized(policy, now + cpu, DisplayTiming::Fixed(PERIOD))
    }

    #[test]
    fn a_capped_workload_with_headroom_stays_synchronized_through_hitches() {
        let mut policy = StrictPacing::default();
        let mut now = Instant::now();
        for i in 0..600 {
            // Starts held to the refresh period with jitter either side, and a hitch every
            // two seconds, like a streaming chunk landing.
            now += if i % 2 == 0 {
                PERIOD.mul_f64(1.08)
            } else {
                PERIOD.mul_f64(0.92)
            };
            let cpu = if i % 150 == 0 {
                PERIOD * 3
            } else {
                Duration::from_millis(4)
            };
            assert!(present(&mut policy, now, cpu, Duration::from_millis(8)));
        }
    }

    #[test]
    fn sustained_overload_tears_and_recovers_only_with_sustained_headroom() {
        let mut policy = StrictPacing::default();
        let mut now = Instant::now();
        for i in 1..=3 {
            now += PERIOD * 2;
            assert_eq!(
                present(&mut policy, now, Duration::from_millis(4), PERIOD * 2),
                i < 3
            );
        }
        for _ in 0..200 {
            now += Duration::from_millis(15);
            assert!(!present(
                &mut policy,
                now,
                Duration::from_millis(4),
                PERIOD.mul_f64(0.98)
            ));
        }
        let mut recovered = None;
        for i in 1..=120 {
            now += Duration::from_millis(9);
            if present(
                &mut policy,
                now,
                Duration::from_millis(4),
                Duration::from_millis(8),
            ) {
                recovered.get_or_insert(i);
            }
        }
        assert_eq!(recovered, Some(90));
    }

    #[test]
    fn variable_refresh_synchronizes_and_unknown_timing_does_not() {
        let mut policy = StrictPacing::default();
        let now = Instant::now();
        let variable = DisplayTiming::from_intervals(1.0 / 120.0, 1.0 / 48.0, 0.0);
        assert!(synchronized(&mut policy, now, variable));
        assert!(!synchronized(&mut policy, now, DisplayTiming::Unknown));
    }
}
