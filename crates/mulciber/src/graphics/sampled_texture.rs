//! Checked linear floating-point upload packing, independent of the native backend.
use super::{GraphicsError, GraphicsErrorKind, Vec, format, full_mip_chain_len, mip_extent};

fn byte_size(width: u32, height: u32, bytes_per_texel: usize) -> Result<usize, GraphicsError> {
    if width == 0 || height == 0 {
        return Err(GraphicsError::invalid_request(
            "texture dimensions must be nonzero",
        ));
    }
    usize::try_from(width)
        .ok()
        .and_then(|w| w.checked_mul(bytes_per_texel))
        .and_then(|row| row.checked_mul(usize::try_from(height).ok()?))
        .filter(|size| *size <= isize::MAX.cast_unsigned())
        .ok_or_else(|| GraphicsError::invalid_request("texture dimensions overflow address space"))
}

pub(crate) fn checked_staging_size(
    sizes: impl IntoIterator<Item = usize>,
) -> Result<usize, GraphicsError> {
    sizes.into_iter().try_fold(0_usize, |total, size| {
        total
            .checked_add(size)
            .filter(|total| *total <= isize::MAX.cast_unsigned())
            .ok_or_else(|| GraphicsError::invalid_request("texture staging size overflow"))
    })
}

pub(super) fn pack_float_levels(
    width: u32,
    height: u32,
    levels: &[&[[f32; 4]]],
    complete: bool,
) -> Result<Vec<Vec<u8>>, GraphicsError> {
    // Validate both the input address range and native byte sizes before reading/allocating.
    byte_size(width, height, 16)?;
    let expected_levels = if complete {
        full_mip_chain_len(width, height)
    } else {
        1
    };
    if levels.len() != expected_levels {
        return Err(GraphicsError::invalid_request(format!(
            "float texture mip chain supplies {} levels but needs {expected_levels}",
            levels.len()
        )));
    }
    let mut total = 0_usize;
    for (level, texels) in (0_u32..).zip(levels) {
        let size = byte_size(mip_extent(width, level), mip_extent(height, level), 8)?;
        if texels.len() != size / 8 {
            return Err(GraphicsError::invalid_request(format!(
                "float texture mip level {level} texel count does not match its dimensions"
            )));
        }
        total = checked_staging_size([total, size])?;
        if texels
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 65504.0)
        {
            return Err(GraphicsError::invalid_request(format!(
                "float texture mip level {level} requires finite components in -65504..=65504"
            )));
        }
    }
    let mut packed = Vec::with_capacity(levels.len());
    for texels in levels {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(texels.len() * 8).map_err(|_| {
            GraphicsError::with_kind(
                GraphicsErrorKind::OutOfMemory,
                "float texture conversion allocation failed",
            )
        })?;
        for value in texels.iter().flatten() {
            bytes.extend_from_slice(&finite_f32_to_f16(*value).to_ne_bytes());
        }
        packed.push(bytes);
    }
    Ok(packed)
}

