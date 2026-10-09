//! Native HDR bloom storage dependencies and fullscreen downsampling, and upsampling back up.
use std::vec::Vec;

use super::{
    ClearSurface, GraphicsError, Image, PipelineResource, PostprocessTargetResource, check,
    color_subresource_range, descriptor_write, error, image_barrier, pipeline_barrier, ptr, vk,
};

pub(super) fn validate_format(
    surface: &ClearSurface<'_>,
    samples: vk::VkSampleCountFlagBits,
    width: u32,
    height: u32,
) -> Result<(), GraphicsError> {
    let mut properties = vk::VkFormatProperties::default();
    unsafe {
        surface
            .device()
            .instance
            .functions
            .get_physical_device_format_properties
            .expect("loaded function")(
            surface.device().adapter.handle,
            vk::VK_FORMAT_R16G16B16A16_SFLOAT,
            &raw mut properties,
        );
    }
    let required = (vk::VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT
        | vk::VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BLEND_BIT
        | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT
        | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR_BIT) as u32;
    if properties.optimalTilingFeatures & required != required {
        return Err(error(
            "HDR requires renderable, blendable, linearly filterable RGBA16Float",
        ));
    }
    for (usage, count) in [
        (
            vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | vk::VK_IMAGE_USAGE_SAMPLED_BIT,
            vk::VK_SAMPLE_COUNT_1_BIT,
        ),
        (vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT, samples),
    ] {
        let mut image = vk::VkImageFormatProperties::default();
        check(
            unsafe {
                surface
                    .device()
                    .instance
                    .functions
                    .get_physical_device_image_format_properties
                    .expect("loaded function")(
                    surface.device().adapter.handle,
                    vk::VK_FORMAT_R16G16B16A16_SFLOAT,
                    vk::VK_IMAGE_TYPE_2D,
                    vk::VK_IMAGE_TILING_OPTIMAL,
                    usage.cast_unsigned(),
                    0,
                    &raw mut image,
                )
            },
            "HDR RGBA16Float image format support",
        )?;
        if image.sampleCounts & count.cast_unsigned() == 0
            || width > image.maxExtent.width
            || height > image.maxExtent.height
        {
            return Err(error(
                "HDR RGBA16Float does not support the selected MSAA count or scene extent",
            ));
        }
    }
    Ok(())
}

pub(super) fn descriptor_pool(
    device: &super::super::Device,
) -> Result<vk::VkDescriptorPool, GraphicsError> {
    let sizes = [
        vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
        vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
        vk::VK_DESCRIPTOR_TYPE_SAMPLER,
    ]
    .map(|ty| vk::VkDescriptorPoolSize {
        type_: ty,
        descriptorCount: 4096,
    });
    let info = vk::VkDescriptorPoolCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
        maxSets: 1024,
        poolSizeCount: 3,
        pPoolSizes: sizes.as_ptr(),
        ..Default::default()
    };
    let mut pool = ptr::null_mut();
    check(
        unsafe {
            device
                .functions
                .create_descriptor_pool
                .expect("loaded function")(
                device.handle,
                &raw const info,
                ptr::null(),
                &raw mut pool,
            )
        },
        "vkCreateDescriptorPool for HDR postprocess",
    )?;
    Ok(pool)
}

/// The number of levels a pipeline's bloom filters: six without upsampling, all with it.
fn filtered(pipeline: &PipelineResource, levels: usize) -> usize {
    if pipeline.bloom.len() > 2 {
        levels
    } else {
        crate::graphics::BLOOM_LEVELS.min(levels)
    }
}

/// A set for each downsample pass (its input the scene or the level before), then, with an
/// upsample filter, each upsample pass from the smallest level up (its input the level after).
pub(super) fn descriptor_sets(
    surface: &ClearSurface<'_>,
    pipeline: &mut PipelineResource,
    scene: Image,
    levels: &[Image],
) -> Result<Vec<vk::VkDescriptorSet>, GraphicsError> {
    let device = surface.device();
    let count = filtered(pipeline, levels.len());
    let down = (0..count).map(|level| {
        let input = if level == 0 { scene } else { levels[level - 1] };
        (usize::from(level != 0), input)
    });
    let up = (0..count.saturating_sub(1))
        .rev()
        .filter(|_| pipeline.bloom.len() > 2)
        .map(|level| (2, levels[level + 1]));
    let passes: Vec<(usize, Image)> = down.chain(up).collect();
    let mut sets = Vec::with_capacity(passes.len());
    for (filter, input) in passes {
        let (set_layout, sampler) = {
            let filter = &pipeline.bloom[filter];
            (filter.set_layout, filter.sampler)
        };
        let set = pipeline.descriptor_pools.allocate(
            device,
            set_layout,
            "vkAllocateDescriptorSets for bloom level",
        )?;
        let image = vk::VkDescriptorImageInfo {
            imageView: input.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            ..Default::default()
        };
        let sampler = vk::VkDescriptorImageInfo {
            sampler,
            ..Default::default()
        };
        let writes = [
            descriptor_write(
                set,
                1,
                vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
                (&raw const image).cast(),
            ),
            descriptor_write(
                set,
                2,
                vk::VK_DESCRIPTOR_TYPE_SAMPLER,
                (&raw const sampler).cast(),
            ),
        ];
        unsafe {
            device
                .functions
                .update_descriptor_sets
                .expect("loaded function")(
                device.handle, 2, writes.as_ptr(), 0, ptr::null()
            );
        }
        sets.push(set);
    }
    Ok(sets)
}

