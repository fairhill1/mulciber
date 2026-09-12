//! Synchronous one-pixel HDR readback, compiled only for repository validation.
use super::{GraphicsError, Origin3, ResourceId, Size3, TexturedSession, objc, required};

impl TexturedSession<'_> {
    pub(crate) fn read_hdr_validation_pixel(
        &mut self,
        id: ResourceId,
    ) -> Result<[u16; 4], GraphicsError> {
        self.surface.finish_all_frames()?;
        let target = self.postprocess_targets.get(id)?;
        if target.scene_color.is_null() {
            return Err(GraphicsError::stale_resource(
                "validation target was retired",
            ));
        }
        unsafe {
            let buffer = required(
                objc::object_two_usizes(
                    self.surface.device,
                    c"newBufferWithLength:options:",
                    256,
                    0,
                ),
                "validation readback buffer",
            )?;
            let result = (|| {
                let command = required(
                    objc::object(self.surface.queue, c"commandBuffer"),
                    "validation command",
                )?;
                let blit = required(
                    objc::object(command, c"blitCommandEncoder"),
                    "validation blit",
                )?;
                objc::void_copy_texture_to_buffer(blit,
                    c"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:",
                    target.scene_color, 0, 0, Origin3 { x: 0, y: 0, z: 0 },
                    Size3 { width: 1, height: 1, depth: 1 }, buffer, 0, 256, 256);
                objc::void(blit, c"endEncoding");
                objc::void(command, c"commit");
                objc::void(command, c"waitUntilCompleted");
                if objc::usize_value(command, c"status") != 4 {
                    return Err(GraphicsError::new("validation readback command failed"));
                }
                let contents = objc::object(buffer, c"contents").cast::<u16>();
                Ok([
                    *contents,
                    *contents.add(1),
                    *contents.add(2),
                    *contents.add(3),
                ])
            })();
            objc::void(buffer, c"release");
            result
        }
    }
}
