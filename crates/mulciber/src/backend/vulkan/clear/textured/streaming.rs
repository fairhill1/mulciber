//! Frame-ordered texture replacement; staging belongs to the acquired frame slot.
use super::{
    Buffer, ClearSurface, GraphicsError, Image, ResourceId, SampledTextureFormat, TextureResource,
    TexturedSession, color_subresource_levels, create_buffer, image_barrier, pipeline_barrier, vk,
    write_buffer,
};
use std::vec::Vec;
impl TextureResource {
    pub(super) fn new(
        image: Image,
        sampler: vk::VkSampler,
        extent: [u32; 2],
        format: SampledTextureFormat,
        mip_levels: u32,
    ) -> Self {
        Self {
            image,
            sampler,
            extent,
            format,
            mip_levels,
            pending: None,
            uploads: [Buffer::default(); ClearSurface::frames_in_flight()],
            upload_ready: false,
        }
    }
}
impl TexturedSession<'_> {
    pub(crate) fn update_float_texture(
        &mut self,
        id: ResourceId,
        width: u32,
        height: u32,
        bytes: Vec<u8>,
    ) -> Result<(), GraphicsError> {
        let index = self.textures.index_of(id)?;
        let texture = &mut self.textures[index];
        if texture.extent != [width, height]
            || texture.format != SampledTextureFormat::Float16
            || texture.mip_levels != 1
        {
            return Err(GraphicsError::invalid_request(
                "float texture update requires matching dimensions and one RGBA16Float level",
            ));
        }
        texture.pending = Some(bytes);
        Ok(())
    }
    pub(super) fn prepare_texture_updates(&mut self) -> Result<(), GraphicsError> {
        let slot = self.surface.frame_slot_index();
        for texture in self.textures.iter_mut() {
            let Some(bytes) = texture.pending.as_ref() else {
                continue;
            };
            let staging = &mut texture.uploads[slot];
            if staging.handle.is_null() {
                *staging = create_buffer(
                    &self.surface,
                    bytes.len(),
                    vk::VK_BUFFER_USAGE_TRANSFER_SRC_BIT as u32,
                    bytes,
                )?;
            } else {
                write_buffer(&self.surface, staging, bytes)?;
            }
            // Keep the source until recording succeeds, so an aborted preparation
            // can retry on a different acquired frame slot.
            texture.upload_ready = true;
        }
        Ok(())
    }
    pub(super) fn record_texture_updates(&mut self) {
        let slot = self.surface.frame_slot_index();
        let command = self.surface.frame_command_buffer();
        for texture in self.textures.iter_mut().filter(|t| t.upload_ready) {
            let before = image_barrier(
                texture.image.handle,
                vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_VERTEX_SHADER_BIT
                    | vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
                vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
                color_subresource_levels(1),
            );
            pipeline_barrier(&self.surface, command, &before);
            let region = vk::VkBufferImageCopy2 {
                sType: vk::VK_STRUCTURE_TYPE_BUFFER_IMAGE_COPY_2,
                imageSubresource: vk::VkImageSubresourceLayers {
                    aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
                    layerCount: 1,
                    ..Default::default()
                },
                imageExtent: vk::VkExtent3D {
                    width: texture.extent[0],
                    height: texture.extent[1],
                    depth: 1,
                },
                ..Default::default()
            };
            let copy = vk::VkCopyBufferToImageInfo2 {
                sType: vk::VK_STRUCTURE_TYPE_COPY_BUFFER_TO_IMAGE_INFO_2,
                srcBuffer: texture.uploads[slot].handle,
                dstImage: texture.image.handle,
                dstImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                regionCount: 1,
                pRegions: &raw const region,
                ..Default::default()
            };
            unsafe {
                self.surface
                    .device()
                    .functions
                    .cmd_copy_buffer_to_image2
                    .expect("loaded function")(command, &raw const copy);
            }
            let after = image_barrier(
                texture.image.handle,
                vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_PIPELINE_STAGE_2_VERTEX_SHADER_BIT
                    | vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
                vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
                vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
                color_subresource_levels(1),
            );
            pipeline_barrier(&self.surface, command, &after);
            texture.pending = None;
            texture.upload_ready = false;
        }
    }
}
