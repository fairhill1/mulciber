//! Presented-frame capture: the swapchain image is copied into a host-visible buffer after the
//! frame's last pass, in the frame's own command buffer, and read once that frame's fence signals.
use core::{mem, slice};

use super::{
    Buffer, GraphicsError, TexturedFrameToken, TexturedSession, color_subresource_range,
    create_buffer, destroy_buffer, error, image_barrier, map_buffer, vk,
};
use crate::graphics::{
    CaptureByteOrder, FrameCapture, capture_byte_len, frame_capture_from_native,
};
use crate::{FrameDisposition, GraphicsErrorKind};

/// A capture copy recorded, or about to be recorded, into the frame being built.
pub(super) struct PendingCapture {
    buffer: Buffer,
    width: u32,
    height: u32,
    order: CaptureByteOrder,
    opaque: bool,
}

/// Capture state owned by the session.
#[derive(Default)]
pub(super) struct CaptureState {
    /// The application asked for the next acquired frame to be captured.
    requested: bool,
    /// Readback storage for the frame currently being recorded. Only `submit_recorded` takes it
    /// after a submission; anything left here was never submitted.
    pending: Option<PendingCapture>,
    /// The last completed capture, until the application takes it.
    completed: Option<FrameCapture>,
}

/// Channel order of a swapchain format Mulciber can capture.
const fn byte_order(format: vk::VkFormat) -> Option<CaptureByteOrder> {
    match format {
        vk::VK_FORMAT_B8G8R8A8_SRGB | vk::VK_FORMAT_B8G8R8A8_UNORM => Some(CaptureByteOrder::Bgra),
        vk::VK_FORMAT_R8G8B8A8_SRGB | vk::VK_FORMAT_R8G8B8A8_UNORM => Some(CaptureByteOrder::Rgba),
        _ => None,
    }
}