pub(super) fn record(
    surface: &ClearSurface<'_>,
    pipeline: &PipelineResource,
    target: &PostprocessTargetResource,
    composite_set: vk::VkDescriptorSet,
) {
    if pipeline.bloom.is_empty() {
        return;
    }
    let sets = &pipeline
        .bloom_sets
        .iter()
        .find(|(set, _)| *set == composite_set)
        .expect("prepared bloom descriptors")
        .1;
    let extents =
        crate::graphics::bloom_extents(target.scene_extent.width, target.scene_extent.height);
    let count = filtered(pipeline, target.bloom.len());
    for level in 0..count {
        let image = &target.bloom[level];
        // These images persist across frames. Discard contents only after preceding shader reads
        // finish, including reads by the final composite in the previous submission.
        let write = image_barrier(
            image.handle,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT
                | vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT
                | vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            color_subresource_range(),
        );
        pipeline_barrier(surface, surface.frame_command_buffer(), &write);
        let filter = &pipeline.bloom[usize::from(level != 0)];
        draw(
            surface,
            filter,
            sets[level],
            image,
            extents[level],
            vk::VK_ATTACHMENT_LOAD_OP_DONT_CARE,
        );
        read_after_write(surface, image);
    }
    if pipeline.bloom.len() > 2 {
        // Each level from the smallest up, added into the next larger: its downsampled contents
        // are kept, and the blend reads them, once the pass that sampled it to filter the next
        // level down has finished.
        for (pass, level) in (0..count.saturating_sub(1)).rev().enumerate() {
            let image = &target.bloom[level];
            let add = image_barrier(
                image.handle,
                vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
                vk::VK_ACCESS_2_COLOR_ATTACHMENT_READ_BIT
                    | vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                color_subresource_range(),
            );
            pipeline_barrier(surface, surface.frame_command_buffer(), &add);
            draw(
                surface,
                &pipeline.bloom[2],
                sets[count + pass],
                image,
                extents[level],
                vk::VK_ATTACHMENT_LOAD_OP_LOAD,
            );
            read_after_write(surface, image);
        }
    }
}

/// Makes what was just drawn into `image` readable by the fragment shaders after it.
fn read_after_write(surface: &ClearSurface<'_>, image: &Image) {
    let read = image_barrier(
        image.handle,
        vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
        vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
        vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
        vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
        vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
        color_subresource_range(),
    );
    pipeline_barrier(surface, surface.frame_command_buffer(), &read);
}

/// One fullscreen pass of `filter`, its input in `set`, into `image` of `extent`.
#[allow(clippy::cast_precision_loss)] // Device-limited image extents are exactly representable.
fn draw(
    surface: &ClearSurface<'_>,
    filter: &PipelineResource,
    set: vk::VkDescriptorSet,
    image: &Image,
    (width, height): (u32, u32),
    load: vk::VkAttachmentLoadOp,
) {
    let attachment = vk::VkRenderingAttachmentInfo {
        sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
        imageView: image.view,
        imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
        loadOp: load,
        storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
        ..Default::default()
    };
    let area = vk::VkRect2D {
        offset: vk::VkOffset2D { x: 0, y: 0 },
        extent: vk::VkExtent2D { width, height },
    };
    let rendering = vk::VkRenderingInfo {
        sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
        renderArea: area,
        layerCount: 1,
        colorAttachmentCount: 1,
        pColorAttachments: &raw const attachment,
        ..Default::default()
    };
    let viewport = vk::VkViewport {
        x: 0.0,
        y: 0.0,
        width: width as f32,
        height: height as f32,
        minDepth: 0.0,
        maxDepth: 1.0,
    };
    unsafe {
        let cmd = surface.frame_command_buffer();
        let f = &surface.device().functions;
        f.cmd_begin_rendering.expect("loaded function")(cmd, &raw const rendering);
        f.cmd_bind_pipeline.expect("loaded function")(
            cmd,
            vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
            filter.pipeline,
        );
        f.cmd_bind_descriptor_sets.expect("loaded function")(
            cmd,
            vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
            filter.layout,
            0,
            1,
            &raw const set,
            0,
            ptr::null(),
        );
        f.cmd_set_viewport.expect("loaded function")(cmd, 0, 1, &raw const viewport);
        f.cmd_set_scissor.expect("loaded function")(cmd, 0, 1, &raw const area);
        f.cmd_draw.expect("loaded function")(cmd, 3, 1, 0, 0);
        f.cmd_end_rendering.expect("loaded function")(cmd);
    }
}
