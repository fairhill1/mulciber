//! Render textures: an `RGBA16Float` color image sampled like any upload, with depth and
//! multisample images of its own, rendered by offscreen passes recorded after the shadow
//! prepass and before the scene, each with explicit attachment-to-sampled-read transitions.
use super::{
    ClearColor, DEPTH_FORMAT, GraphicsError, Image, ResolvedMaterialDraw, ResourceId,
    SampledTextureFormat, TextureResource, TexturedSession, bloom, check, color_subresource_range,
    create_image, depth_subresource_range, destroy_image, destroy_image_device, error,
    image_barrier, pipeline_barriers, ptr, vk,
};
use crate::OffscreenPass;
use std::vec;

/// What a render texture owns beyond its sampled color.
pub(super) struct RenderAttachments {
    depth: Image,
    /// Multisample color resolved into the sampled color; absent at one sample.
    multisample: Option<Image>,
    /// Samples per pixel the storage was built for; rendering at another count is refused.
    sample_count: vk::VkSampleCountFlagBits,
    /// Whether any offscreen pass has rendered into the texture; sampling before that is
    /// rejected.
    rendered: bool,
}

impl RenderAttachments {
    pub(super) fn destroy(self, device: &super::super::Device) {
        unsafe {
            if let Some(multisample) = self.multisample {
                destroy_image_device(device, multisample);
            }
            destroy_image_device(device, self.depth);
        }
    }
}

/// One offscreen pass prepared for recording: its target's arena index, how many of the
/// resolved offscreen draws are its own, and its clears.
#[derive(Clone, Copy)]
pub(super) struct PendingOffscreen {
    texture: usize,
    draws: usize,
    clear: ClearColor,
    depth_clear: f32,
}

