//! Presentation pacing instrumentation: CPU present-return estimation beside native feedback.
//!
//! The CPU side records the wall-clock return of each successful `vkQueuePresentKHR` under FIFO
//! backpressure — what an application can observe without native feedback. When the surveyed tier
//! exposes the `VK_EXT_present_timing` chain, the probe additionally drains identified native
//! present-stage timestamps, so the Gate 4 pacing plan can compare the two paths on the same run.
//! Tiers without the chain (such as the surveyed Windows Intel tier) keep the estimation-only
//! report with the observable reason.

use std::fmt::Write as _;
use std::path::PathBuf;

use super::{
    Duration, Instant, LoadSpike, PresentTimingSelection, ProbeError, Renderer, RunOptions, fs,
    present_stage_name,
};

/// Intervals required before cadence estimation and missed-interval detection begin.
const MIN_ESTIMATION_INTERVALS: usize = 10;

/// One drained native present-stage timestamp.
#[derive(Clone, Copy)]
struct NativeSample {
    frame: u64,
    /// Present-stage time in the units of the owning swapchain's time domain.
    time: u64,
    /// Swapchain generation whose time-domain epoch this time belongs to. Each swapchain owns its
    /// own domain instance, so intervals must never pair times across generations.
    generation: u32,
}

/// Native presented-time feedback collected beside the estimation, or the reason it is absent.
enum NativeTiming {
    Estimation {
        reason: String,
    },
    Active {
        stage_label: &'static str,
        domain_label: Option<&'static str>,
        refresh_duration_ns: Option<u64>,
        times: Vec<NativeSample>,
        generation: u32,
    },
}

pub(super) struct PresentPacing {
    csv_path: Option<PathBuf>,
    load_spike: Option<LoadSpike>,
    present_returns: Vec<Instant>,
    native: NativeTiming,
    reported: bool,
}

