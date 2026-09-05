#[allow(missing_docs)]
pub mod objc;
mod timing;

use core::ffi::c_void;
use core::marker::PhantomData;
use core::ptr;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use std::vec::Vec;

use mulciber_platform::{SurfaceTarget, WindowMetrics, integration};

use crate::{
    ClearColor, FrameAcquire, FrameDisposition, GpuFrameTiming, GpuScopeTiming, GpuTimingFeedback,
    GpuTimingScope, GraphicsError, GraphicsErrorKind, PresentFeedback, PresentedFrame,
    SurfaceExtent, SurfaceInfo, SurfaceUnavailable,
};

pub(crate) const BACKEND_NAME: &str = "Metal";

use objc::{AutoreleasePool, Object, Size};

const PIXEL_FORMAT_BGRA8_UNORM_SRGB: usize = 81;
// Match the drawable pool. A slot is reused only after its command buffer completes.
const FRAMES_IN_FLIGHT: usize = 3;

#[derive(Default)]
struct FrameSlot {
    command_buffer: Object,
    timing_index: Option<u64>,
    counters: Option<timing::CounterSamples>,
}

const LOAD_ACTION_CLEAR: usize = 2;
const STORE_ACTION_STORE: usize = 1;

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
    fn MTLCreateSystemDefaultDevice() -> Object;
}

#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {
    fn CACurrentMediaTime() -> f64;
}

#[link(name = "System")]
unsafe extern "C" {
    #[allow(non_upper_case_globals)]
    static _NSConcreteGlobalBlock: c_void;
}

// `Block_literal_1` and `Block_descriptor_1` from the Clang Blocks ABI specification. The
// presented handler captures nothing, so one global block with no copy/dispose helpers serves
// every drawable registration and `Block_copy` returns it unchanged.
#[repr(C)]
struct BlockDescriptor {
    reserved: usize,
    size: usize,
}

#[repr(C)]
struct BlockLiteral {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut BlockLiteral, Object),
    descriptor: *const BlockDescriptor,
}

const BLOCK_IS_GLOBAL: i32 = 1 << 28;

static PRESENTED_HANDLER_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
    reserved: 0,
    size: size_of::<BlockLiteral>(),
};

fn presented_handler_block() -> Object {
    static BLOCK_ADDRESS: OnceLock<usize> = OnceLock::new();
    let address = *BLOCK_ADDRESS.get_or_init(|| {
        // The captureless literal is intentionally leaked once for process lifetime.
        let block: *mut BlockLiteral = std::boxed::Box::leak(std::boxed::Box::new(BlockLiteral {
            isa: &raw const _NSConcreteGlobalBlock,
            flags: BLOCK_IS_GLOBAL,
            reserved: 0,
            invoke: presented_handler,
            descriptor: &raw const PRESENTED_HANDLER_DESCRIPTOR,
        }));
        block as usize
    });
    address as Object
}

/// Bounds the undrained event and pending-submission queues so a consumer that never drains
/// feedback pays a fixed cost.
const PRESENT_FEEDBACK_CAP: usize = 1024;

#[derive(Clone, Copy)]
struct PresentedEvent {
    layer: usize,
    drawable_id: usize,
    presented_time: f64,
}

/// Presented handlers run on a Core Animation thread; surfaces drain their own layer's events on
/// the main thread and correlate them to submissions by drawable ID.
static PRESENTED_EVENTS: Mutex<Vec<PresentedEvent>> = Mutex::new(Vec::new());

unsafe extern "C" fn presented_handler(_block: *mut BlockLiteral, drawable: Object) {
    // SAFETY: Core Animation invokes the handler with a live drawable; `layer`, `drawableID`, and
    // `presentedTime` are read-only queries valid inside a presented handler on the macOS 13
    // baseline.
    let layer = unsafe { objc::object(drawable, c"layer") } as usize;
    let drawable_id = unsafe { objc::usize_value(drawable, c"drawableID") };
    let presented_time = unsafe { objc::f64_value(drawable, c"presentedTime") };
    if let Ok(mut events) = PRESENTED_EVENTS.lock() {
        if events.len() >= PRESENT_FEEDBACK_CAP {
            events.remove(0);
        }
        events.push(PresentedEvent {
            layer,
            drawable_id,
            presented_time,
        });
    }
}

