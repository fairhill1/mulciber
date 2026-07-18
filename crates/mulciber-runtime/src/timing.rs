use std::error::Error;
use std::fmt;
use std::time::{Duration, Instant};

const DEFAULT_MAX_FRAME_DELTA: Duration = Duration::from_millis(250);
const DEFAULT_MAX_FIXED_STEPS: u32 = 8;

/// Configuration for fixed simulation and variable-rate rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    fixed_step: Duration,
    max_frame_delta: Duration,
    max_fixed_steps: u32,
}

impl RuntimeConfig {
    /// Creates a fixed-rate configuration with bounded catch-up defaults.
    ///
    /// The default maximum accepted frame delta is 250 milliseconds and at most eight fixed steps
    /// may run in one frame. Whole fixed steps beyond that budget are discarded to avoid a spiral
    /// of death, and the discarded duration is reported by [`FramePlan::dropped_time`].
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeConfigError::ZeroUpdateRate`] when `updates_per_second` is zero, or
    /// [`RuntimeConfigError::UpdateRateTooHigh`] when its period is below `Duration` resolution.
    pub fn fixed_hz(updates_per_second: u32) -> Result<Self, RuntimeConfigError> {
        if updates_per_second == 0 {
            return Err(RuntimeConfigError::ZeroUpdateRate);
        }
        let fixed_step = Duration::from_secs_f64(1.0 / f64::from(updates_per_second));
        if fixed_step.is_zero() {
            return Err(RuntimeConfigError::UpdateRateTooHigh);
        }
        Ok(Self {
            fixed_step,
            max_frame_delta: DEFAULT_MAX_FRAME_DELTA,
            max_fixed_steps: DEFAULT_MAX_FIXED_STEPS,
        })
    }

    /// Replaces the largest wall-clock delta accepted for one frame.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeConfigError::ZeroMaxFrameDelta`] when the duration is zero.
    pub fn with_max_frame_delta(
        mut self,
        max_frame_delta: Duration,
    ) -> Result<Self, RuntimeConfigError> {
        if max_frame_delta.is_zero() {
            return Err(RuntimeConfigError::ZeroMaxFrameDelta);
        }
        self.max_frame_delta = max_frame_delta;
        Ok(self)
    }

    /// Replaces the maximum number of fixed updates allowed in one frame.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeConfigError::ZeroMaxFixedSteps`] when the limit is zero.
    pub fn with_max_fixed_steps(
        mut self,
        max_fixed_steps: u32,
    ) -> Result<Self, RuntimeConfigError> {
        if max_fixed_steps == 0 {
            return Err(RuntimeConfigError::ZeroMaxFixedSteps);
        }
        self.max_fixed_steps = max_fixed_steps;
        Ok(self)
    }

    /// Returns the duration of one fixed simulation update.
    #[must_use]
    pub const fn fixed_step(self) -> Duration {
        self.fixed_step
    }

    /// Returns the largest wall-clock delta accepted for one frame.
    #[must_use]
    pub const fn max_frame_delta(self) -> Duration {
        self.max_frame_delta
    }

    /// Returns the maximum number of fixed updates allowed in one frame.
    #[must_use]
    pub const fn max_fixed_steps(self) -> u32 {
        self.max_fixed_steps
    }
}

/// An invalid runtime timing configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeConfigError {
    /// The requested fixed update rate was zero.
    ZeroUpdateRate,
    /// The requested update rate cannot be represented by a nonzero [`Duration`].
    UpdateRateTooHigh,
    /// The maximum accepted frame delta was zero.
    ZeroMaxFrameDelta,
    /// The maximum number of fixed updates per frame was zero.
    ZeroMaxFixedSteps,
}

impl fmt::Display for RuntimeConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ZeroUpdateRate => "fixed update rate must be greater than zero",
            Self::UpdateRateTooHigh => "fixed update rate is too high to represent",
            Self::ZeroMaxFrameDelta => "maximum frame delta must be greater than zero",
            Self::ZeroMaxFixedSteps => "maximum fixed steps must be greater than zero",
        })
    }
}

impl Error for RuntimeConfigError {}

/// Work scheduled for one rendered frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FramePlan {
    frame_delta: Duration,
    fixed_step: Duration,
    fixed_steps: u32,
    interpolation: f64,
    dropped_time: Duration,
}

impl FramePlan {
    /// Returns the clamped variable-rate frame delta.
    #[must_use]
    pub const fn frame_delta(self) -> Duration {
        self.frame_delta
    }

    /// Returns the duration to use for every fixed simulation update.
    #[must_use]
    pub const fn fixed_step(self) -> Duration {
        self.fixed_step
    }

    /// Returns how many fixed simulation updates should run before rendering.
    #[must_use]
    pub const fn fixed_steps(self) -> u32 {
        self.fixed_steps
    }

    /// Returns the fractional position between the previous and current simulation states.
    ///
    /// After running [`Self::fixed_steps`] updates, render state as
    /// `previous.lerp(current, interpolation)` to remain smooth at arbitrary presentation rates.
    #[must_use]
    pub const fn interpolation(self) -> f64 {
        self.interpolation
    }

    /// Returns wall-clock time discarded by frame clamping or the catch-up step limit.
    #[must_use]
    pub const fn dropped_time(self) -> Duration {
        self.dropped_time
    }
}

#[derive(Debug)]
pub(crate) struct FrameClock {
    config: RuntimeConfig,
    previous: Instant,
    accumulator: Duration,
    suspended: bool,
}

impl FrameClock {
    pub(crate) const fn new(config: RuntimeConfig, started_at: Instant) -> Self {
        Self {
            config,
            previous: started_at,
            accumulator: Duration::ZERO,
            suspended: false,
        }
    }

