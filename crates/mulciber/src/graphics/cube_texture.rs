//! Cube texture input validation, independent of the native backend.
use super::sampled_texture::checked_staging_size;
use super::{GraphicsError, SampledTextureFormat, format, full_mip_chain_len, mip_extent};

/// Face names in the standard cube layer order, which both native APIs share: +X, -X, +Y, -Y,
/// +Z, -Z.
pub(super) const FACE_NAMES: [&str; 6] = ["+X", "-X", "+Y", "-Y", "+Z", "-Z"];

/// Requires six faces of one nonzero square extent, each either a single level or the complete
/// chain to 1×1, whose every level's byte count matches its extent in the format's texels or
/// blocks. Diagnostics name the face and level, and the summed staging size is checked before
/// any native allocation.
pub(super) fn validate_faces(
    format: SampledTextureFormat,
    size: u32,
    faces: &[&[&[u8]]; 6],
    complete: bool,
) -> Result<(), GraphicsError> {
    if size == 0 {
        return Err(GraphicsError::invalid_request(
            "cube texture face extent must be nonzero",
        ));
    }
    let expected_levels = if complete {
        full_mip_chain_len(size, size)
    } else {
        1
    };
    let mut total = 0_usize;
    for (name, levels) in FACE_NAMES.iter().zip(faces) {
        if levels.len() != expected_levels {
            return Err(GraphicsError::invalid_request(format!(
                "cube face {name} supplies {} mip levels but a {size}x{size} face needs \
                 {expected_levels}{}",
                levels.len(),
                if complete { " to reach 1x1" } else { "" }
            )));
        }
        for (level, bytes) in (0_u32..).zip(levels.iter()) {
            let extent = mip_extent(size, level);
            let expected = format
                .level_bytes(extent, extent)
                .filter(|size| *size <= isize::MAX.cast_unsigned())
                .ok_or_else(|| {
                    GraphicsError::invalid_request("cube texture dimensions overflow address space")
                })?;
            if bytes.len() != expected {
                return Err(GraphicsError::invalid_request(format!(
                    "cube face {name} mip level {level} supplies {} bytes but its \
                     {extent}x{extent} extent needs {expected}",
                    bytes.len()
                )));
            }
            total = checked_staging_size([total, expected])?;
        }
    }
    Ok(())
}

/// Prefixes a per-face packing diagnostic with the face it came from, keeping its kind.
pub(super) fn name_face(name: &str, error: &GraphicsError) -> GraphicsError {
    GraphicsError::with_kind(
        error.kind(),
        format!("cube face {name}: {}", error.message()),
    )
}

#[cfg(test)]
mod tests {
    use super::{FACE_NAMES, validate_faces};
    use crate::GraphicsErrorKind;
    use crate::graphics::{BlockCompression, SampledTextureFormat};

    fn faces<'a>(levels: &'a [&'a [u8]]) -> [&'a [&'a [u8]]; 6] {
        [levels; 6]
    }

    #[test]
    fn six_equal_square_faces_are_accepted_in_every_format() {
        let rgba = [0_u8; 4 * 4 * 4];
        assert!(validate_faces(SampledTextureFormat::Srgb, 4, &faces(&[&rgba]), false).is_ok());
        assert!(validate_faces(SampledTextureFormat::Unorm, 4, &faces(&[&rgba]), false).is_ok());
        let half = [0_u8; 4 * 4 * 8];
        assert!(validate_faces(SampledTextureFormat::Float16, 4, &faces(&[&half]), false).is_ok());
        // A 4x4 face is one block: eight bytes in BC1, sixteen in BC3.
        let block = [0_u8; 16];
        let bc1 = BlockCompression::Bc1Srgb.sampled();
        let bc3 = BlockCompression::Bc3Unorm.sampled();
        assert!(validate_faces(bc1, 4, &faces(&[&block[..8]]), false).is_ok());
        assert!(validate_faces(bc3, 4, &faces(&[&block]), false).is_ok());
        assert!(validate_faces(bc1, 4, &faces(&[&block]), false).is_err());
    }

    #[test]
    fn a_zero_extent_or_a_mismatched_face_is_rejected_by_name() {
        let rgba = [0_u8; 2 * 2 * 4];
        let level: [&[u8]; 1] = [&rgba];
        let empty: [&[u8]; 1] = [&[]];
        assert!(validate_faces(SampledTextureFormat::Unorm, 0, &faces(&empty), false).is_err());
        // One face a texel short: the diagnostic names it.
        let short: [&[u8]; 1] = [&rgba[..12]];
        for (index, name) in FACE_NAMES.iter().enumerate() {
            let mut cube = faces(&level);
            cube[index] = &short;
            let error = validate_faces(SampledTextureFormat::Unorm, 2, &cube, false)
                .expect_err("a short face is rejected");
            assert_eq!(error.kind(), GraphicsErrorKind::InvalidRequest);
            assert!(error.message().contains(&std::format!("cube face {name} ")));
        }
        // A face sized for a larger extent is not square at this extent.
        let wide = [0_u8; 4 * 2 * 4];
        let wide_face: [&[u8]; 1] = [&wide];
        let mut cube = faces(&level);
        cube[3] = &wide_face;
        assert!(validate_faces(SampledTextureFormat::Unorm, 2, &cube, false).is_err());
    }

    #[test]
    fn every_face_supplies_the_complete_chain() {
        // 4x4 RGBA8: 64, 16, then 4 bytes.
        let base = [0_u8; 64];
        let chain: [&[u8]; 3] = [&base, &base[..16], &base[..4]];
        assert!(validate_faces(SampledTextureFormat::Srgb, 4, &faces(&chain), true).is_ok());
        assert!(validate_faces(SampledTextureFormat::Srgb, 4, &faces(&chain[..2]), true).is_err());
        let mut cube = faces(&chain);
        cube[5] = &chain[..2];
        let error = validate_faces(SampledTextureFormat::Srgb, 4, &cube, true)
            .expect_err("one short chain is rejected");
        assert!(error.message().contains("cube face -Z"));
        // A single-level upload takes exactly one level per face.
        assert!(validate_faces(SampledTextureFormat::Srgb, 4, &faces(&chain), false).is_err());
    }

    #[test]
    fn a_compressed_cube_chain_is_measured_in_blocks() {
        // 8x8 BC1: four blocks, then one block at 4x4, 2x2 and 1x1.
        let blocks = [0_u8; 32];
        let chain: [&[u8]; 4] = [&blocks, &blocks[..8], &blocks[..8], &blocks[..8]];
        let bc1 = BlockCompression::Bc1Unorm.sampled();
        assert!(validate_faces(bc1, 8, &faces(&chain), true).is_ok());
        let mut cube = faces(&chain);
        let sixteen: [&[u8]; 4] = [&blocks, &blocks[..8], &blocks[..8], &blocks[..16]];
        cube[2] = &sixteen;
        let error = validate_faces(bc1, 8, &cube, true).expect_err("a sixteen-byte BC1 tail");
        assert!(error.message().contains("cube face +Y mip level 3"));
        // BC7 at the same extent takes sixteen bytes per block.
        let bc7 = BlockCompression::Bc7Srgb.sampled();
        assert!(validate_faces(bc7, 8, &faces(&chain), true).is_err());
    }
}