struct PendingPresent {
    index: u64,
    drawable_id: usize,
}

pub(crate) struct ClearSurface<'window> {
    view: Object,
    device: Object,
    layer: Object,
    queue: Object,
    info: SurfaceInfo,
    frames: [FrameSlot; FRAMES_IN_FLIGHT],
    /// Next slot to acquire; advances only after an actual submission.
    frame_slot: usize,
    counter_set: Option<timing::CounterSet>,
    gpu_timing_enabled: bool,
    gpu_timings: VecDeque<GpuFrameTiming>,
    pending_presents: VecDeque<PendingPresent>,
    presented_count: u64,
    _window: PhantomData<SurfaceTarget<'window>>,
}

impl<'window> ClearSurface<'window> {
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn new(
        target: SurfaceTarget<'window>,
        metrics: WindowMetrics,
    ) -> Result<Self, GraphicsError> {
        let extent = surface_extent(metrics)?;
        let info = SurfaceInfo::initial(extent).ok_or_else(|| {
            GraphicsError::invalid_request("Metal surface requires non-empty initial metrics")
        })?;
        let _pool = AutoreleasePool::new();
        // SAFETY: The target remains borrowed for this surface's lifetime and all selectors match
        // their AppKit, QuartzCore, and Metal SDK ABIs on the process main thread.
        unsafe {
            let view = integration::appkit_view(&target).as_ptr();
            let device = MTLCreateSystemDefaultDevice();
            if device.is_null() {
                return Err(GraphicsError::with_kind(
                    GraphicsErrorKind::Unsupported,
                    "no default Metal device is available",
                ));
            }
            objc::void(device, c"retain");

            let layer = objc::object(objc::class(c"CAMetalLayer"), c"new");
            if layer.is_null() {
                objc::void(device, c"release");
                return Err(GraphicsError::new(
                    "create CAMetalLayer: object is unavailable",
                ));
            }
            objc::void_object(layer, c"setDevice:", device);
            objc::void_usize(layer, c"setPixelFormat:", PIXEL_FORMAT_BGRA8_UNORM_SRGB);
            objc::void_bool(layer, c"setFramebufferOnly:", true);
            objc::void_usize(layer, c"setMaximumDrawableCount:", FRAMES_IN_FLIGHT);
            objc::void_bool(layer, c"setDisplaySyncEnabled:", true);
            objc::void_bool(layer, c"setAllowsNextDrawableTimeout:", true);
            configure_layer(layer, metrics);

            let queue = objc::object(device, c"newCommandQueue");
            if queue.is_null() {
                objc::void(layer, c"release");
                objc::void(device, c"release");
                return Err(GraphicsError::new(
                    "create Metal command queue: object is unavailable",
                ));
            }
            set_label(queue, c"Mulciber clear queue");
            objc::void_object(view, c"setLayer:", layer);

            Ok(Self {
                view,
                device,
                layer,
                queue,
                info,
                frames: core::array::from_fn(|_| FrameSlot::default()),
                frame_slot: 0,
                counter_set: timing::CounterSet::find(device),
                gpu_timing_enabled: false,
                gpu_timings: VecDeque::new(),
                pending_presents: VecDeque::new(),
                presented_count: 0,
                _window: PhantomData,
            })
        }
    }

    pub(crate) const fn info(&self) -> SurfaceInfo {
        self.info
    }

