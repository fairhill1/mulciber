//! Presentation pacing diagnostics.
//!
//! This module is the diagnostics-first half of the pacing vocabulary: it observes the presented
//! cadence a graphics backend reports and summarizes it, without owning any scheduling policy.
//! Timestamps arrive as plain [`Instant`]s so the diagnostics stay independent of any particular
//! graphics crate or feedback mechanism, including estimated timestamps where native feedback is
//! absent.

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

const DEFAULT_INTERVAL_WINDOW: usize = 240;
/// Intervals required before cadence estimation and missed-interval detection begin.
const MIN_ESTIMATION_INTERVALS: usize = 10;
/// An interval longer than the estimated cadence by this factor counts as missed.
const MISSED_INTERVAL_FACTOR: f64 = 1.5;

/// Accumulates presented-frame timestamps into cadence diagnostics.
///
/// Feed every presented frame in presentation order through [`Self::record_presented`] (or
/// [`Self::record_untimed_presented`] when presentation completed without a display time), then
/// read summaries with [`Self::report`].
#[derive(Debug)]
pub struct PacingDiagnostics {
    intervals: VecDeque<Duration>,
    window: usize,
    last_presented_at: Option<Instant>,
    presented_frames: u64,
    untimed_frames: u64,
    missed_intervals: u64,
}

impl Default for PacingDiagnostics {
    fn default() -> Self {
        Self::new()
    }
}

impl PacingDiagnostics {
    /// Creates diagnostics summarizing the most recent 240 presented intervals.
    #[must_use]
    pub fn new() -> Self {
        Self::with_window(DEFAULT_INTERVAL_WINDOW)
    }

    /// Creates diagnostics summarizing at most `window` recent presented intervals.
    ///
    /// A zero window is treated as a window of one interval.
    #[must_use]
    pub fn with_window(window: usize) -> Self {
        Self {
            intervals: VecDeque::new(),
            window: window.max(1),
            last_presented_at: None,
            presented_frames: 0,
            untimed_frames: 0,
            missed_intervals: 0,
        }
    }

    /// Records one presented frame with the display time the backend reported for it.
    ///
    /// A timestamp not later than its predecessor contributes no interval; the frame is still
    /// counted.
    pub fn record_presented(&mut self, presented_at: Instant) {
        self.presented_frames += 1;
        if let Some(previous) = self.last_presented_at
            && let Some(interval) = presented_at.checked_duration_since(previous)
            && !interval.is_zero()
        {
            if let Some(cadence) = self.estimated_cadence()
                && interval.as_secs_f64() > cadence.as_secs_f64() * MISSED_INTERVAL_FACTOR
            {
                self.missed_intervals += 1;
            }
            if self.intervals.len() >= self.window {
                self.intervals.pop_front();
            }
            self.intervals.push_back(interval);
        }
        self.last_presented_at = Some(presented_at);
    }

    /// Records one frame whose presentation completed without a reported display time, such as
    /// while the window is off screen.
    pub fn record_untimed_presented(&mut self) {
        self.presented_frames += 1;
        self.untimed_frames += 1;
    }

    /// Estimates the display cadence as the median of the recent presented intervals.
    ///
    /// Returns `None` until enough intervals have been observed to estimate responsibly.
    #[must_use]
    pub fn estimated_cadence(&self) -> Option<Duration> {
        if self.intervals.len() < MIN_ESTIMATION_INTERVALS {
            return None;
        }
        let mut sorted: Vec<Duration> = self.intervals.iter().copied().collect();
        sorted.sort_unstable();
        Some(sorted[(sorted.len() - 1) / 2])
    }

    /// Summarizes everything recorded so far.
    #[must_use]
    pub fn report(&self) -> PacingReport {
        let mut sorted: Vec<Duration> = self.intervals.iter().copied().collect();
        sorted.sort_unstable();
        let recent_intervals = (!sorted.is_empty()).then(|| IntervalSummary {
            samples: sorted.len(),
            min: sorted[0],
            median: sorted[(sorted.len() - 1) / 2],
            p95: sorted[percentile_rank(sorted.len(), 95)],
            max: sorted[sorted.len() - 1],
        });
        PacingReport {
            presented_frames: self.presented_frames,
            untimed_frames: self.untimed_frames,
            estimated_cadence: self.estimated_cadence(),
            recent_intervals,
            missed_intervals: self.missed_intervals,
        }
    }
}

/// Nearest-rank index for a percentile over `length` ascending samples.
fn percentile_rank(length: usize, percent: usize) -> usize {
    (percent * length).div_ceil(100).max(1) - 1
}

