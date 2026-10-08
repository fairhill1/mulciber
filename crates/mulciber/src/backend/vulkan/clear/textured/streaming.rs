//! Frame-ordered texture replacement; staging belongs to the acquired frame slot.
use super::{
    Buffer, ClearSurface, GraphicsError, Image, ResourceId, SampledTextureFormat, TextureResource,
    TexturedSession, color_subresource_levels, create_buffer, image_barrier, mip_extent,
    mip_generation, pipeline_barrier, vk, write_buffer,
};
use std::{format, vec::Vec};
impl TextureResource {
    pub(super) fn new(
        image: Image,
        sampler: vk::VkSampler,
        extent: [u32; 2],
        format: SampledTextureFormat,
        mip_levels: u32,
        layers: u32,
        generates_mips: bool,
    ) -> Self {
        Self {
            image,
            sampler,
            extent,
            format,
            mip_levels,
            layers,
            generates_mips,
            pending: None,
            uploads: [Buffer::default(); ClearSurface::frames_in_flight()],
            upload_ready: false,
        }
    }
}
impl TexturedSession<'_> {
    /// Queues the texture's whole chain, one tightly packed byte vector per level, or with
    /// `generate_mips` level 0 alone for blits to regenerate the rest.
    pub(crate) fn update_float_texture(
        &mut self,
        id: ResourceId,
        width: u32,
        height: u32,
        levels: Vec<Vec<u8>>,
        generate_mips: bool,
    ) -> Result<(), GraphicsError> {
        let index = self.textures.index_of(id)?;
        let texture = &mut self.textures[index];
        if texture.extent != [width, height]
            || texture.format != SampledTextureFormat::Float16
            || texture.layers != 1
        {
            return Err(GraphicsError::invalid_request(
                "float texture update requires a 2D RGBA16Float texture of matching dimensions",
            ));
        }
        if generate_mips && !texture.generates_mips {
            return Err(GraphicsError::invalid_request(
                "generated-mip updates require a texture created with generated mips",
            ));
        }
        if generate_mips {
            debug_assert_eq!(levels.len(), 1, "generated-mip updates supply level 0");
        } else if u32::try_from(levels.len()).ok() != Some(texture.mip_levels) {
            return Err(GraphicsError::invalid_request(format!(
                "float texture update supplies {} mip levels but the texture has {}",
                levels.len(),
                texture.mip_levels
            )));
        }
        texture.pending = Some(levels);
        Ok(())
    }
    pub(super) fn prepare_texture_updates(&mut self) -> Result<(), GraphicsError> {
        let slot = self.surface.frame_slot_index();
        for texture in self.textures.iter_mut() {
            let Some(levels) = texture.pending.as_ref() else {
                continue;
            };
            let parts: Vec<&[u8]> = levels.iter().map(Vec::as_slice).collect();
            let staging = &mut texture.uploads[slot];
            if staging.handle.is_null() {
                // Extents and mip count are fixed, so the full chain sizes this slot's staging
                // once for every later replacement, whether it carries every level or level 0.
                let chain = (0..texture.mip_levels)
                    .map(|level| {
                        mip_extent(texture.extent[0], level) as usize
                            * mip_extent(texture.extent[1], level) as usize
                            * 8
                    })
                    .sum();
                *staging = create_buffer(
                    &self.surface,
                    chain,
                    vk::VK_BUFFER_USAGE_TRANSFER_SRC_BIT as u32,
                    &[],
                )?;
            }
            write_buffer(&self.surface, staging, &parts)?;
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
            let levels = texture.pending.as_deref().unwrap_or_default();
            // Levels sit back to back in staging, as `write_buffer` packed them. Each is a whole
            // number of 8-byte texels, so every offset is a multiple of the texel block size.
            let mut regions = Vec::with_capacity(levels.len());
            let mut buffer_offset = 0_u64;
            for (level, bytes) in (0_u32..).zip(levels) {
                debug_assert_eq!(buffer_offset % 8, 0, "RGBA16Float texel alignment");
                regions.push(vk::VkBufferImageCopy2 {
                    sType: vk::VK_STRUCTURE_TYPE_BUFFER_IMAGE_COPY_2,
                    bufferOffset: buffer_offset,
                    imageSubresource: vk::VkImageSubresourceLayers {
                        aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
                        mipLevel: level,
                        baseArrayLayer: 0,
                        layerCount: 1,
                    },
                    imageExtent: vk::VkExtent3D {
                        width: mip_extent(texture.extent[0], level),
                        height: mip_extent(texture.extent[1], level),
                        depth: 1,
                    },
                    ..Default::default()
                });
                // The staging allocation already holds the whole sum as a `VkDeviceSize`.
                buffer_offset += bytes.len() as u64;
            }
            let before = image_barrier(
                texture.image.handle,
                vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_VERTEX_SHADER_BIT
                    | vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
                vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
                color_subresource_levels(texture.mip_levels),
            );
            pipeline_barrier(&self.surface, command, &before);
            let generate = levels.len() < texture.mip_levels as usize;
            let copy = vk::VkCopyBufferToImageInfo2 {
                sType: vk::VK_STRUCTURE_TYPE_COPY_BUFFER_TO_IMAGE_INFO_2,
                srcBuffer: texture.uploads[slot].handle,
                dstImage: texture.image.handle,
                dstImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                regionCount: u32::try_from(regions.len()).expect("validated mip count fits u32"),
                pRegions: regions.as_ptr(),
                ..Default::default()
            };
            unsafe {
                self.surface
                    .device()
                    .functions
                    .cmd_copy_buffer_to_image2
                    .expect("loaded function")(command, &raw const copy);
            }
            if generate {
                mip_generation::record_generated_mips(
                    self.surface.device(),
                    command,
                    texture.image.handle,
                    texture.extent,
                    texture.mip_levels,
                    1,
                );
            } else {
                let after = image_barrier(
                    texture.image.handle,
                    vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                    vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                    vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                    vk::VK_PIPELINE_STAGE_2_VERTEX_SHADER_BIT
                        | vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
                    vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
                    vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
                    color_subresource_levels(texture.mip_levels),
                );
                pipeline_barrier(&self.surface, command, &after);
            }
            texture.pending = None;
            texture.upload_ready = false;
        }
    }
}
