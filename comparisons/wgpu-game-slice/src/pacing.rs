//! Best-effort presentation pacing estimation.
//!
//! Pinned wgpu 30.0.0 and winit 0.30.13 expose no presented-time feedback, so the closest this
//! application can get to a presented cadence is timestamping the return of its own present calls
//! and summarizing those intervals. The runtime-backed Mulciber peer consumes backend-reported
//! display times through the same summary vocabulary; this module is the estimation cost that the
//! Gate 4 pacing plan records for the portable baseline.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

/// Recent present-return intervals retained for the summary.
const INTERVAL_WINDOW: usize = 240;
/// Intervals required before cadence estimation and missed-interval detection begin.
const MIN_ESTIMATION_INTERVALS: usize = 10;
/// An interval longer than the estimated cadence by this factor counts as missed.
const MISSED_INTERVAL_FACTOR: f64 = 1.5;

#[derive(Default)]
pub(crate) struct PacingEstimate {
    intervals: VecDeque<Duration>,
    last_present_return: Option<Instant>,
    presented_frames: u64,
    missed_intervals: u64,
}

impl PacingEstimate {
    pub(crate) fn record_present_return(&mut self, returned_at: Instant) {
        self.presented_frames += 1;
        if let Some(previous) = self.last_present_return {
            let interval = returned_at.saturating_duration_since(previous);
            if !interval.is_zero() {
                if let Some(cadence) = self.estimated_cadence()
                    && interval.as_secs_f64() > cadence.as_secs_f64() * MISSED_INTERVAL_FACTOR
                {
                    self.missed_intervals += 1;
                }
                if self.intervals.len() >= INTERVAL_WINDOW {
                    self.intervals.pop_front();
                }
                self.intervals.push_back(interval);
            }
        }
        self.last_present_return = Some(returned_at);
    }

    /// Median of the retained intervals once enough exist to estimate responsibly.
    fn estimated_cadence(&self) -> Option<Duration> {
        if self.intervals.len() < MIN_ESTIMATION_INTERVALS {
            return None;
        }
        let mut sorted: Vec<Duration> = self.intervals.iter().copied().collect();
        sorted.sort_unstable();
        Some(sorted[(sorted.len() - 1) / 2])
    }

    pub(crate) fn report(&self) -> String {
        let mut report = format!("{} presented frames", self.presented_frames);
        if let Some(cadence) = self.estimated_cadence() {
            write_millis(&mut report, ", estimated cadence", cadence);
        }
        let mut sorted: Vec<Duration> = self.intervals.iter().copied().collect();
        sorted.sort_unstable();
        if !sorted.is_empty() {
            let _ = write!(report, ", recent intervals (n={})", sorted.len());
            write_millis(&mut report, " min", sorted[0]);
            write_millis(&mut report, ", median", sorted[(sorted.len() - 1) / 2]);
            write_millis(
                &mut report,
                ", p95",
                sorted[percentile_rank(sorted.len(), 95)],
            );
            write_millis(&mut report, ", max", sorted[sorted.len() - 1]);
        }
        let _ = write!(report, ", {} missed intervals", self.missed_intervals);
        report
    }
}

fn write_millis(report: &mut String, label: &str, value: Duration) {
    let _ = write!(report, "{label} {:.3} ms", value.as_secs_f64() * 1_000.0);
}

/// Nearest-rank index for a percentile over `length` ascending samples.
fn percentile_rank(length: usize, percent: usize) -> usize {
    ((percent * length).div_ceil(100)).max(1) - 1
}
