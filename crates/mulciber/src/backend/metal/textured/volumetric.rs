//! Tracked private world depth survives until scattering and additive upscaling finish.
use super::{
    GraphicsError, LOAD_ACTION_DONT_CARE, LOAD_ACTION_LOAD, Object, PIXEL_FORMAT_RGBA16_FLOAT,
    PRIMITIVE_TYPE_TRIANGLE, PostprocessPipelineResource, PostprocessTargetResource,
    STORE_ACTION_STORE, TEXTURE_USAGE_RENDER_TARGET, TEXTURE_USAGE_SHADER_READ,
    create_target_texture, objc, required, store_scene_color,
};

pub(super) fn ensure_target(
    device: Object,
    target: &mut PostprocessTargetResource,
) -> Result<(), GraphicsError> {
    if target.scattering.is_null() {
        let (width, height) = unsafe {
            (
                objc::usize_value(target.scene_color, c"width"),
                objc::usize_value(target.scene_color, c"height"),
            )
        };
        target.scattering = create_target_texture(
            device,
            PIXEL_FORMAT_RGBA16_FLOAT,
            (width / 2).max(1),
            (height / 2).max(1),
            1,
            TEXTURE_USAGE_RENDER_TARGET | TEXTURE_USAGE_SHADER_READ,
        )?;
    }
    Ok(())
}

pub(super) fn encode(
    command: Object,
    pipeline: &PostprocessPipelineResource,
    target: &PostprocessTargetResource,
    shadow: Object,
    uniform: &[u8],
    foreground: bool,
) -> Result<(), GraphicsError> {
    for (index, stage) in pipeline.volume.iter().enumerate() {
        unsafe {
            let pass = required(
                objc::object(
                    objc::class(c"MTLRenderPassDescriptor"),
                    c"renderPassDescriptor",
                ),
                "volumetric pass",
            )?;
            let colors = required(
                objc::object(pass, c"colorAttachments"),
                "volumetric attachments",
            )?;
            let color = required(
                objc::object_usize(colors, c"objectAtIndexedSubscript:", 0),
                "volumetric color",
            )?;
            if index == 0 {
                objc::void_object(color, c"setTexture:", target.scattering);
                objc::void_usize(color, c"setLoadAction:", LOAD_ACTION_DONT_CARE);
                objc::void_usize(color, c"setStoreAction:", STORE_ACTION_STORE);
            } else {
                objc::void_usize(color, c"setLoadAction:", LOAD_ACTION_LOAD);
                store_scene_color(color, target, !foreground);
            }
            let encoder = required(
                objc::object_object(command, c"renderCommandEncoderWithDescriptor:", pass),
                "volumetric encoder",
            )?;
            objc::void_object(encoder, c"setRenderPipelineState:", stage.pipeline);
            objc::void_object_usize(encoder, c"setFragmentTexture:atIndex:", target.depth, 1);
            objc::void_object_usize(
                encoder,
                c"setFragmentTexture:atIndex:",
                if index == 0 {
                    shadow
                } else {
                    target.scattering
                },
                2,
            );
            objc::void_bytes_usize_usize(
                encoder,
                c"setFragmentBytes:length:atIndex:",
                uniform.as_ptr().cast(),
                uniform.len(),
                0,
            );
            objc::void_three_usizes(
                encoder,
                c"drawPrimitives:vertexStart:vertexCount:",
                PRIMITIVE_TYPE_TRIANGLE,
                0,
                3,
            );
            objc::void(encoder, c"endEncoding");
        }
    }
    Ok(())
}
