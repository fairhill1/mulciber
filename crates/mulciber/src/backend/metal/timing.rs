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

    pub(super) fn resolve(&self, device: Object) -> Vec<GpuScopeTiming> {
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
        let mut bounds: [Option<[u64; 4]>; 3] = [None; 3];
        let mut stages = [[Duration::ZERO; 2]; 3];
        let mut invalid = [false; 3];
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
            if pass_duration(samples, cpu_span, gpu_span).is_none() {
                invalid[group] = true;
                continue;
            }
            let [vs, ve, fs, fe] = samples;
            stages[group][0] += ticks_duration(ve - vs, cpu_span, gpu_span).unwrap_or_default();
            stages[group][1] += ticks_duration(fe - fs, cpu_span, gpu_span).unwrap_or_default();
            bounds[group] = Some(match bounds[group] {
                Some(previous) => [
                    previous[0].min(vs),
                    previous[1].max(ve),
                    previous[2].min(fs),
                    previous[3].max(fe),
                ],
                None => samples,
            });
        }
        [
            GpuTimingScope::Shadow,
            GpuTimingScope::Scene,
            GpuTimingScope::Postprocess,
        ]
        .into_iter()
        .enumerate()
        .filter_map(|(group, scope)| {
            if invalid[group] {
                return None;
            }
            let duration = pass_duration(bounds[group]?, cpu_span, gpu_span)?;
            Some(
                GpuScopeTiming::new(scope, duration)
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

/// A render pass may overlap its vertex and fragment stages. Measure the span,
/// not their sum; convert GPU ticks with paired clocks (CPU timestamps are ns).
#[allow(clippy::cast_precision_loss)]
fn pass_duration(samples: [u64; 4], cpu_span: u64, gpu_span: u64) -> Option<Duration> {
    if gpu_span == 0 || samples.iter().any(|&value| value == 0 || value == u64::MAX) {
        return None;
    }
    let [vs, ve, fs, fe] = samples;
    if ve < vs || fe < fs {
        return None;
    }
    let ticks = ve.max(fe).checked_sub(vs.min(fs))?;
    ticks_duration(ticks, cpu_span, gpu_span)
}

#[allow(clippy::cast_precision_loss)]
fn ticks_duration(ticks: u64, cpu_span: u64, gpu_span: u64) -> Option<Duration> {
    Duration::try_from_secs_f64(ticks as f64 / gpu_span as f64 * cpu_span as f64 / 1e9).ok()
}

#[cfg(test)]
mod tests {
    use super::pass_duration;
    use std::time::Duration;
    #[test]
    fn overlapping_stages_use_a_span_and_calibrated_clock() {
        assert_eq!(
            pass_duration([100, 160, 140, 220], 2000, 1000),
            Some(Duration::from_nanos(240))
        );
        assert_eq!(pass_duration([100, 160, 140, u64::MAX], 2000, 1000), None);
        assert_eq!(pass_duration([100, 99, 140, 220], 2000, 1000), None);
        assert_eq!(pass_duration([100, 160, 140, 220], 2000, 0), None);
    }
}