impl TexturedSession<'_> {
    pub(crate) fn request_frame_capture(&mut self) -> Result<(), GraphicsError> {
        let swapchain = &self.surface.swapchain;
        if !swapchain.capturable {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                "this Vulkan surface does not allow transfer-source swapchain images, so its \
                 presented frames cannot be captured",
            ));
        }
        if byte_order(swapchain.format).is_none() {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                std::format!(
                    "Vulkan swapchain format {} is not a four-channel 8-bit format a frame \
                     capture can read",
                    swapchain.format
                ),
            ));
        }
        self.capture.requested = true;
        Ok(())
    }

    pub(crate) fn take_frame_capture(&mut self) -> Option<FrameCapture> {
        self.capture.completed.take()
    }

    /// Whether a frame acquired now from the current swapchain is the one to capture.
    pub(super) fn capture_next_acquired(&self) -> bool {
        self.capture.requested
            && self.surface.swapchain.capturable
            && byte_order(self.surface.swapchain.format).is_some()
    }

    /// Allocates readback storage when this frame was acquired under a pending request. Called
    /// once the submission has passed validation, just before the frame is recorded; the copy
    /// itself is recorded by [`Self::record_present_transition`].
    pub(super) fn begin_capture(
        &mut self,
        token: &TexturedFrameToken,
    ) -> Result<(), GraphicsError> {
        self.discard_unsubmitted_capture();
        if !token.capture {
            return Ok(());
        }
        let swapchain = &self.surface.swapchain;
        let order = byte_order(swapchain.format)
            .ok_or_else(|| error("captured swapchain format has no byte order"))?;
        let (width, height) = (swapchain.extent.width, swapchain.extent.height);
        let size = capture_byte_len(width, height)
            .ok_or_else(|| error("frame capture size exceeds the address space"))?;
        let opaque = swapchain.opaque;
        let buffer = create_buffer(
            &self.surface,
            size,
            vk::VK_BUFFER_USAGE_TRANSFER_DST_BIT.cast_unsigned(),
            &[],
        )?;
        self.capture.pending = Some(PendingCapture {
            buffer,
            width,
            height,
            order,
            opaque,
        });
        Ok(())
    }

    /// Releases readback storage left by a frame whose recording or submission failed. The GPU
    /// never received it.
    pub(super) fn discard_unsubmitted_capture(&mut self) {
        if let Some(pending) = self.capture.pending.take() {
            destroy_buffer(&self.surface, pending.buffer);
        }
    }

    /// Ends the frame's command buffer work on the swapchain image: the transition to
    /// presentation, preceded by the capture copy when one is pending.
    pub(super) fn record_present_transition(&self, image: vk::VkImage) {
        let command_buffer = self.surface.frame_command_buffer();
        let Some(capture) = &self.capture.pending else {
            let present = image_barrier(
                image,
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                vk::VK_PIPELINE_STAGE_2_NONE,
                vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                vk::VK_ACCESS_2_NONE,
                color_subresource_range(),
            );
            super::pipeline_barrier(&self.surface, command_buffer, &present);
            return;
        };
        // The last pass (scene, resolve, postprocess or overlay) wrote the image as a color
        // attachment; the copy reads it once those writes are available.
        let to_copy = image_barrier(
            image,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_ACCESS_2_TRANSFER_READ_BIT,
            color_subresource_range(),
        );
        super::pipeline_barrier(&self.surface, command_buffer, &to_copy);
        // Zero row length and image height pack the rows tightly.
        let region = vk::VkBufferImageCopy2 {
            sType: vk::VK_STRUCTURE_TYPE_BUFFER_IMAGE_COPY_2,
            imageSubresource: vk::VkImageSubresourceLayers {
                aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT.cast_unsigned(),
                layerCount: 1,
                ..Default::default()
            },
            imageExtent: vk::VkExtent3D {
                width: capture.width,
                height: capture.height,
                depth: 1,
            },
            ..Default::default()
        };
        let copy = vk::VkCopyImageToBufferInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_COPY_IMAGE_TO_BUFFER_INFO_2,
            srcImage: image,
            srcImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            dstBuffer: capture.buffer.handle,
            regionCount: 1,
            pRegions: &raw const region,
            ..Default::default()
        };
        let functions = &self.surface.device().functions;
        unsafe {
            functions
                .cmd_copy_image_to_buffer2
                .expect("loaded function")(command_buffer, &raw const copy);
        }
        // The presentation transition waits only for the copy's read; the semaphore the
        // submission signals orders presentation after it. The copied bytes are made visible to
        // the host, which reads them after the frame's fence.
        let to_present = image_barrier(
            image,
            vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_NONE,
            color_subresource_range(),
        );
        let host = vk::VkMemoryBarrier2 {
            sType: vk::VK_STRUCTURE_TYPE_MEMORY_BARRIER_2,
            srcStageMask: vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            srcAccessMask: vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
            dstStageMask: vk::VK_PIPELINE_STAGE_2_HOST_BIT,
            dstAccessMask: vk::VK_ACCESS_2_HOST_READ_BIT,
            ..Default::default()
        };
        let dependency = vk::VkDependencyInfo {
            sType: vk::VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
            memoryBarrierCount: 1,
            pMemoryBarriers: &raw const host,
            imageMemoryBarrierCount: 1,
            pImageMemoryBarriers: &raw const to_present,
            ..Default::default()
        };
        unsafe {
            functions.cmd_pipeline_barrier2.expect("loaded function")(
                command_buffer,
                &raw const dependency,
            );
        }
    }

    /// Completes the capture recorded into the frame just handed to `vkQueueSubmit2`. A submitted
    /// copy is waited for even when presentation failed, because its buffer cannot be released
    /// earlier; only a presented frame becomes the capture and clears the request.
    #[allow(clippy::needless_pass_by_value)] // Taking it marks the readback storage released.
    pub(super) fn finish_capture(
        &mut self,
        pending: PendingCapture,
        slot: usize,
        submitted: bool,
        frame_index: u64,
        disposition: &Result<FrameDisposition, GraphicsError>,
    ) -> Result<(), GraphicsError> {
        let result = (|| {
            if !submitted {
                return Ok(());
            }
            self.surface.wait_for_slot(slot)?;
            if !matches!(disposition, Ok(FrameDisposition::Presented(_))) {
                return Ok(());
            }
            let length = usize::try_from(pending.buffer.size)
                .map_err(|_| error("frame capture size exceeds the address space"))?;
            let mapped = map_buffer(&self.surface, &pending.buffer)?;
            // The memory is host-coherent and the frame's fence has signalled after the copy's
            // host-visibility barrier, so the mapped bytes are the copied image.
            let pixels = unsafe { slice::from_raw_parts(mapped.cast_const(), length) }.to_vec();
            let device = self.surface.device();
            unsafe {
                device.functions.unmap_memory.expect("loaded function")(
                    device.handle,
                    pending.buffer.memory,
                );
            }
            self.capture.completed = Some(frame_capture_from_native(
                frame_index,
                pending.width,
                pending.height,
                pixels,
                pending.order,
                pending.opaque,
            ));
            self.capture.requested = false;
            Ok(())
        })();
        destroy_buffer(&self.surface, pending.buffer);
        result
    }

    /// Takes the frame's readback storage once its submission has been attempted.
    pub(super) fn take_pending_capture(&mut self) -> Option<PendingCapture> {
        self.capture.pending.take()
    }

    /// Releases readback storage at teardown, after the device is idle.
    pub(super) fn destroy_capture(&mut self) {
        if let Some(pending) = mem::take(&mut self.capture).pending {
            destroy_buffer(&self.surface, pending.buffer);
        }
    }
}
