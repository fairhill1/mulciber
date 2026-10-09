//! Ordered render encoders use tracked private textures for the bloom chain: the levels filtered
//! down from the scene, then, with an upsample filter, added back up from the smallest.
use super::{
    GraphicsError, LOAD_ACTION_DONT_CARE, LOAD_ACTION_LOAD, Object, PRIMITIVE_TYPE_TRIANGLE,
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
    // Without upsampling the composite reads six levels, so only those are filtered.
    let upsample = pipeline.bloom.get(2);
    let levels = if upsample.is_some() {
        target.bloom.len()
    } else {
        crate::graphics::BLOOM_LEVELS.min(target.bloom.len())
    };
    for level in 0..levels {
        let input = if level == 0 {
            target.scene_color
        } else {
            target.bloom[level - 1]
        };
        let filter = &pipeline.bloom[usize::from(level != 0)];
        pass(
            command,
            filter,
            input,
            target.bloom[level],
            LOAD_ACTION_DONT_CARE,
        )?;
    }
    if let Some(filter) = upsample {
        // Each level from the smallest up, added into the next larger, its contents kept.
        for level in (0..levels - 1).rev() {
            pass(
                command,
                filter,
                target.bloom[level + 1],
                target.bloom[level],
                LOAD_ACTION_LOAD,
            )?;
        }
    }
    Ok(())
}

/// One fullscreen pass of `filter` reading `input` into `output`.
fn pass(
    command: Object,
    filter: &PostprocessPipelineResource,
    input: Object,
    output: Object,
    load: usize,
) -> Result<(), GraphicsError> {
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
        objc::void_object(color, c"setTexture:", output);
        objc::void_usize(color, c"setLoadAction:", load);
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
    Ok(())
}
