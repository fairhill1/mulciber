//! Native display timing, independent of observed application frame rate.

use std::time::Duration;

/// Timing reported for the window's current display. A variable range is a capability,
/// not proof that the compositor presented a particular frame with VRR engaged.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum DisplayTiming {
    /// The backend cannot establish a timing policy; do not assume fixed refresh.
    #[default]
    Unknown,
    /// The screen reports one fixed refresh period.
    Fixed(Duration),
    /// The screen reports a range. Preserve elapsed simulation time within and below it.
    Variable {
        /// Shortest supported refresh interval.
        minimum_interval: Duration,
        /// Longest supported refresh interval.
        maximum_interval: Duration,
        /// Zero allows continuous updates; otherwise updates lie on this native grid.
        update_granularity: Duration,
    },
}

impl DisplayTiming {
    /// Classifies native intervals in seconds. Invalid or unavailable values remain unknown.
    #[must_use]
    pub fn from_intervals(minimum: f64, maximum: f64, granularity: f64) -> Self {
        if !minimum.is_finite()
            || !maximum.is_finite()
            || !granularity.is_finite()
            || minimum <= 0.0
            || maximum < minimum
            || maximum > 1.0
            || granularity < 0.0
            || granularity > maximum
        {
            return Self::Unknown;
        }
        let Ok(minimum_interval) = Duration::try_from_secs_f64(minimum) else {
            return Self::Unknown;
        };
        let Ok(maximum_interval) = Duration::try_from_secs_f64(maximum) else {
            return Self::Unknown;
        };
        let Ok(update_granularity) = Duration::try_from_secs_f64(granularity) else {
            return Self::Unknown;
        };
        if minimum_interval.is_zero() {
            return Self::Unknown;
        }
        if minimum_interval == maximum_interval {
            Self::Fixed(minimum_interval)
        } else {
            Self::Variable {
                minimum_interval,
                maximum_interval,
                update_granularity,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_ranges_distinguish_fixed_variable_and_unavailable() {
        assert_eq!(
            DisplayTiming::from_intervals(0.02, 0.02, 0.02),
            DisplayTiming::Fixed(Duration::from_millis(20))
        );
        assert!(matches!(
            DisplayTiming::from_intervals(1.0 / 144.0, 1.0 / 48.0, 0.0),
            DisplayTiming::Variable { .. }
        ));
        for (min, max, granularity) in [
            (0.0, 0.0, 0.0),
            (0.02, 0.01, 0.0),
            (f64::NAN, 0.02, 0.0),
            (0.01, f64::INFINITY, 0.0),
            (0.01, 0.02, -0.01),
            (0.01, 0.02, 0.03),
        ] {
            assert_eq!(
                DisplayTiming::from_intervals(min, max, granularity),
                DisplayTiming::Unknown
            );
        }
    }
}
