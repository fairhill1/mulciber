//! Presented-frame capture: the drawable texture is blitted into a shared buffer after the frame's
//! last pass, in the frame's own command buffer, and read once that command buffer completes.
//!
//! Drawables are vended with `framebufferOnly` only while no capture is pending, so ordinary frames
//! keep Metal's display optimizations. This path type-checks for macOS but has not run.
use core::slice;

use super::{GraphicsError, Origin3, Size3, TexturedSession, objc, required};
use crate::graphics::{
    CaptureByteOrder, FrameCapture, capture_byte_len, frame_capture_from_native,
};

use objc::Object;

/// Capture state owned by the session.
#[derive(Default)]
pub(super) struct CaptureState {
    /// The application asked for the next acquired frame to be captured, and the layer vends
    /// readable drawables until it is.
    requested: bool,
    /// The last completed capture, until the application takes it.
    completed: Option<FrameCapture>,
}

/// A blit encoded into the frame being presented.
#[derive(Clone, Copy)]
pub(super) struct EncodedCapture {
    buffer: Object,
    width: u32,
    height: u32,
    opaque: bool,
    index: u64,
}

impl TexturedSession<'_> {
    #[allow(clippy::unnecessary_wraps)] // Vulkan can refuse; Metal's BGRA8 drawables always read back.
    pub(crate) fn request_frame_capture(&mut self) -> Result<(), GraphicsError> {
        if !self.capture.requested {
            // SAFETY: The layer is live on the main thread; the change applies to drawables
            // vended from now on, which is why a request must precede acquisition.
            unsafe { objc::void_bool(self.surface.layer, c"setFramebufferOnly:", false) };
            self.capture.requested = true;
        }
        Ok(())
    }

    pub(crate) fn take_frame_capture(&mut self) -> Option<FrameCapture> {
        self.capture.completed.take()
    }

    pub(super) const fn capture_next_acquired(&self) -> bool {
        self.capture.requested
    }

    /// Encodes the copy of the finished drawable texture into `command`, after every render
    /// encoder and before presentation, when this frame was acquired under a pending request.
    /// A drawable that is still framebuffer-only — one the layer vended before the property
    /// change took effect — is skipped and the request stays pending.
    ///
    /// # Safety
    ///
    /// `command` must be the frame's uncommitted command buffer with every encoder ended, and
    /// `texture` its live drawable texture.
    pub(super) unsafe fn encode_capture(
        &self,
        capture: bool,
        command: Object,
        texture: Object,
    ) -> Result<Option<EncodedCapture>, GraphicsError> {
        if !capture {
            return Ok(None);
        }
        unsafe {
            if objc::bool_value(texture, c"isFramebufferOnly") {
                return Ok(None);
            }
            let width = u32::try_from(objc::usize_value(texture, c"width"))
                .map_err(|_| GraphicsError::new("Metal drawable width exceeds u32"))?;
            let height = u32::try_from(objc::usize_value(texture, c"height"))
                .map_err(|_| GraphicsError::new("Metal drawable height exceeds u32"))?;
            let length = capture_byte_len(width, height).ok_or_else(|| {
                GraphicsError::new("frame capture size exceeds the address space")
            })?;
            // MTLResourceStorageModeShared: the CPU reads the bytes after completion.
            let buffer = required(
                objc::object_two_usizes(
                    self.surface.device,
                    c"newBufferWithLength:options:",
                    length.max(1),
                    0,
                ),
                "Metal frame capture buffer",
            )?;
            let blit = objc::object(command, c"blitCommandEncoder");
            if blit.is_null() {
                objc::void(buffer, c"release");
                return Err(GraphicsError::new(
                    "create Metal frame capture blit encoder: object is unavailable",
                ));
            }
            let row = usize::try_from(width).expect("u32 fits usize") * 4;
            objc::void_copy_texture_to_buffer(
                blit,
                c"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:",
                texture,
                0,
                0,
                Origin3 { x: 0, y: 0, z: 0 },
                Size3 {
                    width: usize::try_from(width).expect("u32 fits usize"),
                    height: usize::try_from(height).expect("u32 fits usize"),
                    depth: 1,
                },
                buffer,
                0,
                row,
                length,
            );
            objc::void(blit, c"endEncoding");
            Ok(Some(EncodedCapture {
                buffer,
                width,
                height,
                opaque: objc::bool_value(self.surface.layer, c"isOpaque"),
                index: self.surface.presented_count,
            }))
        }
    }

    /// Blocks until the presented frame's command buffer completes, then converts the copied
    /// BGRA texels into the capture, clears the request and restores framebuffer-only drawables.
    ///
    /// # Safety
    ///
    /// `command` must be the committed command buffer `capture` was encoded into.
    pub(super) unsafe fn finish_capture(
        &mut self,
        command: Object,
        capture: EncodedCapture,
    ) -> Result<(), GraphicsError> {
        unsafe {
            objc::void(command, c"waitUntilCompleted");
            // MTLCommandBufferStatusCompleted is 4; a failed frame keeps the request pending.
            let result = if objc::usize_value(command, c"status") == 4 {
                let length =
                    capture_byte_len(capture.width, capture.height).expect("checked when encoded");
                let contents = objc::object(capture.buffer, c"contents").cast::<u8>();
                let pixels = slice::from_raw_parts(contents.cast_const(), length).to_vec();
                self.capture.completed = Some(frame_capture_from_native(
                    capture.index,
                    capture.width,
                    capture.height,
                    pixels,
                    CaptureByteOrder::Bgra,
                    capture.opaque,
                ));
                self.capture.requested = false;
                objc::void_bool(self.surface.layer, c"setFramebufferOnly:", true);
                Ok(())
            } else {
                Err(GraphicsError::new("Metal frame capture command failed"))
            };
            objc::void(capture.buffer, c"release");
            result
        }
    }
}
