//! A frame-local copy preserves every opaque MSAA sample while translucents test live depth.
use super::{
    ClearSurface, DEPTH_FORMAT, GraphicsError, Image, PostprocessTargetResource,
    color_subresource_range, create_image, depth_subresource_range, image_barrier,
    pipeline_barriers, vk,
};

pub(super) fn ensure_target(
    surface: &ClearSurface<'_>,
    target: &mut PostprocessTargetResource,
    samples: vk::VkSampleCountFlagBits,
) -> Result<(), GraphicsError> {
    if target.depth_snapshot.is_none() {
        let extent = target.scene_extent;
        target.depth_snapshot = Some(create_image(
            surface,
            extent.width,
            extent.height,
            DEPTH_FORMAT,
            (vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT
                | vk::VK_IMAGE_USAGE_SAMPLED_BIT
                | vk::VK_IMAGE_USAGE_TRANSFER_DST_BIT)
                .cast_unsigned(),
            vk::VK_IMAGE_ASPECT_DEPTH_BIT.cast_unsigned(),
            samples,
            1,
        )?);
    }
    Ok(())
}

pub(super) fn capture(surface: &ClearSurface<'_>, target: &PostprocessTargetResource) {
    let source = target.depth.expect("live world depth");
    let snapshot = target.depth_snapshot.expect("allocated snapshot");
    let cmd = surface.frame_command_buffer();
    pipeline_barriers(
        surface,
        cmd,
        &[
            image_barrier(
                source.handle,
                vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
                vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                    | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
                vk::VK_ACCESS_2_TRANSFER_READ_BIT,
                depth_subresource_range(),
            ),
            image_barrier(
                snapshot.handle,
                vk::VK_IMAGE_LAYOUT_UNDEFINED,
                vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
                vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
                depth_subresource_range(),
            ),
        ],
    );
    let subresource = vk::VkImageSubresourceLayers {
        aspectMask: vk::VK_IMAGE_ASPECT_DEPTH_BIT.cast_unsigned(),
        mipLevel: 0,
        baseArrayLayer: 0,
        layerCount: 1,
    };
    let region = vk::VkImageCopy2 {
        sType: vk::VK_STRUCTURE_TYPE_IMAGE_COPY_2,
        srcSubresource: subresource,
        dstSubresource: subresource,
        extent: vk::VkExtent3D {
            width: target.scene_extent.width,
            height: target.scene_extent.height,
            depth: 1,
        },
        ..Default::default()
    };
    let copy = vk::VkCopyImageInfo2 {
        sType: vk::VK_STRUCTURE_TYPE_COPY_IMAGE_INFO_2,
        srcImage: source.handle,
        srcImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
        dstImage: snapshot.handle,
        dstImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        regionCount: 1,
        pRegions: &raw const region,
        ..Default::default()
    };
    unsafe {
        surface
            .device()
            .functions
            .cmd_copy_image2
            .expect("loaded function")(cmd, &raw const copy);
    }
    pipeline_barriers(
        surface,
        cmd,
        &[
            image_barrier(
                source.handle,
                vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                    | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
                vk::VK_ACCESS_2_TRANSFER_READ_BIT,
                vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_READ_BIT
                    | vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
                depth_subresource_range(),
            ),
            image_barrier(
                snapshot.handle,
                vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_PIPELINE_STAGE_2_VERTEX_SHADER_BIT
                    | vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
                vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
                vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
                depth_subresource_range(),
            ),
        ],
    );
}

/// Both MSAA color and its resolve destination were written by the first scope.
pub(super) fn continue_color(surface: &ClearSurface<'_>, color: Image, msaa: Option<Image>) {
    let barrier = |image: Image| {
        image_barrier(
            image.handle,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_READ_BIT | vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            color_subresource_range(),
        )
    };
    let barriers = [barrier(color), barrier(msaa.unwrap_or(color))];
    pipeline_barriers(
        surface,
        surface.frame_command_buffer(),
        &barriers[..if msaa.is_some() { 2 } else { 1 }],
    );
}
