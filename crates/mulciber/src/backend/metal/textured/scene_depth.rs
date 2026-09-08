//! Copy the stored opaque depth before any consumer binds it alongside the live attachment.
use super::{
    GraphicsError, Object, PIXEL_FORMAT_DEPTH32_FLOAT, PostprocessTargetResource,
    TEXTURE_USAGE_RENDER_TARGET, TEXTURE_USAGE_SHADER_READ, create_target_texture_with_storage,
    objc, ptr, required,
};

pub(super) fn ensure_target(
    device: Object,
    target: &mut PostprocessTargetResource,
    samples: usize,
) -> Result<(), GraphicsError> {
    if target.depth_snapshot.is_null() {
        let (width, height) = unsafe {
            (
                objc::usize_value(target.depth, c"width"),
                objc::usize_value(target.depth, c"height"),
            )
        };
        target.depth_snapshot = create_target_texture_with_storage(
            device,
            PIXEL_FORMAT_DEPTH32_FLOAT,
            width,
            height,
            samples,
            TEXTURE_USAGE_SHADER_READ | TEXTURE_USAGE_RENDER_TARGET,
            false,
        )?;
    }
    Ok(())
}

pub(super) fn capture(
    command: Object,
    target: &PostprocessTargetResource,
) -> Result<(), GraphicsError> {
    unsafe {
        let blit = required(
            objc::object(command, c"blitCommandEncoder"),
            "Metal scene-depth copy encoder",
        )?;
        objc::void_two_objects(
            blit,
            c"copyFromTexture:toTexture:",
            target.depth,
            target.depth_snapshot,
        );
        objc::void(blit, c"endEncoding");
    }
    Ok(())
}

pub(super) fn clear_timing(pass: Object) {
    unsafe {
        let attachments = objc::object(pass, c"sampleBufferAttachments");
        let attachment = objc::object_usize(attachments, c"objectAtIndexedSubscript:", 0);
        objc::void_object(attachment, c"setSampleBuffer:", ptr::null_mut());
    }
}
