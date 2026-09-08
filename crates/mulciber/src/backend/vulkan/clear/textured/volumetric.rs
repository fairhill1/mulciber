//! World-depth scattering, before foreground destroys world depth and before bloom.
use super::{
    Buffer, ClearSurface, GraphicsError, PipelineResource, PostprocessTargetResource, Vec, check,
    color_subresource_range, create_image, depth_subresource_range, descriptor_write,
    image_barrier, pipeline_barrier, pipeline_barriers, ptr, vk,
};

pub(super) fn validate_depth(
    surface: &ClearSurface<'_>,
    samples: vk::VkSampleCountFlagBits,
    width: u32,
    height: u32,
) -> Result<(), GraphicsError> {
    let device = surface.device();
    let mut properties = vk::VkImageFormatProperties::default();
    check(
        unsafe {
            device
                .instance
                .functions
                .get_physical_device_image_format_properties
                .expect("loaded function")(
                device.adapter.handle,
                super::DEPTH_FORMAT,
                vk::VK_IMAGE_TYPE_2D,
                vk::VK_IMAGE_TILING_OPTIMAL,
                (vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT
                    | vk::VK_IMAGE_USAGE_SAMPLED_BIT
                    | vk::VK_IMAGE_USAGE_TRANSFER_SRC_BIT
                    | vk::VK_IMAGE_USAGE_TRANSFER_DST_BIT)
                    .cast_unsigned(),
                0,
                &raw mut properties,
            )
        },
        "sampleable HDR depth format",
    )?;
    if properties.sampleCounts & samples.cast_unsigned() == 0
        || width > properties.maxExtent.width
        || height > properties.maxExtent.height
    {
        return Err(super::error(
            "HDR depth cannot be sampled at the selected MSAA count or extent",
        ));
    }
    Ok(())
}

pub(super) struct DescriptorSets {
    composite: vk::VkDescriptorSet,
    shadow: vk::VkImageView,
    sets: [vk::VkDescriptorSet; 2],
}

