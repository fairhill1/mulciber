//! Cube texture and cube texture array input validation, independent of the native backend.
use super::sampled_texture::checked_staging_size;
use super::{GraphicsError, SampledTextureFormat, Vec, format, full_mip_chain_len, mip_extent};

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

/// Requires a nonzero face extent and at least one cube in a cube texture array.
pub(super) fn require_layers(size: u32, layers: usize) -> Result<(), GraphicsError> {
    if layers == 0 {
        return Err(GraphicsError::invalid_request(
            "cube texture array needs at least one layer",
        ));
    }
    if size == 0 {
        return Err(GraphicsError::invalid_request(
            "cube texture array face extent must be nonzero",
        ));
    }
    Ok(())
}

/// Requires at least one cube, each of which [`validate_faces`] accepts at the same extent,
/// with diagnostics naming the layer as well as the face and level, and checks the staging size
/// summed over every layer before any native allocation.
pub(super) fn validate_layers(
    format: SampledTextureFormat,
    size: u32,
    layers: &[[&[&[u8]]; 6]],
    complete: bool,
) -> Result<(), GraphicsError> {
    require_layers(size, layers.len())?;
    for (layer, faces) in layers.iter().enumerate() {
        validate_faces(format, size, faces, complete).map_err(|error| name_layer(layer, &error))?;
    }
    checked_staging_size(
        layers
            .iter()
            .flatten()
            .flat_map(|levels| levels.iter().map(|bytes| bytes.len())),
    )?;
    Ok(())
}

/// Packs every face of every cube with `pack`, naming the layer and face of the first failure.
pub(super) fn pack_layers<T>(
    layers: &[[&[&[T]]; 6]],
    pack: impl Fn(&[&[T]]) -> Result<Vec<Vec<u8>>, GraphicsError>,
) -> Result<Vec<[Vec<Vec<u8>>; 6]>, GraphicsError> {
    let mut packed = Vec::with_capacity(layers.len());
    for (layer, faces) in layers.iter().enumerate() {
        let mut cube: [Vec<Vec<u8>>; 6] = Default::default();
        for ((name, levels), packed) in FACE_NAMES.iter().zip(faces).zip(&mut cube) {
            *packed = pack(levels).map_err(|error| name_layer_face(layer, name, &error))?;
        }
        packed.push(cube);
    }
    Ok(packed)
}

/// Prefixes a per-layer diagnostic with the layer it came from, keeping its kind.
fn name_layer(layer: usize, error: &GraphicsError) -> GraphicsError {
    GraphicsError::with_kind(
        error.kind(),
        format!("cube array layer {layer}: {}", error.message()),
    )
}

/// Prefixes a per-face packing diagnostic with the layer and face it came from, keeping its
/// kind.
fn name_layer_face(layer: usize, name: &str, error: &GraphicsError) -> GraphicsError {
    name_layer(layer, &name_face(name, error))
}

#[cfg(test)]
mod tests {
    use super::{FACE_NAMES, pack_layers, require_layers, validate_faces, validate_layers};
    use crate::GraphicsErrorKind;
    use crate::graphics::sampled_texture::{pack_float_levels, pack_half_levels};
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

    #[test]
    fn a_cube_array_needs_a_layer_and_names_the_layer_and_face() {
        // 2x2 RGBA16Float, complete chain: 32 then 8 bytes per face.
        let base = [0_u8; 32];
        let chain: [&[u8]; 2] = [&base, &base[..8]];
        let cube = faces(&chain);
        let float = SampledTextureFormat::Float16;
        assert!(validate_layers(float, 2, &[cube], true).is_ok());
        assert!(validate_layers(float, 2, &[cube; 3], true).is_ok());
        let error = validate_layers(float, 2, &[], true).expect_err("no layers");
        assert_eq!(error.kind(), GraphicsErrorKind::InvalidRequest);
        assert!(error.message().contains("at least one layer"));
        assert!(require_layers(0, 1).is_err());
        assert!(require_layers(1, 0).is_err());
        assert!(require_layers(1, 1).is_ok());
        // Layer 2's -Y face is a level short; the message names both.
        let short: [&[u8]; 1] = [&base];
        let mut broken = faces(&chain);
        broken[3] = &short;
        let error = validate_layers(float, 2, &[cube, cube, broken], true)
            .expect_err("a short face in layer 2 is rejected");
        assert_eq!(error.kind(), GraphicsErrorKind::InvalidRequest);
        assert!(
            error
                .message()
                .starts_with("cube array layer 2: cube face -Y supplies 1 mip levels")
        );
        // A face sized for another extent is rejected in whichever layer it sits.
        let wrong: [&[u8]; 2] = [&base[..16], &base[..8]];
        let mut mismatched = faces(&chain);
        mismatched[0] = &wrong;
        let error = validate_layers(float, 2, &[mismatched, cube], true)
            .expect_err("a mis-sized base level is rejected");
        assert!(
            error
                .message()
                .starts_with("cube array layer 0: cube face +X mip level 0 supplies 16 bytes")
        );
    }

    #[test]
    fn cube_array_packing_names_the_layer_and_face_of_invalid_texels() {
        // 2x2 faces with a complete chain: four texels, then one.
        let base = [[0.5_f32, 1.0, 2.0, 1.0]; 4];
        let tail = [[0.25_f32; 4]; 1];
        let chain: [&[[f32; 4]]; 2] = [&base, &tail];
        let pack = |levels: &[&[[f32; 4]]]| pack_float_levels(2, 2, levels, true);
        let packed = pack_layers(&[[&chain[..]; 6]; 2], pack).expect("two valid cubes pack");
        assert_eq!(packed.len(), 2);
        // Eight bytes per binary16 texel: 32 at 2x2, 8 at 1x1, on every face of every cube.
        assert!(
            packed
                .iter()
                .flatten()
                .all(|levels| levels[0].len() == 32 && levels[1].len() == 8)
        );
        let mut bad = [[0.0_f32; 4]; 1];
        bad[0][2] = f32::NAN;
        let broken_chain: [&[[f32; 4]]; 2] = [&base, &bad];
        let mut broken = [&chain[..]; 6];
        broken[4] = &broken_chain;
        let error = pack_layers(&[[&chain[..]; 6], broken], pack).expect_err("NaN is refused");
        assert_eq!(error.kind(), GraphicsErrorKind::InvalidRequest);
        assert!(
            error
                .message()
                .starts_with("cube array layer 1: cube face +Z: float texture mip level 1"),
            "{}",
            error.message()
        );
        // A face that skips its tail level is refused by the chain rule, named the same way.
        let short: [&[[f32; 4]]; 1] = [&base];
        let mut truncated = [&chain[..]; 6];
        truncated[1] = &short;
        let error = pack_layers(&[truncated], pack).expect_err("a short chain is refused");
        assert!(
            error
                .message()
                .starts_with("cube array layer 0: cube face -X: ")
        );
        // Half bits with an all-ones exponent are refused as non-finite.
        let infinite = [[0x7c00_u16, 0, 0, 0x3c00]; 1];
        let half_base = [[0x3c00_u16; 4]; 4];
        let half_chain: [&[[u16; 4]]; 2] = [&half_base, &infinite];
        let error = pack_layers(&[[&half_chain[..]; 6]], |levels: &[&[[u16; 4]]]| {
            pack_half_levels(2, 2, levels, true)
        })
        .expect_err("infinite half bits are refused");
        assert!(
            error
                .message()
                .starts_with("cube array layer 0: cube face +X: ")
        );
    }
}