    pub(crate) fn enable_gpu_timing(&mut self, enabled: bool) -> Result<(), GraphicsError> {
        if enabled
            && !self.gpu_timing_enabled
            && let Some(set) = &self.counter_set
        {
            let counters = (0..FRAMES_IN_FLIGHT)
                .map(|_| timing::CounterSamples::new(self.device, set))
                .collect::<Result<Vec<_>, _>>()?;
            for (frame, counters) in self.frames.iter_mut().zip(counters) {
                frame.counters = Some(counters);
            }
        }
        if self.gpu_timing_enabled != enabled {
            self.gpu_timings.clear();
            // Never attribute an earlier capture's pending frames to a new capture.
            for frame in &mut self.frames {
                frame.timing_index = None;
            }
        }
        self.gpu_timing_enabled = enabled;
        if !enabled {
            for frame in &mut self.frames {
                frame.counters = None;
            }
        }
        Ok(())
    }

    pub(crate) fn take_gpu_timings(&mut self) -> GpuTimingFeedback {
        if !self.gpu_timing_enabled {
            return GpuTimingFeedback::Disabled;
        }
        GpuTimingFeedback::Reported(self.gpu_timings.drain(..).collect())
    }

    pub(crate) fn acquire<'surface>(
        &'surface mut self,
        metrics: WindowMetrics,
    ) -> Result<FrameAcquire<ClearFrame<'surface, 'window>>, GraphicsError> {
        self.acquire_drawable(metrics).map(|acquisition| {
            acquisition.map_ready(|token| ClearFrame {
                surface: self,
                drawable: Some(token.drawable),
                _pool: token.pool,
            })
        })
    }

    fn acquire_drawable(
        &mut self,
        metrics: WindowMetrics,
    ) -> Result<FrameAcquire<MetalFrameToken>, GraphicsError> {
        // Harvest completed frames without blocking. Only reuse of the next slot
        // waits; neither drawable unavailability nor abandonment advances the ring.
        for offset in 0..FRAMES_IN_FLIGHT {
            let slot = (self.frame_slot + offset) % FRAMES_IN_FLIGHT;
            let command = self.frames[slot].command_buffer;
            if !command.is_null() {
                // SAFETY: The slot owns a retain on this submitted command buffer.
                if unsafe { objc::usize_value(command, c"status") } < 4 {
                    // Preserve submission order even if this command completes
                    // while a later slot is being inspected.
                    break;
                }
                self.finish_frame(slot)?;
            }
        }
        self.finish_frame(self.frame_slot)?;
        let Ok(extent) = surface_extent(metrics) else {
            return Ok(FrameAcquire::Unavailable(SurfaceUnavailable::Suspended));
        };
        if extent != self.info.extent() {
            // Reconfiguration happens inside acquisition: the layer is resized and the drawable
            // acquired below already belongs to the advanced generation.
            // SAFETY: The layer is live on AppKit's main thread and the aggregate ABI matches.
            unsafe { configure_layer(self.layer, metrics) };
            self.info = self.info.reconfigured(extent).ok_or_else(|| {
                GraphicsError::internal("Metal surface generation space is exhausted")
            })?;
        }

        let pool = AutoreleasePool::new();
        // SAFETY: The layer is live and nextDrawable returns an autoreleased drawable or nil.
        let drawable = unsafe { objc::object(self.layer, c"nextDrawable") };
        if drawable.is_null() {
            return Ok(FrameAcquire::Unavailable(
                SurfaceUnavailable::DrawableUnavailable,
            ));
        }
        // SAFETY: The drawable texture is live until this frame's autorelease pool drains.
        let texture = unsafe { objc::object(drawable, c"texture") };
        if texture.is_null() {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::SurfaceFailure,
                "Metal drawable returned no presentable texture",
            ));
        }
        let drawable_extent = SurfaceExtent::new(
            u32::try_from(unsafe { objc::usize_value(texture, c"width") })
                .map_err(|_| GraphicsError::new("Metal drawable width exceeds u32"))?,
            u32::try_from(unsafe { objc::usize_value(texture, c"height") })
                .map_err(|_| GraphicsError::new("Metal drawable height exceeds u32"))?,
        );
        if drawable_extent != self.info.extent() {
            // The drawable is authoritative: adopt its extent as a new generation and hand the
            // drawable out as a ready frame of that generation.
            self.info = self.info.reconfigured(drawable_extent).ok_or_else(|| {
                GraphicsError::with_kind(
                    GraphicsErrorKind::SurfaceFailure,
                    if drawable_extent.is_empty() {
                        "Metal produced an empty drawable extent"
                    } else {
                        "Metal surface generation space is exhausted"
                    },
                )
            })?;
        }

        if let Some(counters) = &self.frames[self.frame_slot].counters {
            counters.begin(self.device);
        }
        Ok(FrameAcquire::Ready(MetalFrameToken {
            drawable,
            pool,
            info: self.info,
        }))
    }

    pub(crate) fn shutdown(mut self) -> Result<(), GraphicsError> {
        let result = self.finish_all_frames();
        self.destroy_native_objects();
        result
    }

    fn present(
        &mut self,
        drawable: Object,
        color: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        // SAFETY: Every object is live on the Metal/AppKit main thread and selectors match SDK ABI.
        unsafe {
            let texture = required(objc::object(drawable, c"texture"), "Metal drawable texture")?;
            let descriptor = required(
                objc::object(
                    objc::class(c"MTLRenderPassDescriptor"),
                    c"renderPassDescriptor",
                ),
                "Metal render-pass descriptor",
            )?;
            let attachments = required(
                objc::object(descriptor, c"colorAttachments"),
                "Metal color-attachment array",
            )?;
            let attachment = required(
                objc::object_usize(attachments, c"objectAtIndexedSubscript:", 0),
                "Metal color attachment zero",
            )?;
            objc::void_object(attachment, c"setTexture:", texture);
            objc::void_usize(attachment, c"setLoadAction:", LOAD_ACTION_CLEAR);
            objc::void_usize(attachment, c"setStoreAction:", STORE_ACTION_STORE);
            let [red, green, blue, alpha] = color.components();
            objc::void_clear_color(
                attachment,
                c"setClearColor:",
                objc::ClearColor {
                    red: f64::from(red),
                    green: f64::from(green),
                    blue: f64::from(blue),
                    alpha: f64::from(alpha),
                },
            );

            let command_buffer = required(
                objc::object(self.queue, c"commandBuffer"),
                "Metal clear command buffer",
            )?;
            set_label(command_buffer, c"Mulciber clear frame");
            let encoder = required(
                objc::object_object(
                    command_buffer,
                    c"renderCommandEncoderWithDescriptor:",
                    descriptor,
                ),
                "Metal clear render encoder",
            )?;
            set_label(encoder, c"Mulciber clear pass");
            objc::void(encoder, c"endEncoding");
            self.present_commit(command_buffer, drawable);
        }
        Ok(FrameDisposition::Presented(self.info.generation()))
    }

    /// Presents and commits one frame while registering native presentation feedback.
    ///
    /// # Safety
    ///
    /// `command_buffer` must be an uncommitted Metal command buffer with all encoding ended, and
    /// `drawable` must be the live acquired drawable this frame renders into.
    pub(crate) unsafe fn present_commit(&mut self, command_buffer: Object, drawable: Object) {
        // SAFETY: The caller guarantees live objects; `addPresentedHandler:` precedes
        // presentation as Metal requires, and the retain is balanced by
        // `finish_frame`.
        unsafe {
            let drawable_id = objc::usize_value(drawable, c"drawableID");
            objc::void_object(drawable, c"addPresentedHandler:", presented_handler_block());
            objc::void_object(command_buffer, c"presentDrawable:", drawable);
            objc::void(command_buffer, c"retain");
            objc::void(command_buffer, c"commit");
            debug_assert!(self.frames[self.frame_slot].command_buffer.is_null());
            self.frames[self.frame_slot].command_buffer = command_buffer;
            self.frames[self.frame_slot].timing_index =
                self.gpu_timing_enabled.then_some(self.presented_count);
            self.frame_slot = (self.frame_slot + 1) % FRAMES_IN_FLIGHT;
            if self.pending_presents.len() >= PRESENT_FEEDBACK_CAP {
                self.pending_presents.pop_front();
            }
            self.pending_presents.push_back(PendingPresent {
                index: self.presented_count,
                drawable_id,
            });
            self.presented_count += 1;
        }
    }

    pub(crate) fn take_present_feedback(&mut self) -> PresentFeedback {
        let mut taken = Vec::new();
        if let Ok(mut events) = PRESENTED_EVENTS.lock() {
            let layer = self.layer as usize;
            events.retain(|event| {
                if event.layer == layer {
                    taken.push(*event);
                    false
                } else {
                    true
                }
            });
        }
        // SAFETY: `CACurrentMediaTime` has no preconditions and shares the presented-time
        // timebase, which `Instant` also uses on macOS.
        let now_media_time = unsafe { CACurrentMediaTime() };
        let now = Instant::now();
        let mut frames = Vec::new();
        for event in taken {
            let Some(position) = self
                .pending_presents
                .iter()
                .position(|pending| pending.drawable_id == event.drawable_id)
            else {
                continue;
            };
            let Some(pending) = self.pending_presents.remove(position) else {
                continue;
            };
            let presented_at = (event.presented_time > 0.0)
                .then_some(now_media_time - event.presented_time)
                .filter(|age| *age >= 0.0)
                .and_then(|age| now.checked_sub(Duration::from_secs_f64(age)));
            frames.push(PresentedFrame::new(pending.index, presented_at));
        }
        frames.sort_by_key(PresentedFrame::index);
        PresentFeedback::Reported(frames)
    }

    fn attach_timing(&self, descriptor: Object, pass: usize) {
        if let Some(counters) = &self.frames[self.frame_slot].counters {
            counters.attach(descriptor, pass);
        }
    }

    fn frame_slot(&self) -> usize {
        self.frame_slot
    }

    fn finish_all_frames(&mut self) -> Result<(), GraphicsError> {
        let mut result = Ok(());
        for offset in 0..FRAMES_IN_FLIGHT {
            let slot = (self.frame_slot + offset) % FRAMES_IN_FLIGHT;
            // Drain every retain even when an earlier command buffer failed.
            let completed = self.finish_frame(slot);
            if result.is_ok() {
                result = completed;
            }
        }
        result
    }

    fn finish_frame(&mut self, slot: usize) -> Result<(), GraphicsError> {
        let command_buffer =
            core::mem::replace(&mut self.frames[slot].command_buffer, ptr::null_mut());
        let frame_index = self.frames[slot].timing_index.take();
        if command_buffer.is_null() {
            return Ok(());
        }
        // SAFETY: This surface owns one retain on a committed command buffer.
        unsafe { objc::void(command_buffer, c"waitUntilCompleted") };
        // MTLCommandBufferStatusCompleted is 4; status 5 is an error.
        let status = unsafe { objc::usize_value(command_buffer, c"status") };
        let result = if status == 4 {
            if self.gpu_timing_enabled
                && let Some(frame_index) = frame_index
            {
                // SAFETY: These read-only properties are queried only after successful command
                // buffer completion and are available on Mulciber's macOS 13 baseline.
                let start = unsafe { objc::f64_value(command_buffer, c"GPUStartTime") };
                let end = unsafe { objc::f64_value(command_buffer, c"GPUEndTime") };
                if start > 0.0 && end >= start {
                    if self.gpu_timings.len() >= PRESENT_FEEDBACK_CAP {
                        self.gpu_timings.pop_front();
                    }
                    let mut scopes = std::vec![GpuScopeTiming::new(
                        GpuTimingScope::Frame,
                        Duration::from_secs_f64(end - start)
                    )];
                    if let Some(counters) = &self.frames[slot].counters {
                        scopes.extend(counters.resolve(self.device));
                    }
                    self.gpu_timings
                        .push_back(GpuFrameTiming::new(frame_index, scopes));
                }
            }
            Ok(())
        } else {
            // SAFETY: `error` is nil or an NSError after completion.
            let error = unsafe { objc::object(command_buffer, c"error") };
            Err(GraphicsError::new(std::format!(
                "Metal clear frame completed with status {status}: {}",
                objc::description(error)
            )))
        };
        // SAFETY: GPU completion permits balancing the surface's retain.
        unsafe { objc::void(command_buffer, c"release") };
        result
    }

    fn destroy_native_objects(&mut self) {
        for frame in &mut self.frames {
            frame.counters = None;
        }
        self.counter_set = None;
        // Purge this layer's undrained feedback. A handler still in flight can land after the
        // purge; the bounded queue and per-drawable-ID matching keep that stale remainder inert.
        if !self.layer.is_null()
            && let Ok(mut events) = PRESENTED_EVENTS.lock()
        {
            let layer = self.layer as usize;
            events.retain(|event| event.layer != layer);
        }
        // SAFETY: The view and owned graphics objects remain on their creating main thread.
        unsafe {
            if !self.view.is_null() && !self.layer.is_null() {
                objc::void_object(self.view, c"setLayer:", ptr::null_mut());
            }
            if !self.queue.is_null() {
                objc::void(self.queue, c"release");
                self.queue = ptr::null_mut();
            }
            if !self.layer.is_null() {
                objc::void(self.layer, c"release");
                self.layer = ptr::null_mut();
            }
            if !self.device.is_null() {
                objc::void(self.device, c"release");
                self.device = ptr::null_mut();
            }
        }
    }
}