pub(super) fn ensure_target(
    surface: &ClearSurface<'_>,
    target: &mut PostprocessTargetResource,
) -> Result<(), GraphicsError> {
    if target.scattering.is_none() {
        let extent = target.scene_extent;
        target.scattering = Some(create_image(
            surface,
            (extent.width / 2).max(1),
            (extent.height / 2).max(1),
            vk::VK_FORMAT_R16G16B16A16_SFLOAT,
            (vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | vk::VK_IMAGE_USAGE_SAMPLED_BIT) as u32,
            vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
            vk::VK_SAMPLE_COUNT_1_BIT,
            1,
        )?);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_descriptors(
    surface: &ClearSurface<'_>,
    pipeline: &mut PipelineResource,
    target: &PostprocessTargetResource,
    composite: vk::VkDescriptorSet,
    shadow: vk::VkImageView,
    uniform: &Buffer,
    base: usize,
) -> Result<(), GraphicsError> {
    if pipeline
        .volume_sets
        .iter()
        .any(|s| s.composite == composite && s.shadow == shadow)
    {
        return Ok(());
    }
    let device = surface.device();
    let mut sets = [ptr::null_mut(); 2];
    for (index, set) in sets.iter_mut().enumerate() {
        let stage = &pipeline.volume[index];
        let info = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: pipeline.descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const stage.set_layout,
            ..Default::default()
        };
        check(
            unsafe {
                device
                    .functions
                    .allocate_descriptor_sets
                    .expect("loaded function")(device.handle, &raw const info, set)
            },
            "volumetric descriptors",
        )?;
        let buffer = vk::VkDescriptorBufferInfo {
            buffer: uniform.handle,
            offset: base as u64,
            range: u64::from(pipeline.uniform_size),
        };
        let depth = vk::VkDescriptorImageInfo {
            imageView: target.depth.expect("live depth").view,
            imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            ..Default::default()
        };
        let input = vk::VkDescriptorImageInfo {
            imageView: if index == 0 {
                shadow
            } else {
                target.scattering.expect("scattering target").view
            },
            imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            ..Default::default()
        };
        let writes = [
            descriptor_write(
                *set,
                0,
                vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
                (&raw const buffer).cast(),
            ),
            descriptor_write(
                *set,
                1,
                vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
                (&raw const depth).cast(),
            ),
            descriptor_write(
                *set,
                2,
                vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
                (&raw const input).cast(),
            ),
        ];
        unsafe {
            device
                .functions
                .update_descriptor_sets
                .expect("loaded function")(
                device.handle, 3, writes.as_ptr(), 0, ptr::null()
            );
        }
    }
    // One live pair per composite/slot: shadow changes replace its lookup, not its pool lifetime.
    pipeline.volume_sets.retain(|s| s.composite != composite);
    pipeline.volume_sets.push(DescriptorSets {
        composite,
        shadow,
        sets,
    });
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::cast_precision_loss
)]
pub(super) fn record(
    surface: &ClearSurface<'_>,
    pipeline: &PipelineResource,
    target: &PostprocessTargetResource,
    composite: vk::VkDescriptorSet,
    scene_attachment: vk::VkRenderingAttachmentInfo,
    area: vk::VkRect2D,
    viewport: vk::VkViewport,
) {
    if pipeline.volume.is_empty() {
        return;
    }
    let sets = &pipeline
        .volume_sets
        .iter()
        .find(|s| s.composite == composite)
        .expect("volume descriptors prepared")
        .sets;
    let depth = target.depth.expect("live depth");
    let scattering = target.scattering.expect("live scattering");
    let cmd = surface.frame_command_buffer();
    let read_depth = image_barrier(
        depth.handle,
        vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
        vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
            | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
        vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
        vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
        vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
        depth_subresource_range(),
    );
    let write_scattering = image_barrier(
        scattering.handle,
        vk::VK_IMAGE_LAYOUT_UNDEFINED,
        vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
        vk::VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
        vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
        vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT | vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
        vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
        color_subresource_range(),
    );
    pipeline_barriers(surface, cmd, &[read_depth, write_scattering]);
    for (index, stage) in pipeline.volume.iter().enumerate() {
        let attachment = if index == 0 {
            vk::VkRenderingAttachmentInfo {
                sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
                imageView: scattering.view,
                imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                loadOp: vk::VK_ATTACHMENT_LOAD_OP_DONT_CARE,
                storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
                ..Default::default()
            }
        } else {
            // Both MSAA color and its resolve destination were written by the world scope.
            let barriers: Vec<_> = target
                .multisample_color
                .iter()
                .chain(target.scene_color.iter())
                .map(|image| {
                    image_barrier(
                        image.handle,
                        vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                        vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                        vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                        vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                        vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                        vk::VK_ACCESS_2_COLOR_ATTACHMENT_READ_BIT
                            | vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                        color_subresource_range(),
                    )
                })
                .collect();
            pipeline_barriers(surface, cmd, &barriers);
            vk::VkRenderingAttachmentInfo {
                loadOp: vk::VK_ATTACHMENT_LOAD_OP_LOAD,
                ..scene_attachment
            }
        };
        let stage_area = if index == 0 {
            vk::VkRect2D {
                extent: vk::VkExtent2D {
                    width: (area.extent.width / 2).max(1),
                    height: (area.extent.height / 2).max(1),
                },
                ..area
            }
        } else {
            area
        };
        let stage_viewport = vk::VkViewport {
            width: stage_area.extent.width as f32,
            height: stage_area.extent.height as f32,
            ..viewport
        };
        let rendering = vk::VkRenderingInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
            renderArea: stage_area,
            layerCount: 1,
            colorAttachmentCount: 1,
            pColorAttachments: &raw const attachment,
            ..Default::default()
        };
        unsafe {
            let f = &surface.device().functions;
            f.cmd_begin_rendering.expect("loaded function")(cmd, &raw const rendering);
            f.cmd_bind_pipeline.expect("loaded function")(
                cmd,
                vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                stage.pipeline,
            );
            f.cmd_bind_descriptor_sets.expect("loaded function")(
                cmd,
                vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                stage.layout,
                0,
                1,
                &raw const sets[index],
                0,
                ptr::null(),
            );
            f.cmd_set_viewport.expect("loaded function")(cmd, 0, 1, &raw const stage_viewport);
            f.cmd_set_scissor.expect("loaded function")(cmd, 0, 1, &raw const stage_area);
            f.cmd_draw.expect("loaded function")(cmd, 3, 1, 0, 0);
            f.cmd_end_rendering.expect("loaded function")(cmd);
        }
        if index == 0 {
            let read = image_barrier(
                scattering.handle,
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
                vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
                color_subresource_range(),
            );
            pipeline_barrier(surface, cmd, &read);
        }
    }
    // End the sampling lifetime before foreground clears/reuses the same allocation.
    let restore = image_barrier(
        depth.handle,
        vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
        vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
        vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
            | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
        vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
        vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT
            | vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_READ_BIT,
        depth_subresource_range(),
    );
    pipeline_barrier(surface, cmd, &restore);
}