/// Precondition: finite input in -65504..=65504. Integer rounding avoids host FP-mode dependence.
fn finite_f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = u16::try_from((bits >> 16) & 0x8000).expect("sign fits");
    let magnitude = bits & 0x7fff_ffff;
    // Below the midpoint of zero and the least binary16 subnormal, including f32 subnormals.
    if magnitude <= 0x3300_0000 {
        return sign;
    }
    let exponent = (magnitude >> 23) & 0xff;
    let (base, remainder, halfway) = if exponent < 113 {
        let shift = 126 - exponent;
        let significand = (magnitude & 0x7f_ffff) | 0x80_0000;
        (
            significand >> shift,
            significand & ((1 << shift) - 1),
            1 << (shift - 1),
        )
    } else {
        ((magnitude - 0x3800_0000) >> 13, magnitude & 0x1fff, 0x1000)
    };
    let rounded = base + u32::from(remainder > halfway || (remainder == halfway && base & 1 != 0));
    sign | u16::try_from(rounded).expect("validated finite half range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_counts_and_sizes_are_checked() {
        for (w, h) in [(0, 1), (1, 0), (u32::MAX, u32::MAX)] {
            assert!(pack_float_levels(w, h, &[&[]], false).is_err());
        }
        assert!(byte_size(u32::MAX, u32::MAX, 8).is_err());
        assert!(byte_size(u32::MAX, u32::MAX, 16).is_err());
        assert!(pack_float_levels(2, 1, &[&[[0.0; 4]]], false).is_err());
        assert!(pack_float_levels(1, 1, &[&[[0.0; 4]; 2]], false).is_err());
    }

    #[test]
    fn staging_sum_checks_address_and_capacity_overflow() {
        assert_eq!(checked_staging_size([32, 8]).unwrap(), 40);
        assert!(checked_staging_size([isize::MAX.cast_unsigned(), 1]).is_err());
        assert!(checked_staging_size([usize::MAX, 8]).is_err());
    }

    #[test]
    fn mip_chains_are_complete_and_per_level_checked() {
        let a = [[0.0; 4]; 15];
        let b = [[1.0; 4]; 2];
        let c = [[2.0; 4]; 1];
        assert!(pack_float_levels(5, 3, &[&a, &b, &c], true).is_ok());
        assert!(pack_float_levels(5, 3, &[&a], false).is_ok());
        for levels in [
            &[&a[..], &b[..]][..],
            &[&a[..], &c[..], &c[..]],
            &[&a[..], &b[..], &c[..], &c[..]],
        ] {
            assert!(pack_float_levels(5, 3, levels, true).is_err());
        }
        assert!(pack_float_levels(1, 1, &[&c], true).is_ok());
        assert!(pack_float_levels(1, 1, &[], true).is_err());
    }

    #[test]
    fn invalid_components_are_rejected_including_later_mips() {
        for value in [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            65505.0,
            -65505.0,
            f32::MAX,
        ] {
            assert!(pack_float_levels(1, 1, &[&[[value; 4]]], false).is_err());
            assert!(pack_float_levels(2, 1, &[&[[0.0; 4]; 2], &[[value; 4]]], true).is_err());
        }
    }

    #[test]
    fn quantization_policy_and_packing() {
        for (value, expected) in [
            (0.0, 0),
            (-0.0, 0x8000),
            (-2.0, 0xc000),
            (4.0, 0x4400),
            (65504.0, 0x7bff),
            (-65504.0, 0xfbff),
            (1e-5, 0x00a8),
            (1e-4, 0x068e),
            (1e-3, 0x1419),
            (f32::MIN_POSITIVE, 0),
            (-f32::from_bits(1), 0x8000),
            (2.0_f32.powi(-24), 1),
            (2.0_f32.powi(-25), 0),
            (3.0 * 2.0_f32.powi(-25), 2),
            (1.0 + 2.0_f32.powi(-11), 0x3c00),
            (1.0 + 3.0 * 2.0_f32.powi(-11), 0x3c02),
        ] {
            assert_eq!(finite_f32_to_f16(value), expected, "{value}");
        }
        let packed = pack_float_levels(1, 1, &[&[[0.0, -2.0, 4.0, 1e-5]]], false).unwrap();
        let expected: Vec<u8> = [0_u16, 0xc000, 0x4400, 0x00a8]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        assert_eq!(packed[0], expected);
    }

    #[test]
    fn every_finite_half_round_trips_and_midpoints_round_even() {
        fn decode(bits: u16) -> f32 {
            let e = (bits >> 10) & 31;
            let m = bits & 1023;
            let v = if e == 0 {
                f32::from(m) * 2.0_f32.powi(-24)
            } else {
                (1.0 + f32::from(m) / 1024.0) * 2.0_f32.powi(i32::from(e) - 15)
            };
            if bits & 0x8000 == 0 { v } else { -v }
        }
        for bits in 0_u16..=u16::MAX {
            if bits & 0x7c00 == 0x7c00 {
                continue;
            }
            assert_eq!(finite_f32_to_f16(decode(bits)), bits);
        }
        for bits in 0_u16..0x7bff {
            let midpoint = (decode(bits) + decode(bits + 1)) * 0.5;
            assert_eq!(finite_f32_to_f16(midpoint), bits + (bits & 1));
            assert_eq!(finite_f32_to_f16(-midpoint), 0x8000 | (bits + (bits & 1)));
        }
    }
}
