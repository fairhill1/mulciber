//! Ordered render encoders use tracked private textures for the bloom chain.
use super::{
    GraphicsError, LOAD_ACTION_DONT_CARE, Object, PRIMITIVE_TYPE_TRIANGLE,
    PostprocessPipelineResource, PostprocessTargetResource, STORE_ACTION_STORE, objc, required,
};

pub(super) fn encode(
    command: Object,
    pipeline: &PostprocessPipelineResource,
    target: &PostprocessTargetResource,
) -> Result<(), GraphicsError> {
    if pipeline.bloom.is_empty() {
        return Ok(());
    }
    for (level, &texture) in target.bloom.iter().enumerate() {
        let filter = &pipeline.bloom[usize::from(level != 0)];
        let input = if level == 0 {
            target.scene_color
        } else {
            target.bloom[level - 1]
        };
        unsafe {
            let pass = required(
                objc::object(
                    objc::class(c"MTLRenderPassDescriptor"),
                    c"renderPassDescriptor",
                ),
                "bloom render pass",
            )?;
            let colors = required(objc::object(pass, c"colorAttachments"), "bloom attachments")?;
            let color = required(
                objc::object_usize(colors, c"objectAtIndexedSubscript:", 0),
                "bloom color attachment",
            )?;
            objc::void_object(color, c"setTexture:", texture);
            objc::void_usize(color, c"setLoadAction:", LOAD_ACTION_DONT_CARE);
            objc::void_usize(color, c"setStoreAction:", STORE_ACTION_STORE);
            let encoder = required(
                objc::object_object(command, c"renderCommandEncoderWithDescriptor:", pass),
                "bloom render encoder",
            )?;
            objc::void_object(encoder, c"setRenderPipelineState:", filter.pipeline);
            objc::void_object_usize(encoder, c"setFragmentTexture:atIndex:", input, 1);
            objc::void_object_usize(
                encoder,
                c"setFragmentSamplerState:atIndex:",
                filter.sampler,
                2,
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
