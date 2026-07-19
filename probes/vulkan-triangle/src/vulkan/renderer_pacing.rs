//! CPU-side presentation pacing estimation.
//!
//! The surveyed Windows Intel tier exposes none of the native presentation-feedback extensions
//! (`VK_KHR_present_id`/`present_wait`, `VK_GOOGLE_display_timing`), so this instrumentation
//! records the wall-clock return of each successful `vkQueuePresentKHR` under FIFO backpressure.
//! It is deliberately the estimation side of the Gate 4 pacing plan: what an application can
//! observe without native feedback, to be compared against tiers that report true present times.

use std::fmt::Write as _;
use std::path::PathBuf;

use super::{Duration, Instant, LoadSpike, ProbeError, Renderer, RunOptions, fs};

/// Intervals required before cadence estimation and missed-interval detection begin.
const MIN_ESTIMATION_INTERVALS: usize = 10;

pub(super) struct PresentPacing {
    csv_path: Option<PathBuf>,
    load_spike: Option<LoadSpike>,
    present_returns: Vec<Instant>,
    reported: bool,
}

impl PresentPacing {
    pub(super) fn new(options: &RunOptions) -> Self {
        Self {
            csv_path: options.pacing_csv.clone(),
            load_spike: options.load_spike,
            present_returns: Vec::new(),
            reported: false,
        }
    }

    pub(super) fn spike_sleep(&self) -> Option<Duration> {
        let spike = self.load_spike?;
        let frame = u64::try_from(self.present_returns.len()).ok()?;
        (frame >= spike.start && frame < spike.start + spike.count)
            .then(|| Duration::from_millis(spike.millis))
    }

    pub(super) fn record_present_return(&mut self) {
        self.present_returns.push(Instant::now());
    }

    /// Interval in milliseconds between consecutive present returns, tagged with the later
    /// frame's zero-based index.
    fn intervals_ms(&self) -> Vec<(u64, f64)> {
        self.present_returns
            .windows(2)
            .enumerate()
            .map(|(index, pair)| {
                (
                    u64::try_from(index + 1).unwrap_or(u64::MAX),
                    pair[1].duration_since(pair[0]).as_secs_f64() * 1_000.0,
                )
            })
            .collect()
    }
}

impl Renderer {
    pub(super) fn finish_present_pacing(&mut self) -> Result<(), ProbeError> {
        if self.present_pacing.reported {
            return Ok(());
        }
        self.present_pacing.reported = true;
        self.report_present_pacing();
        self.write_pacing_csv()
    }

    fn report_present_pacing(&self) {
        let pacing = &self.present_pacing;
        let intervals = pacing.intervals_ms();
        println!(
            "presentation pacing (CPU present-return estimation; no native feedback extension on \
             this tier): {} presents, {} intervals",
            pacing.present_returns.len(),
            intervals.len()
        );
        if intervals.is_empty() {
            return;
        }
        let in_spike = |frame: u64| {
            pacing
                .load_spike
                .is_some_and(|spike| frame >= spike.start && frame < spike.start + spike.count)
        };
        let mut steady: Vec<f64> = intervals
            .iter()
            .filter(|(frame, _)| !in_spike(*frame))
            .map(|(_, interval)| *interval)
            .collect();
        if let Some(summary) = distribution_summary(&mut steady) {
            let estimate =
                (steady.len() >= MIN_ESTIMATION_INTERVALS).then(|| percentile(&steady, 50));
            let missed = estimate.map_or_else(String::new, |estimate| {
                let count = steady
                    .iter()
                    .filter(|&&interval| interval > estimate * 1.5)
                    .count();
                format!(", estimated cadence {estimate:.3} ms, {count} missed (>1.5x estimate)")
            });
            println!(
                "present-return intervals (steady): n={}, {summary}{missed}",
                steady.len()
            );
        }
        if let Some(spike) = pacing.load_spike {
            let mut spiked: Vec<f64> = intervals
                .iter()
                .filter(|(frame, _)| in_spike(*frame))
                .map(|(_, interval)| *interval)
                .collect();
            if let Some(summary) = distribution_summary(&mut spiked) {
                println!(
                    "present-return intervals (load spike frames {}..{}, {} ms stall): n={}, {summary}",
                    spike.start,
                    spike.start + spike.count,
                    spike.millis,
                    spiked.len()
                );
            }
        }
    }

    fn write_pacing_csv(&self) -> Result<(), ProbeError> {
        let Some(path) = &self.present_pacing.csv_path else {
            return Ok(());
        };
        let returns = &self.present_pacing.present_returns;
        let mut csv = String::from("frame,present_return_offset_s,present_return_interval_ms\n");
        let first = returns.first().copied();
        let mut previous: Option<Instant> = None;
        for (frame, moment) in returns.iter().enumerate() {
            let offset = first.map_or(0.0, |first| moment.duration_since(first).as_secs_f64());
            let interval = previous.map_or_else(String::new, |previous| {
                format!(
                    "{:.6}",
                    moment.duration_since(previous).as_secs_f64() * 1_000.0
                )
            });
            writeln!(csv, "{frame},{offset:.6},{interval}")
                .expect("writing to a String cannot fail");
            previous = Some(*moment);
        }
        fs::write(path, csv).map_err(|error| {
            ProbeError(format!(
                "could not write pacing CSV {}: {error}",
                path.display()
            ))
        })
    }
}

/// Nearest-rank percentile over an ascending slice; callers guarantee non-emptiness.
fn percentile(sorted_values: &[f64], percent: usize) -> f64 {
    let rank = (percent * sorted_values.len()).div_ceil(100).max(1);
    sorted_values[rank - 1]
}

fn distribution_summary(values: &mut [f64]) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(format!(
        "min {:.3} ms, p50 {:.3} ms, p95 {:.3} ms, p99 {:.3} ms, max {:.3} ms",
        values[0],
        percentile(values, 50),
        percentile(values, 95),
        percentile(values, 99),
        values[values.len() - 1]
    ))
}
