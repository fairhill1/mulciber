#[allow(missing_docs)]
pub mod objc;

use core::marker::PhantomData;
use core::ptr;

use mulciber_platform::{SurfaceTarget, WindowMetrics, integration};

use crate::{
    ClearColor, FrameAcquire, FrameDisposition, GraphicsError, SurfaceExtent, SurfaceInfo,
    SurfaceUnavailable,
};

pub(crate) const BACKEND_NAME: &str = "Metal";

use objc::{AutoreleasePool, Object, Size};

const PIXEL_FORMAT_BGRA8_UNORM_SRGB: usize = 81;
const LOAD_ACTION_CLEAR: usize = 2;
const STORE_ACTION_STORE: usize = 1;

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
    fn MTLCreateSystemDefaultDevice() -> Object;
}

#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {}

pub(crate) struct ClearSurface<'window> {
    view: Object,
    device: Object,
    layer: Object,
    queue: Object,
    info: SurfaceInfo,
    last_command_buffer: Object,
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
            GraphicsError::new("Metal surface requires non-empty initial metrics")
        })?;
        let _pool = AutoreleasePool::new();
        // SAFETY: The target remains borrowed for this surface's lifetime and all selectors match
        // their AppKit, QuartzCore, and Metal SDK ABIs on the process main thread.
        unsafe {
            let view = integration::appkit_view(&target).as_ptr();
            let device = MTLCreateSystemDefaultDevice();
            if device.is_null() {
                return Err(GraphicsError::new("no default Metal device is available"));
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
            objc::void_usize(layer, c"setMaximumDrawableCount:", 3);
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
                last_command_buffer: ptr::null_mut(),
                _window: PhantomData,
            })
        }
    }

    pub(crate) const fn info(&self) -> SurfaceInfo {
        self.info
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
        self.finish_last_submission()?;
        let Ok(extent) = surface_extent(metrics) else {
            return Ok(FrameAcquire::Unavailable(SurfaceUnavailable::Suspended));
        };
        if extent != self.info.extent() {
            // Reconfiguration happens inside acquisition: the layer is resized and the drawable
            // acquired below already belongs to the advanced generation.
            // SAFETY: The layer is live on AppKit's main thread and the aggregate ABI matches.
            unsafe { configure_layer(self.layer, metrics) };
            self.info = self
                .info
                .reconfigured(extent)
                .ok_or_else(|| GraphicsError::new("Metal surface generation space is exhausted"))?;
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
            return Err(GraphicsError::new(
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
                GraphicsError::new(if drawable_extent.is_empty() {
                    "Metal produced an empty drawable extent"
                } else {
                    "Metal surface generation space is exhausted"
                })
            })?;
        }

        Ok(FrameAcquire::Ready(MetalFrameToken {
            drawable,
            pool,
            info: self.info,
        }))
    }

    pub(crate) fn shutdown(mut self) -> Result<(), GraphicsError> {
        let result = self.finish_last_submission();
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
            objc::void_object(command_buffer, c"presentDrawable:", drawable);
            objc::void(command_buffer, c"retain");
            objc::void(command_buffer, c"commit");
            self.last_command_buffer = command_buffer;
        }
        Ok(FrameDisposition::Presented(self.info.generation()))
    }

    fn finish_last_submission(&mut self) -> Result<(), GraphicsError> {
        if self.last_command_buffer.is_null() {
            return Ok(());
        }
        let command_buffer = core::mem::replace(&mut self.last_command_buffer, ptr::null_mut());
        // SAFETY: This surface owns one retain on a committed command buffer.
        unsafe { objc::void(command_buffer, c"waitUntilCompleted") };
        // MTLCommandBufferStatusCompleted is 4; status 5 is an error.
        let status = unsafe { objc::usize_value(command_buffer, c"status") };
        let result = if status == 4 {
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
        let _ = self.finish_last_submission();
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
        Err(GraphicsError::new("window surface is suspended"))
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