impl TexturedSession<'_> {
    pub(crate) fn create_render_texture(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<ResourceId, GraphicsError> {
        bloom::validate_format(&self.surface, self.sample_count, width, height)?;
        let sample_count = self.sample_count;
        let surface = &self.surface;
        let color = create_image(
            surface,
            width,
            height,
            vk::VK_FORMAT_R16G16B16A16_SFLOAT,
            (vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | vk::VK_IMAGE_USAGE_SAMPLED_BIT)
                .cast_unsigned(),
            vk::VK_IMAGE_ASPECT_COLOR_BIT.cast_unsigned(),
            vk::VK_SAMPLE_COUNT_1_BIT,
            1,
        )?;
        let depth = match create_image(
            surface,
            width,
            height,
            DEPTH_FORMAT,
            vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT.cast_unsigned(),
            vk::VK_IMAGE_ASPECT_DEPTH_BIT.cast_unsigned(),
            sample_count,
            1,
        ) {
            Ok(depth) => depth,
            Err(failure) => {
                destroy_image(surface, color);
                return Err(failure);
            }
        };
        let multisample = if sample_count == vk::VK_SAMPLE_COUNT_1_BIT {
            None
        } else {
            match create_image(
                surface,
                width,
                height,
                vk::VK_FORMAT_R16G16B16A16_SFLOAT,
                vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT.cast_unsigned(),
                vk::VK_IMAGE_ASPECT_COLOR_BIT.cast_unsigned(),
                sample_count,
                1,
            ) {
                Ok(multisample) => Some(multisample),
                Err(failure) => {
                    destroy_image(surface, depth);
                    destroy_image(surface, color);
                    return Err(failure);
                }
            }
        };
        let render = RenderAttachments {
            depth,
            multisample,
            sample_count,
            rendered: false,
        };
        let sampler_info = vk::VkSamplerCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,
            magFilter: vk::VK_FILTER_LINEAR,
            minFilter: vk::VK_FILTER_LINEAR,
            mipmapMode: vk::VK_SAMPLER_MIPMAP_MODE_NEAREST,
            addressModeU: vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
            addressModeV: vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
            addressModeW: vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
            maxAnisotropy: 1.0,
            maxLod: 0.0,
            ..Default::default()
        };
        let mut sampler = ptr::null_mut();
        if let Err(failure) = check(
            unsafe {
                surface
                    .device()
                    .functions
                    .create_sampler
                    .expect("loaded function")(
                    surface.device().handle,
                    &raw const sampler_info,
                    ptr::null(),
                    &raw mut sampler,
                )
            },
            "vkCreateSampler for render texture",
        ) {
            render.destroy(surface.device());
            destroy_image(surface, color);
            return Err(failure);
        }
        let mut resource = TextureResource::new(
            color,
            sampler,
            [width, height],
            SampledTextureFormat::Float16,
            1,
            1,
            false,
        );
        resource.render = Some(render);
        self.textures.insert(resource)
    }

    /// Whether an offscreen pass has rendered into the texture.
    pub(crate) fn render_texture_rendered(&self, id: ResourceId) -> Result<bool, GraphicsError> {
        Ok(self
            .textures
            .get(id)?
            .render
            .as_ref()
            .is_some_and(|render| render.rendered))
    }

    /// Checks each pass's target against the session's sample count and queues the passes for
    /// recording, in order, their draws being the resolved offscreen draws in the same order.
    pub(super) fn queue_offscreen_passes(
        &mut self,
        offscreen: &[OffscreenPass<'_>],
        depth_clears: &[f32],
    ) -> Result<(), GraphicsError> {
        self.pending_offscreen.clear();
        for (pass, &depth_clear) in offscreen.iter().zip(depth_clears) {
            let texture = self.textures.index_of(pass.target.texture().id())?;
            let render = self.textures[texture]
                .render
                .as_ref()
                .ok_or_else(|| error("offscreen pass target is not a render texture"))?;
            self.check_sample_count(render.sample_count, "render texture")?;
            self.pending_offscreen.push(PendingOffscreen {
                texture,
                draws: pass.records.len(),
                clear: pass.clear,
                depth_clear,
            });
        }
        Ok(())
    }

    /// Records the queued offscreen passes into the frame's command buffer, leaving each target
    /// readable by the vertex and fragment stages of everything recorded after it.
    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    pub(super) fn record_offscreen_passes(&mut self) {
        let passes = core::mem::take(&mut self.pending_offscreen);
        let command = self.surface.frame_command_buffer();
        let mut start = 0_usize;
        for pass in &passes {
            let texture = &self.textures[pass.texture];
            let render = texture
                .render
                .as_ref()
                .expect("queued targets are render textures");
            let color = texture.image;
            let extent = vk::VkExtent2D {
                width: texture.extent[0],
                height: texture.extent[1],
            };
            let sampling_stages = vk::VK_PIPELINE_STAGE_2_VERTEX_SHADER_BIT
                | vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT;
            // Earlier samplers and writers of the color, in this frame or one before it,
            // finish before the pass clears it; its old contents are discarded.
            let mut barriers = vec![
                image_barrier(
                    color.handle,
                    vk::VK_IMAGE_LAYOUT_UNDEFINED,
                    vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                    sampling_stages | vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                    vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                    vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                    vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                    color_subresource_range(),
                ),
                image_barrier(
                    render.depth.handle,
                    vk::VK_IMAGE_LAYOUT_UNDEFINED,
                    vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
                    vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                        | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
                    vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                        | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
                    vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
                    vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_READ_BIT
                        | vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
                    depth_subresource_range(),
                ),
            ];
            if let Some(multisample) = render.multisample {
                barriers.push(image_barrier(
                    multisample.handle,
                    vk::VK_IMAGE_LAYOUT_UNDEFINED,
                    vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                    vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                    vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                    vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                    vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                    color_subresource_range(),
                ));
            }
            pipeline_barriers(&self.surface, command, &barriers);
            let color_attachment = vk::VkRenderingAttachmentInfo {
                sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
                imageView: render.multisample.map_or(color.view, |image| image.view),
                imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                resolveMode: if render.multisample.is_some() {
                    vk::VK_RESOLVE_MODE_AVERAGE_BIT
                } else {
                    vk::VK_RESOLVE_MODE_NONE
                },
                resolveImageView: render.multisample.map_or(ptr::null_mut(), |_| color.view),
                resolveImageLayout: if render.multisample.is_some() {
                    vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL
                } else {
                    vk::VK_IMAGE_LAYOUT_UNDEFINED
                },
                loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
                // Multisample color lives only inside the pass; its resolve is what's kept.
                storeOp: if render.multisample.is_some() {
                    vk::VK_ATTACHMENT_STORE_OP_DONT_CARE
                } else {
                    vk::VK_ATTACHMENT_STORE_OP_STORE
                },
                clearValue: vk::VkClearValue {
                    color: vk::VkClearColorValue {
                        float32: pass.clear.components(),
                    },
                },
                ..Default::default()
            };
            let depth_attachment = vk::VkRenderingAttachmentInfo {
                sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
                imageView: render.depth.view,
                imageLayout: vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
                loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
                storeOp: vk::VK_ATTACHMENT_STORE_OP_DONT_CARE,
                clearValue: vk::VkClearValue {
                    depthStencil: vk::VkClearDepthStencilValue {
                        depth: pass.depth_clear,
                        stencil: 0,
                    },
                },
                ..Default::default()
            };
            let area = vk::VkRect2D {
                offset: vk::VkOffset2D { x: 0, y: 0 },
                extent,
            };
            let rendering = vk::VkRenderingInfo {
                sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
                renderArea: area,
                layerCount: 1,
                colorAttachmentCount: 1,
                pColorAttachments: &raw const color_attachment,
                pDepthAttachment: &raw const depth_attachment,
                ..Default::default()
            };
            let viewport = vk::VkViewport {
                x: 0.0,
                y: 0.0,
                width: extent.width as f32,
                height: extent.height as f32,
                minDepth: 0.0,
                maxDepth: 1.0,
            };
            let draws: &[ResolvedMaterialDraw] =
                &self.resolved_offscreen_draws[start..start + pass.draws];
            start += pass.draws;
            unsafe {
                let functions = &self.surface.device().functions;
                functions.cmd_begin_rendering.expect("loaded function")(
                    command,
                    &raw const rendering,
                );
                functions.cmd_set_viewport.expect("loaded function")(
                    command,
                    0,
                    1,
                    &raw const viewport,
                );
                functions.cmd_set_scissor.expect("loaded function")(command, 0, 1, &raw const area);
                self.record_material_draws(draws, false);
                functions.cmd_end_rendering.expect("loaded function")(command);
            }
            let to_sampled = image_barrier(
                color.handle,
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                sampling_stages,
                vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
                color_subresource_range(),
            );
            pipeline_barriers(&self.surface, command, &[to_sampled]);
        }
        for pass in &passes {
            if let Some(render) = self.textures[pass.texture].render.as_mut() {
                render.rendered = true;
            }
        }
    }
}
