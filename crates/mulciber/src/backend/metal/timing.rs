//! Optional stage-boundary counters, owned and reused with the frame slot.

use super::{
    objc::{self, Object},
    required,
};
use crate::{GpuScopeTiming, GpuTimingScope, GraphicsError, SHADOW_MAP_LAYER_LIMIT};
use core::{cell::Cell, ptr};
use std::{time::Duration, vec::Vec};

pub(super) const SCENE: usize = SHADOW_MAP_LAYER_LIMIT as usize;
pub(super) const FOREGROUND: usize = SCENE + 1;
pub(super) const POSTPROCESS: usize = FOREGROUND + 1;
const PASSES: usize = POSTPROCESS + 1;
const SAMPLES: usize = PASSES * 4;

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
    static MTLCommonCounterSetTimestamp: Object;
}

pub(super) struct CounterSet(Object);

impl CounterSet {
    pub(super) fn find(device: Object) -> Option<Self> {
        // SAFETY: All selectors exist on the macOS 13 baseline. Stage-boundary
        // sampling is queried independently of timestamp counter availability.
        unsafe {
            if !objc::bool_usize(device, c"supportsCounterSampling:", 0) {
                return None;
            }
            let sets = objc::object(device, c"counterSets");
            for index in 0..objc::usize_value(sets, c"count") {
                let set = objc::object_usize(sets, c"objectAtIndexedSubscript:", index);
                if objc::bool_object(
                    objc::object(set, c"name"),
                    c"isEqualToString:",
                    MTLCommonCounterSetTimestamp,
                ) {
                    objc::void(set, c"retain");
                    return Some(Self(set));
                }
            }
        }
        None
    }
}

impl Drop for CounterSet {
    fn drop(&mut self) {
        unsafe { objc::void(self.0, c"release") };
    }
}

#[derive(Clone, Copy, Default)]
struct ClockPair {
    cpu: u64,
    gpu: u64,
}

fn clock_pair(device: Object) -> ClockPair {
    let mut clocks = ClockPair::default();
    // SAFETY: The device writes both MTLTimestamp (uint64_t) outputs synchronously.
    unsafe {
        objc::void_two_u64_out(
            device,
            c"sampleTimestamps:gpuTimestamp:",
            &raw mut clocks.cpu,
            &raw mut clocks.gpu,
        );
    };
    clocks
}

pub(super) struct CounterSamples {
    buffer: Object,
    used: Cell<u16>,
    start: Cell<ClockPair>,
}

impl CounterSamples {
    pub(super) fn new(device: Object, set: &CounterSet) -> Result<Self, GraphicsError> {
        // SAFETY: The descriptor owns the counter set and the returned sample
        // buffer is an owned shared-storage object, released by Drop.
        let buffer = unsafe {
            let descriptor = required(
                objc::object(objc::class(c"MTLCounterSampleBufferDescriptor"), c"new"),
                "Metal counter descriptor",
            )?;
            objc::void_object(descriptor, c"setCounterSet:", set.0);
            objc::void_usize(descriptor, c"setStorageMode:", 0);
            objc::void_usize(descriptor, c"setSampleCount:", SAMPLES);
            let mut error = ptr::null_mut();
            let buffer = objc::object_object_out(
                device,
                c"newCounterSampleBufferWithDescriptor:error:",
                descriptor,
                &raw mut error,
            );
            objc::void(descriptor, c"release");
            if buffer.is_null() {
                return Err(GraphicsError::new(std::format!(
                    "Metal timestamp counter allocation: {}",
                    objc::description(error)
                )));
            }
            buffer
        };
        Ok(Self {
            buffer,
            used: Cell::new(0),
            start: Cell::new(ClockPair::default()),
        })
    }

    pub(super) fn begin(&self, device: Object) {
        self.used.set(0);
        self.start.set(clock_pair(device));
    }

    pub(super) fn attach(&self, descriptor: Object, pass: usize) {
        assert!(pass < PASSES);
        // SAFETY: The caller supplies a live render-pass descriptor before its
        // encoder is created. Each pass owns four distinct counter indices.
        unsafe {
            let attachments = objc::object(descriptor, c"sampleBufferAttachments");
            let attachment = objc::object_usize(attachments, c"objectAtIndexedSubscript:", 0);
            objc::void_object(attachment, c"setSampleBuffer:", self.buffer);
            for (offset, selector) in [
                c"setStartOfVertexSampleIndex:",
                c"setEndOfVertexSampleIndex:",
                c"setStartOfFragmentSampleIndex:",
                c"setEndOfFragmentSampleIndex:",
            ]
            .into_iter()
            .enumerate()
            {
                objc::void_usize(attachment, selector, pass * 4 + offset);
            }
        }
        self.used.set(self.used.get() | (1 << pass));
    }