/// Distribution of the retained recent presented intervals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntervalSummary {
    /// Number of intervals summarized.
    pub samples: usize,
    /// Shortest retained interval.
    pub min: Duration,
    /// Median retained interval.
    pub median: Duration,
    /// Nearest-rank 95th-percentile interval.
    pub p95: Duration,
    /// Longest retained interval.
    pub max: Duration,
}

/// A point-in-time summary of presentation pacing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacingReport {
    /// Presented frames recorded, timed and untimed.
    pub presented_frames: u64,
    /// Presented frames that carried no display time.
    pub untimed_frames: u64,
    /// Median-of-window cadence estimate, once enough intervals exist.
    pub estimated_cadence: Option<Duration>,
    /// Distribution of the retained recent intervals, once any interval exists.
    pub recent_intervals: Option<IntervalSummary>,
    /// Intervals that exceeded 1.5 times the running cadence estimate.
    pub missed_intervals: u64,
}

impl fmt::Display for PacingReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} presented frames ({} without a display time)",
            self.presented_frames, self.untimed_frames
        )?;
        if let Some(cadence) = self.estimated_cadence {
            write!(
                formatter,
                ", estimated cadence {:.3} ms",
                cadence.as_secs_f64() * 1_000.0
            )?;
        }
        if let Some(intervals) = self.recent_intervals {
            write!(
                formatter,
                ", recent intervals (n={}) min {:.3} ms, median {:.3} ms, p95 {:.3} ms, max {:.3} ms",
                intervals.samples,
                intervals.min.as_secs_f64() * 1_000.0,
                intervals.median.as_secs_f64() * 1_000.0,
                intervals.p95.as_secs_f64() * 1_000.0,
                intervals.max.as_secs_f64() * 1_000.0,
            )?;
        }
        write!(formatter, ", {} missed intervals", self.missed_intervals)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{MIN_ESTIMATION_INTERVALS, PacingDiagnostics};

    const STEP: Duration = Duration::from_micros(16_667);

    fn diagnostics_after_steady_frames(count: usize) -> (PacingDiagnostics, Instant) {
        let mut diagnostics = PacingDiagnostics::new();
        let base = Instant::now();
        let mut at = base;
        for _ in 0..count {
            diagnostics.record_presented(at);
            at += STEP;
        }
        (diagnostics, at)
    }

    #[test]
    fn steady_frames_estimate_their_cadence() {
        let (diagnostics, _) = diagnostics_after_steady_frames(60);
        let report = diagnostics.report();
        assert_eq!(report.presented_frames, 60);
        assert_eq!(report.untimed_frames, 0);
        assert_eq!(report.estimated_cadence, Some(STEP));
        assert_eq!(report.missed_intervals, 0);
        let intervals = report.recent_intervals.unwrap();
        assert_eq!(intervals.samples, 59);
        assert_eq!(intervals.min, STEP);
        assert_eq!(intervals.max, STEP);
    }

    #[test]
    fn a_skipped_vsync_counts_as_missed_once_estimation_exists() {
        let (mut diagnostics, mut at) = diagnostics_after_steady_frames(30);
        at += STEP;
        diagnostics.record_presented(at);
        assert_eq!(diagnostics.report().missed_intervals, 1);
        assert_eq!(diagnostics.report().estimated_cadence, Some(STEP));
    }

    #[test]
    fn estimation_requires_enough_intervals() {
        let (diagnostics, _) = diagnostics_after_steady_frames(MIN_ESTIMATION_INTERVALS);
        assert_eq!(diagnostics.report().estimated_cadence, None);
        let (diagnostics, _) = diagnostics_after_steady_frames(MIN_ESTIMATION_INTERVALS + 1);
        assert!(diagnostics.report().estimated_cadence.is_some());
    }

    #[test]
    fn untimed_and_non_monotonic_frames_count_without_intervals() {
        let mut diagnostics = PacingDiagnostics::new();
        let base = Instant::now();
        diagnostics.record_presented(base);
        diagnostics.record_presented(base);
        diagnostics.record_untimed_presented();
        let report = diagnostics.report();
        assert_eq!(report.presented_frames, 3);
        assert_eq!(report.untimed_frames, 1);
        assert!(report.recent_intervals.is_none());
    }

    #[test]
    fn the_interval_window_is_bounded() {
        let mut diagnostics = PacingDiagnostics::with_window(4);
        let mut at = Instant::now();
        for _ in 0..20 {
            diagnostics.record_presented(at);
            at += STEP;
        }
        assert_eq!(diagnostics.report().recent_intervals.unwrap().samples, 4);
    }
}
