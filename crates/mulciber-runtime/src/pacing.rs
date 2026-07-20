//! Presentation pacing diagnostics and scheduling policy.
//!
//! This module holds both halves of the pacing vocabulary. [`PacingDiagnostics`] observes the
//! presented cadence a graphics backend reports and summarizes it. [`FramePacer`] consumes the
//! same presented timestamps and schedules frame work onto the observed presentation grid, so
//! simulation advances by whole display intervals instead of by jittery wall-clock gaps between
//! render starts. Timestamps arrive as plain [`Instant`]s so both halves stay independent of any
//! particular graphics crate or feedback mechanism, including estimated timestamps where native
//! feedback is absent.

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

/// Presented feedback older than this no longer anchors the presentation grid; scheduling falls
/// back to wall-clock timing until fresh feedback arrives, such as after occlusion or resume.
const PACING_STALENESS_LIMIT: Duration = Duration::from_millis(250);

/// Derives display-cadence frame deltas from presented-frame feedback.
///
/// Feed every presented frame through [`Self::record_presented`] (or
/// [`Self::record_untimed_presented`] when presentation completed without a display time), then
/// ask [`Self::schedule`] when a frame is about to be built. While a cadence estimate and fresh
/// feedback exist, each frame delta is a whole number of display intervals: one interval
/// normally, more when the wall-clock gap since the previous schedule shows the display consumed
/// extra intervals. Without an estimate, or when feedback goes stale, deltas observably fall back
/// to raw wall-clock gaps.
///
/// Deltas quantize to the cadence instead of following the wall clock because a FIFO-presented
/// backend displays exactly one frame per display interval even when frame building starts at
/// irregular times; physically measured on the Wayland/KWin tier, presented intervals stayed
/// within ±0.4 ms of the cadence while build starts jittered by ±7 ms, so wall-clock deltas
/// animate that jitter onto a steady display. The same measurements showed presented feedback
/// arrives about two frames late with drain-latency bias in its absolute placement, so this
/// policy deliberately owns no absolute frame-start scheduling: an earlier revision that slept
/// toward grid instants extrapolated from feedback timestamps paired whole-interval delta errors
/// on the same tier.
#[derive(Debug)]
pub struct FramePacer {
    diagnostics: PacingDiagnostics,
    last_presented: Option<Instant>,
    last_schedule_at: Option<Instant>,
}

impl Default for FramePacer {
    fn default() -> Self {
        Self::new()
    }
}

impl FramePacer {
    /// Creates a pacer with no observed presentation feedback.
    #[must_use]
    pub fn new() -> Self {
        Self {
            diagnostics: PacingDiagnostics::new(),
            last_presented: None,
            last_schedule_at: None,
        }
    }

    /// Records one presented frame with the display time the backend reported for it.
    pub fn record_presented(&mut self, presented_at: Instant) {
        self.diagnostics.record_presented(presented_at);
        self.last_presented = Some(match self.last_presented {
            Some(previous) => previous.max(presented_at),
            None => presented_at,
        });
    }

    /// Records one frame whose presentation completed without a reported display time.
    pub fn record_untimed_presented(&mut self) {
        self.diagnostics.record_untimed_presented();
    }

    /// Summarizes everything the underlying diagnostics recorded so far.
    #[must_use]
    pub fn report(&self) -> PacingReport {
        self.diagnostics.report()
    }

    /// Schedules the frame about to be built, given the current wall-clock instant.
    pub fn schedule(&mut self, now: Instant) -> FrameSchedule {
        let elapsed = self.last_schedule_at.map_or(Duration::ZERO, |previous| {
            now.saturating_duration_since(previous)
        });
        self.last_schedule_at = Some(now);
        match self.display_intervals(now, elapsed) {
            Some(frame_delta) => FrameSchedule {
                frame_delta,
                paced: true,
            },
            None => FrameSchedule {
                frame_delta: elapsed,
                paced: false,
            },
        }
    }

    /// Quantizes `elapsed` to whole display intervals, or `None` when cadence or fresh feedback
    /// is missing.
    fn display_intervals(&self, now: Instant, elapsed: Duration) -> Option<Duration> {
        let cadence = self.diagnostics.estimated_cadence()?;
        let presented = self.last_presented?;
        if now.saturating_duration_since(presented) > PACING_STALENESS_LIMIT {
            return None;
        }
        // Whole interval count with a one-interval floor and a quarter-interval of slack: the
        // display consumed at least one interval no matter how quickly this schedule followed
        // the previous one, and measured build-start spikes reach 1.7 intervals on frames that
        // still made their display slot while genuinely missed slots start at two intervals, so
        // gaps count an extra interval only past one and three-quarters.
        let slack = cadence / 4;
        let intervals = (elapsed + slack).as_nanos() / cadence.as_nanos();
        u32::try_from(intervals.max(1))
            .ok()
            .map(|intervals| cadence * intervals)
    }
}