impl PresentPacing {
    pub(super) fn new(
        options: &RunOptions,
        selection: Result<PresentTimingSelection, &'static str>,
    ) -> Self {
        Self {
            csv_path: options.pacing_csv.clone(),
            load_spike: options.load_spike,
            present_returns: Vec::new(),
            native: match selection {
                Ok(selection) => NativeTiming::Active {
                    stage_label: present_stage_name(selection.stage),
                    domain_label: None,
                    refresh_duration_ns: None,
                    times: Vec::new(),
                    generation: 0,
                },
                Err(reason) => NativeTiming::Estimation {
                    reason: reason.into(),
                },
            },
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

    /// Successful presents recorded so far; the latest present's frame index is one less.
    pub(super) fn presents_recorded(&self) -> u64 {
        u64::try_from(self.present_returns.len()).unwrap_or(u64::MAX)
    }

    /// Records the swapchain-reported refresh duration; a zero report is treated as unknown so
    /// missed-interval detection falls back to the measured median.
    pub(super) fn record_refresh_duration(&mut self, duration_ns: u64) {
        if let NativeTiming::Active {
            refresh_duration_ns,
            ..
        } = &mut self.native
            && duration_ns != 0
        {
            *refresh_duration_ns = Some(duration_ns);
        }
    }

    /// Records the freshly configured swapchain's time domain and starts a new epoch generation,
    /// because domain instances (and therefore epochs) are swapchain-scoped.
    pub(super) fn record_native_domain(&mut self, label: &'static str) {
        if let NativeTiming::Active {
            domain_label,
            generation,
            ..
        } = &mut self.native
        {
            *domain_label = Some(label);
            *generation += 1;
        }
    }

    /// Downgrades the native path to estimation-only with an observable reason.
    pub(super) fn record_native_inactive(&mut self, reason: &str) {
        println!("Present timing: falling back to CPU estimation; {reason}");
        self.native = NativeTiming::Estimation {
            reason: reason.into(),
        };
    }

    pub(super) fn record_native_time(&mut self, frame: u64, time: u64) {
        if let NativeTiming::Active {
            times, generation, ..
        } = &mut self.native
        {
            times.push(NativeSample {
                frame,
                time,
                generation: *generation,
            });
        }
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

    fn in_spike(&self, frame: u64) -> bool {
        self.load_spike
            .is_some_and(|spike| frame >= spike.start && frame < spike.start + spike.count)
    }
}

/// Native samples ordered by frame index.
fn sorted_native_times(times: &[NativeSample]) -> Vec<NativeSample> {
    let mut sorted = times.to_vec();
    sorted.sort_unstable_by_key(|sample| sample.frame);
    sorted
}

/// Intervals between native times of directly consecutive frames within one swapchain
/// generation, in milliseconds, tagged with the later frame. A frame without a native time
/// breaks the pair on both sides, and a swapchain recreation breaks the pair because each
/// swapchain's time domain has its own epoch.
#[allow(clippy::cast_precision_loss)]
fn native_intervals_ms(sorted: &[NativeSample]) -> Vec<(u64, f64)> {
    sorted
        .windows(2)
        .filter(|pair| {
            pair[1].frame == pair[0].frame + 1
                && pair[1].generation == pair[0].generation
                && pair[1].time > pair[0].time
        })
        .map(|pair| {
            (
                pair[1].frame,
                (pair[1].time - pair[0].time) as f64 / 1_000_000.0,
            )
        })
        .collect()
}

impl Renderer {
    pub(super) fn finish_present_pacing(&mut self) -> Result<(), ProbeError> {
        if self.present_pacing.reported {
            return Ok(());
        }
        // Salvage reports that completed after the last presented frame drained.
        self.drain_present_timing()?;
        self.present_pacing.reported = true;
        self.report_present_pacing();
        self.write_pacing_csv()
    }

    #[allow(clippy::cast_precision_loss)]
    fn report_present_pacing(&self) {
        let pacing = &self.present_pacing;
        let intervals = pacing.intervals_ms();
        match &pacing.native {
            NativeTiming::Estimation { reason } => {
                println!(
                    "presentation pacing (CPU present-return estimation; {reason}): {} presents, \
                     {} intervals",
                    pacing.present_returns.len(),
                    intervals.len()
                );
            }
            NativeTiming::Active {
                stage_label,
                domain_label,
                refresh_duration_ns,
                times,
                generation: _,
            } => {
                println!(
                    "presentation pacing: {} presents, {} with native present times, {} untimed",
                    pacing.present_returns.len(),
                    times.len(),
                    pacing.present_returns.len().saturating_sub(times.len())
                );
                let refresh = refresh_duration_ns.map_or_else(String::new, |duration| {
                    format!(", refresh duration {:.3} ms", duration as f64 / 1_000_000.0)
                });
                println!(
                    "native present timing: VK_EXT_present_timing, {stage_label}, {}{refresh}",
                    domain_label.unwrap_or("unreported time domain"),
                );
                self.report_native_intervals(times, *refresh_duration_ns);
            }
        }
        self.report_return_intervals(&intervals);
    }

    #[allow(clippy::cast_precision_loss)]
    fn report_native_intervals(&self, times: &[NativeSample], refresh_duration_ns: Option<u64>) {
        let pacing = &self.present_pacing;
        let sorted = sorted_native_times(times);
        let native_intervals = native_intervals_ms(&sorted);
        let mut steady: Vec<f64> = native_intervals
            .iter()
            .filter(|(frame, _)| !pacing.in_spike(*frame))
            .map(|(_, interval)| *interval)
            .collect();
        if let Some(summary) = distribution_summary(&mut steady) {
            let threshold = refresh_duration_ns
                .map(|duration| duration as f64 / 1_000_000.0)
                .or_else(|| {
                    (steady.len() >= MIN_ESTIMATION_INTERVALS).then(|| percentile(&steady, 50))
                });
            let missed = threshold.map_or_else(String::new, |threshold| {
                let count = steady
                    .iter()
                    .filter(|&&interval| interval > threshold * 1.5)
                    .count();
                format!(", {count} missed (>1.5x {threshold:.3} ms)")
            });
            println!(
                "native present-time intervals (steady): n={}, {summary}{missed}",
                steady.len()
            );
        }
        if let Some(spike) = pacing.load_spike {
            let mut spiked: Vec<f64> = native_intervals
                .iter()
                .filter(|(frame, _)| pacing.in_spike(*frame))
                .map(|(_, interval)| *interval)
                .collect();
            if let Some(summary) = distribution_summary(&mut spiked) {
                println!(
                    "native present-time intervals (load spike frames {}..{}, {} ms stall): \
                     n={}, {summary}",
                    spike.start,
                    spike.start + spike.count,
                    spike.millis,
                    spiked.len()
                );
            }
        }
    }

    fn report_return_intervals(&self, intervals: &[(u64, f64)]) {
        let pacing = &self.present_pacing;
        if intervals.is_empty() {
            return;
        }
        let mut steady: Vec<f64> = intervals
            .iter()
            .filter(|(frame, _)| !pacing.in_spike(*frame))
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
                .filter(|(frame, _)| pacing.in_spike(*frame))
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

    #[allow(clippy::cast_precision_loss)]
    fn write_pacing_csv(&self) -> Result<(), ProbeError> {
        let Some(path) = &self.present_pacing.csv_path else {
            return Ok(());
        };
        let returns = &self.present_pacing.present_returns;
        let native_sorted = match &self.present_pacing.native {
            NativeTiming::Active { times, .. } => sorted_native_times(times),
            NativeTiming::Estimation { .. } => Vec::new(),
        };
        let native_sample = |frame: u64| -> Option<NativeSample> {
            native_sorted
                .binary_search_by_key(&frame, |sample| sample.frame)
                .ok()
                .map(|index| native_sorted[index])
        };
        // Offsets restart at each swapchain generation because every swapchain's time domain has
        // its own epoch.
        let generation_base = |generation: u32| -> Option<u64> {
            native_sorted
                .iter()
                .find(|sample| sample.generation == generation)
                .map(|sample| sample.time)
        };
        let mut csv = String::from(
            "frame,present_return_offset_s,present_return_interval_ms,\
             native_present_time_offset_s,native_present_interval_ms\n",
        );
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
            let frame_index = u64::try_from(frame).unwrap_or(u64::MAX);
            let native_offset = native_sample(frame_index)
                .and_then(|sample| Some((sample, generation_base(sample.generation)?)))
                .map_or_else(String::new, |(sample, base)| {
                    format!(
                        "{:.6}",
                        sample.time.saturating_sub(base) as f64 / 1_000_000_000.0
                    )
                });
            let native_interval = frame_index
                .checked_sub(1)
                .and_then(|previous_frame| {
                    native_sample(frame_index).zip(native_sample(previous_frame))
                })
                .filter(|(sample, previous)| {
                    sample.generation == previous.generation && sample.time > previous.time
                })
                .map_or_else(String::new, |(sample, previous)| {
                    format!("{:.6}", (sample.time - previous.time) as f64 / 1_000_000.0)
                });
            writeln!(
                csv,
                "{frame},{offset:.6},{interval},{native_offset},{native_interval}"
            )
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