struct MetalFrameToken {
    drawable: Object,
    pool: AutoreleasePool,
    info: SurfaceInfo,
}

mod textured;
pub(crate) use textured::{TexturedFrameToken, TexturedSession};

impl Drop for ClearSurface<'_> {
    fn drop(&mut self) {
        let _ = self.finish_all_frames();
        self.destroy_native_objects();
    }
}

pub(crate) struct ClearFrame<'surface, 'window> {
    surface: &'surface mut ClearSurface<'window>,
    drawable: Option<Object>,
    _pool: AutoreleasePool,
}

impl ClearFrame<'_, '_> {
    pub(crate) const fn surface_info(&self) -> SurfaceInfo {
        self.surface.info
    }

    pub(crate) fn clear_and_present(
        mut self,
        color: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let drawable = self
            .drawable
            .take()
            .expect("a live clear frame owns one Metal drawable");
        self.surface.present(drawable, color)
    }

    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn abandon(mut self) -> Result<FrameDisposition, GraphicsError> {
        self.drawable.take();
        Ok(FrameDisposition::Abandoned(self.surface.info.generation()))
    }
}

impl Drop for ClearFrame<'_, '_> {
    fn drop(&mut self) {
        // Draining the owned autorelease pool safely releases an unsubmitted Metal drawable.
        self.drawable.take();
    }
}