/// One frame's pacing decision: how much time the frame advances and how that was derived.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "a schedule carries the frame delta the simulation should consume"]
pub struct FrameSchedule {
    frame_delta: Duration,
    paced: bool,
}

impl FrameSchedule {
    /// Returns how much time this frame advances relative to the previous schedule.
    #[must_use]
    pub const fn frame_delta(self) -> Duration {
        self.frame_delta
    }

    /// Returns whether the delta is a whole number of observed display intervals rather than a
    /// wall-clock fallback.
    #[must_use]
    pub const fn paced(self) -> bool {
        self.paced
    }
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

    use super::{FramePacer, MIN_ESTIMATION_INTERVALS, PACING_STALENESS_LIMIT, PacingDiagnostics};

    const STEP: Duration = Duration::from_micros(16_667);

    /// Feeds `count` steady presents and returns the pacer with the last presented instant.
    fn pacer_after_steady_presents(count: u32) -> (FramePacer, Instant) {
        let mut pacer = FramePacer::new();
        let base = Instant::now();
        let mut at = base;
        for _ in 1..count {
            pacer.record_presented(at);
            at += STEP;
        }
        pacer.record_presented(at);
        (pacer, at)
    }

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
    fn jittered_schedule_gaps_still_advance_one_display_interval() {
        // The measured pathology: build starts alternately arrive ~3 ms and ~18 ms apart while
        // the display consumes one frame per ~13.3 ms interval. Deltas must stay one interval.
        let (mut pacer, mut presented) = pacer_after_steady_presents(30);
        let mut now = presented + Duration::from_millis(1);
        for jitter_ms in [3_u64, 18, 2, 17, 4, 16] {
            let schedule = pacer.schedule(now);
            assert!(schedule.paced());
            assert_eq!(schedule.frame_delta(), STEP, "jitter {jitter_ms} ms");
            presented += STEP;
            pacer.record_presented(presented);
            now += Duration::from_millis(jitter_ms);
        }
    }

    #[test]
    fn a_late_schedule_advances_by_whole_missed_intervals() {
        let (mut pacer, last_presented) = pacer_after_steady_presents(30);
        let first_at = last_presented + Duration::from_millis(1);
        let _ = pacer.schedule(first_at);
        let second = pacer.schedule(first_at + STEP * 2 + Duration::from_millis(2));
        assert!(second.paced());
        assert_eq!(second.frame_delta(), STEP * 2);
    }

    #[test]
    fn a_build_start_spike_short_of_two_intervals_stays_one_interval() {
        let (mut pacer, last_presented) = pacer_after_steady_presents(30);
        let first_at = last_presented + Duration::from_millis(1);
        let _ = pacer.schedule(first_at);
        let spike = pacer.schedule(first_at + STEP + STEP * 7 / 10);
        assert!(spike.paced());
        assert_eq!(spike.frame_delta(), STEP);
    }

    #[test]
    fn back_to_back_schedules_keep_the_one_interval_floor() {
        let (mut pacer, last_presented) = pacer_after_steady_presents(30);
        let now = last_presented + Duration::from_millis(1);
        let first = pacer.schedule(now);
        let second = pacer.schedule(now + Duration::from_millis(1));
        assert_eq!(first.frame_delta(), STEP);
        assert_eq!(second.frame_delta(), STEP);
    }

    #[test]
    fn missing_and_stale_feedback_fall_back_to_wall_clock() {
        let mut pacer = FramePacer::new();
        let base = Instant::now();
        let unestimated = pacer.schedule(base);
        assert!(!unestimated.paced());
        assert_eq!(unestimated.frame_delta(), Duration::ZERO);
        let next = pacer.schedule(base + Duration::from_millis(7));
        assert!(!next.paced());
        assert_eq!(next.frame_delta(), Duration::from_millis(7));

        let (mut pacer, last_presented) = pacer_after_steady_presents(30);
        let _ = pacer.schedule(last_presented + Duration::from_millis(1));
        let stale_at = last_presented + Duration::from_millis(1) + PACING_STALENESS_LIMIT + STEP;
        let stale = pacer.schedule(stale_at);
        assert!(!stale.paced());
        assert_eq!(stale.frame_delta(), PACING_STALENESS_LIMIT + STEP);
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
