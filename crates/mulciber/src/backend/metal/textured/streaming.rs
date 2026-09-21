//! Queue-ordered blits, rather than CPU writes racing previously submitted frames.
use super::{
    GraphicsError, Object, Origin3, ResourceId, SampledTextureFormat, Size3, TexturedSession, objc,
    required,
};
use std::vec;
use std::vec::Vec;
impl TexturedSession<'_> {
    pub(crate) fn update_float_texture(
        &mut self,
        id: ResourceId,
        width: u32,
        height: u32,
        bytes: Vec<u8>,
    ) -> Result<(), GraphicsError> {
        let index = self.textures.index_of(id)?;
        let texture = &mut self.textures[index];
        if texture.extent != [width, height]
            || texture.format != SampledTextureFormat::Float16
            || texture.mip_levels != 1
        {
            return Err(GraphicsError::invalid_request(
                "float texture update requires matching dimensions and one RGBA16Float level",
            ));
        }
        texture.pending = Some(bytes);
        Ok(())
    }
    pub(super) fn encode_texture_updates(&mut self, command: Object) -> Result<(), GraphicsError> {
        for texture in self.textures.iter_mut() {
            let Some(bytes) = texture.pending.as_ref() else {
                continue;
            };
            // Metal buffer-to-texture copies require rows aligned to 256 bytes.
            let row = texture.extent[0] as usize * 8;
            let stride = row.div_ceil(256) * 256;
            let mut padded = vec![0u8; stride * texture.extent[1] as usize];
            for (source, destination) in
                bytes.chunks_exact(row).zip(padded.chunks_exact_mut(stride))
            {
                destination[..row].copy_from_slice(source);
            }
            unsafe {
                let buffer = required(
                    objc::object_bytes(
                        self.surface.device,
                        c"newBufferWithBytes:length:options:",
                        padded.as_ptr().cast(),
                        padded.len(),
                        0,
                    ),
                    "texture update staging buffer",
                )?;
                let encoder = match required(
                    objc::object(command, c"blitCommandEncoder"),
                    "texture update blit encoder",
                ) {
                    Ok(encoder) => encoder,
                    Err(error) => {
                        objc::void(buffer, c"release");
                        return Err(error);
                    }
                };
                objc::void_copy_buffer_to_texture(encoder,
                    c"copyFromBuffer:sourceOffset:sourceBytesPerRow:sourceBytesPerImage:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:",
                    buffer, 0, stride, padded.len(),
                    Size3 { width: texture.extent[0] as usize, height: texture.extent[1] as usize, depth: 1 },
                    texture.texture, 0, 0, Origin3 { x: 0, y: 0, z: 0 });
                objc::void(encoder, c"endEncoding");
                // Ordinary command buffers retain encoded resources until completion.
                objc::void(buffer, c"release");
            }
            texture.pending = None;
        }
        Ok(())
    }
}