fn surface_extent(metrics: WindowMetrics) -> Result<SurfaceExtent, GraphicsError> {
    let extent = metrics.extent();
    let extent = SurfaceExtent::new(extent.width(), extent.height());
    if extent.is_empty() {
        Err(GraphicsError::lifecycle("window surface is suspended"))
    } else {
        Ok(extent)
    }
}

unsafe fn configure_layer(layer: Object, metrics: WindowMetrics) {
    let extent = metrics.extent();
    // SAFETY: The caller supplies a live CAMetalLayer and matching aggregate ABI.
    unsafe {
        objc::void_size(
            layer,
            c"setDrawableSize:",
            Size {
                width: f64::from(extent.width()),
                height: f64::from(extent.height()),
            },
        );
        objc::void_f64(layer, c"setContentsScale:", metrics.scale_factor());
    }
}

fn required(value: Object, label: &str) -> Result<Object, GraphicsError> {
    if value.is_null() {
        Err(GraphicsError::new(std::format!(
            "{label}: object is unavailable"
        )))
    } else {
        Ok(value)
    }
}

fn set_label(object: Object, label: &core::ffi::CStr) {
    // SAFETY: Metal objects implement setLabel: with NSString input.
    unsafe { objc::void_object(object, c"setLabel:", objc::ns_string(label)) };
}