    /// Resolves this frame's regions. `previous_frame_end` is the GPU tick the
    /// frame before finished on, and is advanced to this frame's last tick: the
    /// first pass of a frame starts its vertex stage under the previous frame's
    /// last fragment stage just as later passes do under their predecessor.
    pub(super) fn resolve(
        &self,
        device: Object,
        previous_frame_end: &mut Option<u64>,
    ) -> Vec<GpuScopeTiming> {
        if self.used.get() == 0 {
            return Vec::new();
        }
        let end = clock_pair(device);
        let start = self.start.get();
        let Some(cpu_span) = end.cpu.checked_sub(start.cpu).filter(|v| *v > 0) else {
            return Vec::new();
        };
        let Some(gpu_span) = end.gpu.checked_sub(start.gpu).filter(|v| *v > 0) else {
            return Vec::new();
        };
        // SAFETY: The frame's command buffer has completed; shared counter data
        // may now be resolved to NSData. The autorelease pool encloses the read.
        let _pool = objc::AutoreleasePool::new();
        let data = unsafe {
            objc::object_range(
                self.buffer,
                c"resolveCounterRange:",
                objc::Range {
                    location: 0,
                    length: SAMPLES,
                },
            )
        };
        if data.is_null()
            || unsafe { objc::usize_value(data, c"length") } < SAMPLES * size_of::<u64>()
        {
            return Vec::new();
        }
        let bytes = unsafe { objc::pointer_value(data, c"bytes") }.cast::<u64>();
        if bytes.is_null() {
            return Vec::new();
        }
        let mut regions = [Duration::ZERO; 3];
        let mut present = [false; 3];
        let mut stages = [[Duration::ZERO; 2]; 3];
        let mut invalid = [false; 3];
        let mut previous_end = *previous_frame_end;
        for pass in 0..PASSES {
            if self.used.get() & (1 << pass) == 0 {
                continue;
            }
            let samples = core::array::from_fn(|index| unsafe {
                bytes.add(pass * 4 + index).read_unaligned()
            });
            let group = if pass < SCENE {
                0
            } else if pass < POSTPROCESS {
                1
            } else {
                2
            };
            let Some((ticks, end)) = exclusive_pass_ticks(samples, previous_end) else {
                invalid[group] = true;
                continue;
            };
            previous_end = Some(end);
            *previous_frame_end = Some(end);
            let [vs, ve, fs, fe] = samples;
            stages[group][0] += ticks_duration(ve - vs, cpu_span, gpu_span).unwrap_or_default();
            stages[group][1] += ticks_duration(fe - fs, cpu_span, gpu_span).unwrap_or_default();
            regions[group] += ticks_duration(ticks, cpu_span, gpu_span).unwrap_or_default();
            present[group] = true;
        }
        [
            GpuTimingScope::Shadow,
            GpuTimingScope::Scene,
            GpuTimingScope::Postprocess,
        ]
        .into_iter()
        .enumerate()
        .filter_map(|(group, scope)| {
            if invalid[group] || !present[group] {
                return None;
            }
            Some(
                GpuScopeTiming::new(scope, regions[group])
                    .with_render_stages(stages[group][0], stages[group][1]),
            )
        })
        .collect()
    }
}

impl Drop for CounterSamples {
    fn drop(&mut self) {
        unsafe { objc::void(self.buffer, c"release") };
    }
}

/// Ticks a pass adds to the frame, and the tick it finished on.
///
/// Passes execute in order, so what a pass costs the frame is the time from
/// the previous pass finishing to this one finishing. A tile-based GPU runs
/// the vertex stage of one pass while the fragment stage of the one before is
/// still going, and the vertex interval then includes waiting on it; measured
/// on its own, a pass with almost no work can show the whole of its
/// predecessor's fragment time. Attributing by successive ends keeps the
/// regions adding up to the span they cover, and drops a pass's own vertex
/// overlap with the pass before it rather than counting that time twice.
fn exclusive_pass_ticks(samples: [u64; 4], previous_end: Option<u64>) -> Option<(u64, u64)> {
    if samples.iter().any(|&value| value == 0 || value == u64::MAX) {
        return None;
    }
    let [vs, ve, fs, fe] = samples;
    if ve < vs || fe < fs {
        return None;
    }
    let end = ve.max(fe);
    let start = vs.min(fs).max(previous_end.unwrap_or(0));
    Some((end.saturating_sub(start), end))
}

#[allow(clippy::cast_precision_loss)]
fn ticks_duration(ticks: u64, cpu_span: u64, gpu_span: u64) -> Option<Duration> {
    Duration::try_from_secs_f64(ticks as f64 / gpu_span as f64 * cpu_span as f64 / 1e9).ok()
}

#[cfg(test)]
mod tests {
    use super::exclusive_pass_ticks;

    #[test]
    fn a_pass_is_measured_from_the_previous_pass_finishing() {
        // The second pass's vertex stage started under the first pass's
        // fragment stage and waited there: 12..70 is mostly the first pass.
        let first = [2, 10, 10, 50];
        let second = [12, 70, 70, 80];
        assert_eq!(exclusive_pass_ticks(first, None), Some((48, 50)));
        assert_eq!(exclusive_pass_ticks(second, Some(50)), Some((30, 80)));
    }

    #[test]
    fn overlapping_stages_of_one_pass_use_their_span() {
        assert_eq!(
            exclusive_pass_ticks([100, 160, 140, 220], None),
            Some((120, 220))
        );
        assert_eq!(
            exclusive_pass_ticks([100, 160, 140, 220], Some(180)),
            Some((40, 220))
        );
    }

    #[test]
    fn unresolved_or_reversed_samples_are_rejected() {
        assert_eq!(exclusive_pass_ticks([100, 160, 140, u64::MAX], None), None);
        assert_eq!(exclusive_pass_ticks([100, 99, 140, 220], None), None);
        assert_eq!(exclusive_pass_ticks([0, 99, 140, 220], None), None);
    }
}