    pub(crate) fn advance(&mut self, now: Instant) -> FramePlan {
        if self.suspended {
            return self.idle_plan();
        }
        let elapsed = now.saturating_duration_since(self.previous);
        self.previous = now;
        self.advance_by(elapsed)
    }

    pub(crate) const fn suspend(&mut self) {
        self.suspended = true;
    }

    pub(crate) const fn resume(&mut self, now: Instant) {
        self.previous = now;
        self.suspended = false;
    }

    pub(crate) const fn suspended(&self) -> bool {
        self.suspended
    }

    fn idle_plan(&self) -> FramePlan {
        FramePlan {
            frame_delta: Duration::ZERO,
            fixed_step: self.config.fixed_step,
            fixed_steps: 0,
            interpolation: duration_ratio(self.accumulator, self.config.fixed_step),
            dropped_time: Duration::ZERO,
        }
    }

    fn advance_by(&mut self, elapsed: Duration) -> FramePlan {
        let frame_delta = elapsed.min(self.config.max_frame_delta);
        let mut dropped_time = elapsed.saturating_sub(frame_delta);
        self.accumulator += frame_delta;

        let available_steps = duration_ratio_floor(self.accumulator, self.config.fixed_step);
        let fixed_steps =
            u32::try_from(available_steps.min(u128::from(self.config.max_fixed_steps)))
                .expect("fixed step count is bounded by a u32 configuration value");
        self.accumulator -= self.config.fixed_step * fixed_steps;

        let excess_steps = available_steps.saturating_sub(u128::from(fixed_steps));
        if excess_steps != 0 {
            let remainder_nanos = self.accumulator.as_nanos() % self.config.fixed_step.as_nanos();
            let remainder = Duration::from_nanos(
                u64::try_from(remainder_nanos)
                    .expect("fixed-step remainder is less than one second"),
            );
            let excess_duration = self
                .accumulator
                .checked_sub(remainder)
                .expect("fixed-step remainder cannot exceed the accumulator");
            self.accumulator = remainder;
            dropped_time += excess_duration;
        }

        FramePlan {
            frame_delta,
            fixed_step: self.config.fixed_step,
            fixed_steps,
            interpolation: duration_ratio(self.accumulator, self.config.fixed_step),
            dropped_time,
        }
    }
}

fn duration_ratio_floor(numerator: Duration, denominator: Duration) -> u128 {
    numerator.as_nanos() / denominator.as_nanos()
}

#[allow(clippy::cast_precision_loss)]
fn duration_ratio(numerator: Duration, denominator: Duration) -> f64 {
    numerator.as_secs_f64() / denominator.as_secs_f64()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{FrameClock, RuntimeConfig, RuntimeConfigError};

    #[test]
    fn rejects_zero_limits() {
        assert_eq!(
            RuntimeConfig::fixed_hz(0),
            Err(RuntimeConfigError::ZeroUpdateRate)
        );
        assert_eq!(
            RuntimeConfig::fixed_hz(u32::MAX),
            Err(RuntimeConfigError::UpdateRateTooHigh)
        );
        assert_eq!(
            RuntimeConfig::fixed_hz(60)
                .unwrap()
                .with_max_frame_delta(Duration::ZERO),
            Err(RuntimeConfigError::ZeroMaxFrameDelta)
        );
        assert_eq!(
            RuntimeConfig::fixed_hz(60).unwrap().with_max_fixed_steps(0),
            Err(RuntimeConfigError::ZeroMaxFixedSteps)
        );
    }

    #[test]
    fn accumulates_fixed_steps_and_render_interpolation() {
        let start = Instant::now();
        let config = RuntimeConfig::fixed_hz(10).unwrap();
        let mut clock = FrameClock::new(config, start);

        let first = clock.advance(start + Duration::from_millis(40));
        assert_eq!(first.fixed_steps(), 0);
        assert!((first.interpolation() - 0.4).abs() < f64::EPSILON);

        let second = clock.advance(start + Duration::from_millis(125));
        assert_eq!(second.fixed_steps(), 1);
        assert!((second.interpolation() - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn clamps_long_frames_and_discards_excess_catch_up_steps() {
        let start = Instant::now();
        let config = RuntimeConfig::fixed_hz(100)
            .unwrap()
            .with_max_frame_delta(Duration::from_millis(100))
            .unwrap()
            .with_max_fixed_steps(3)
            .unwrap();
        let mut clock = FrameClock::new(config, start);

        let plan = clock.advance(start + Duration::from_millis(250));
        assert_eq!(plan.frame_delta(), Duration::from_millis(100));
        assert_eq!(plan.fixed_steps(), 3);
        assert_eq!(plan.dropped_time(), Duration::from_millis(220));
        assert!(plan.interpolation().abs() < f64::EPSILON);
    }

    #[test]
    fn suspension_freezes_time_and_preserves_interpolation() {
        let start = Instant::now();
        let config = RuntimeConfig::fixed_hz(10).unwrap();
        let mut clock = FrameClock::new(config, start);
        let before = clock.advance(start + Duration::from_millis(40));
        assert!((before.interpolation() - 0.4).abs() < f64::EPSILON);

        clock.suspend();
        let suspended = clock.advance(start + Duration::from_mins(1));
        assert_eq!(suspended.frame_delta(), Duration::ZERO);
        assert_eq!(suspended.fixed_steps(), 0);
        assert!((suspended.interpolation() - 0.4).abs() < f64::EPSILON);

        let resumed_at = start + Duration::from_mins(2);
        clock.resume(resumed_at);
        let resumed = clock.advance(resumed_at + Duration::from_millis(60));
        assert_eq!(resumed.fixed_steps(), 1);
        assert!(resumed.interpolation().abs() < f64::EPSILON);
        assert_eq!(resumed.dropped_time(), Duration::ZERO);
    }
}
