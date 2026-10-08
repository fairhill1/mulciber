//! Queue-ordered blits, rather than CPU writes racing previously submitted frames.
use super::{
    GraphicsError, Object, Origin3, ResourceId, SampledTextureFormat, Size3, TexturedSession,
    mip_extent, objc, required,
};
use std::vec;
use std::{format, vec::Vec};

/// Where one mip level sits in the padded staging buffer.
struct LevelCopy {
    offset: usize,
    /// Tightly packed source row.
    row: usize,
    /// Padded staging row.
    stride: usize,
    width: usize,
    height: usize,
}

impl TexturedSession<'_> {
    /// Queues the texture's whole chain, one tightly packed byte vector per level.
    pub(crate) fn update_float_texture(
        &mut self,
        id: ResourceId,
        width: u32,
        height: u32,
        levels: Vec<Vec<u8>>,
    ) -> Result<(), GraphicsError> {
        let index = self.textures.index_of(id)?;
        let texture = &mut self.textures[index];
        if texture.extent != [width, height]
            || texture.format != SampledTextureFormat::Float16
            || texture.slices != 1
        {
            return Err(GraphicsError::invalid_request(
                "float texture update requires a 2D RGBA16Float texture of matching dimensions",
            ));
        }
        if levels.len() != texture.mip_levels {
            return Err(GraphicsError::invalid_request(format!(
                "float texture update supplies {} mip levels but the texture has {}",
                levels.len(),
                texture.mip_levels
            )));
        }
        texture.pending = Some(levels);
        Ok(())
    }
    pub(super) fn encode_texture_updates(&mut self, command: Object) -> Result<(), GraphicsError> {
        for texture in self.textures.iter_mut() {
            let Some(levels) = texture.pending.as_ref() else {
                continue;
            };
            // Metal buffer-to-texture copies require rows aligned to 256 bytes. Each level keeps
            // its own padded stride and starts where the previous level's padded rows end, so
            // every source offset is also 256-byte (and so texel) aligned.
            let mut copies = Vec::with_capacity(levels.len());
            let mut length = 0_usize;
            for (level, _) in (0_u32..).zip(levels) {
                let width = mip_extent(texture.extent[0], level) as usize;
                let height = mip_extent(texture.extent[1], level) as usize;
                let row = width * 8;
                let stride = row.div_ceil(256) * 256;
                let end = stride
                    .checked_mul(height)
                    .and_then(|image| image.checked_add(length))
                    .filter(|end| *end <= isize::MAX.cast_unsigned())
                    .ok_or_else(|| {
                        GraphicsError::invalid_request("texture update staging size overflow")
                    })?;
                copies.push(LevelCopy {
                    offset: length,
                    row,
                    stride,
                    width,
                    height,
                });
                length = end;
            }
            let mut padded = vec![0u8; length];
            for (copy, bytes) in copies.iter().zip(levels) {
                let image = &mut padded[copy.offset..copy.offset + copy.stride * copy.height];
                for (source, destination) in bytes
                    .chunks_exact(copy.row)
                    .zip(image.chunks_exact_mut(copy.stride))
                {
                    destination[..copy.row].copy_from_slice(source);
                }
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
                for (level, copy) in copies.iter().enumerate() {
                    objc::void_copy_buffer_to_texture(encoder,
                        c"copyFromBuffer:sourceOffset:sourceBytesPerRow:sourceBytesPerImage:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:",
                        buffer, copy.offset, copy.stride, copy.stride * copy.height,
                        Size3 { width: copy.width, height: copy.height, depth: 1 },
                        texture.texture, 0, level, Origin3 { x: 0, y: 0, z: 0 });
                }
                objc::void(encoder, c"endEncoding");
                // Ordinary command buffers retain encoded resources until completion.
                objc::void(buffer, c"release");
            }
            texture.pending = None;
        }
        Ok(())
    }
}
