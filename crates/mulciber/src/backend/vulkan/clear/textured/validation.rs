//! Synchronous HDR readback compiled only for the repository validation probe.
use super::{
    GraphicsError, ResourceId, TexturedSession, color_subresource_range, create_buffer,
    destroy_buffer, image_barrier, map_buffer, pipeline_barrier, vk,
};

impl TexturedSession<'_> {
    #[allow(clippy::too_many_lines)] // One ordered copy/barrier/map sequence.
    pub(crate) fn read_hdr_validation_pixel(
        &mut self,
        id: ResourceId,
    ) -> Result<[u16; 4], GraphicsError> {
        self.surface.finish()?;
        let image = self
            .postprocess_targets
            .get(id)?
            .scene_color
            .ok_or_else(|| GraphicsError::stale_resource("validation target was retired"))?;
        let buffer = create_buffer(
            &self.surface,
            8,
            vk::VK_BUFFER_USAGE_TRANSFER_DST_BIT.cast_unsigned(),
            &[0; 8],
        )?;
        let result = (|| {
            self.begin_upload()?;
            let barrier = image_barrier(
                image.handle,
                vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT
                    | vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
                vk::VK_ACCESS_2_TRANSFER_READ_BIT,
                color_subresource_range(),
            );
            pipeline_barrier(
                &self.surface,
                self.surface.upload_command_buffer(),
                &barrier,
            );
            let region = vk::VkBufferImageCopy2 {
                sType: vk::VK_STRUCTURE_TYPE_BUFFER_IMAGE_COPY_2,
                imageSubresource: vk::VkImageSubresourceLayers {
                    aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT.cast_unsigned(),
                    layerCount: 1,
                    ..Default::default()
                },
                imageExtent: vk::VkExtent3D {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
                ..Default::default()
            };
            let copy = vk::VkCopyImageToBufferInfo2 {
                sType: vk::VK_STRUCTURE_TYPE_COPY_IMAGE_TO_BUFFER_INFO_2,
                srcImage: image.handle,
                srcImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                dstBuffer: buffer.handle,
                regionCount: 1,
                pRegions: &raw const region,
                ..Default::default()
            };
            unsafe {
                self.surface
                    .device()
                    .functions
                    .cmd_copy_image_to_buffer2
                    .expect("loaded function")(
                    self.surface.upload_command_buffer(),
                    &raw const copy,
                );
            }
            let restore = image_barrier(
                image.handle,
                vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
                vk::VK_ACCESS_2_TRANSFER_READ_BIT,
                vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT
                    | vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                color_subresource_range(),
            );
            pipeline_barrier(
                &self.surface,
                self.surface.upload_command_buffer(),
                &restore,
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
                ..Default::default()
            };
            unsafe {
                self.surface
                    .device()
                    .functions
                    .cmd_pipeline_barrier2
                    .expect("loaded function")(
                    self.surface.upload_command_buffer(),
                    &raw const dependency,
                );
            }
            self.end_upload()?;
            let mapped = map_buffer(&self.surface, &buffer)?;
            let result = core::array::from_fn(|i| unsafe {
                u16::from_ne_bytes([*mapped.add(i * 2), *mapped.add(i * 2 + 1)])
            });
            unsafe {
                self.surface
                    .device()
                    .functions
                    .unmap_memory
                    .expect("loaded function")(
                    self.surface.device().handle, buffer.memory
                );
            }
            Ok(result)
        })();
        destroy_buffer(&self.surface, buffer);
        result
    }
}
