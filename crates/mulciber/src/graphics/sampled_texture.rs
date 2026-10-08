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
    }
    let mut packed = Vec::with_capacity(levels.len());
    for (level, texels) in (0_u32..).zip(levels) {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(texels.len() * 8).map_err(|_| {
            GraphicsError::with_kind(
                GraphicsErrorKind::OutOfMemory,
                "float texture conversion allocation failed",
            )
        })?;
        bytes.resize(texels.len() * 8, 0);
        // One pass converts and checks the range together.
        if !f32_to_f16(texels.as_flattened(), &mut bytes) {
            return Err(GraphicsError::invalid_request(format!(
                "float texture mip level {level} requires finite components in -65504..=65504"
            )));
        }
        packed.push(bytes);
    }
    Ok(packed)
}

/// The largest finite binary16 magnitude, 65504, as `f32` bits. A larger magnitude, infinity or
/// NaN has larger bits once the sign is cleared.
const HALF_MAX_BITS: u32 = 0x477f_e000;

/// Converts `values` to binary16 in `out` (native-endian, two bytes each), rounding to nearest,
/// ties to even, and returns whether every value was finite and in -65504..=65504. `out` holds
/// garbage for a false return. Both paths give bit-identical output: an exhaustive comparison
/// over every `f32` bit pattern is the ignored test `fast_paths_match_reference_exhaustively`.
fn f32_to_f16(values: &[f32], out: &mut [u8]) -> bool {
    debug_assert_eq!(out.len(), values.len() * 2);
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("f16c") {
        // SAFETY: the CPU supports F16C, and therefore AVX, as just detected.
        return unsafe { f32_to_f16_f16c(values, out) };
    }
    f32_to_f16_portable(values, out)
}

/// Branch-free, so it vectorizes; no early exit, for the same reason.
fn f32_to_f16_portable(values: &[f32], out: &mut [u8]) -> bool {
    let mut valid = true;
    for (half, value) in out.as_chunks_mut::<2>().0.iter_mut().zip(values) {
        valid &= value.to_bits() & 0x7fff_ffff <= HALF_MAX_BITS;
        *half = finite_f32_to_f16(*value).to_ne_bytes();
    }
    valid
}

/// F16C converts eight values per instruction with the rounding mode given in the instruction,
/// independent of MXCSR.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,f16c")]
fn f32_to_f16_f16c(values: &[f32], out: &mut [u8]) -> bool {
    use std::arch::x86_64::{
        _CMP_LE_OQ, _MM_FROUND_TO_NEAREST_INT, _mm_storeu_si128, _mm256_and_ps,
        _mm256_castsi256_ps, _mm256_cmp_ps, _mm256_cvtps_ph, _mm256_loadu_ps, _mm256_movemask_ps,
        _mm256_set1_epi32, _mm256_set1_ps,
    };
    let (groups, rest) = values.as_chunks::<8>();
    let (halves, rest_out) = out.as_chunks_mut::<16>();
    let magnitude = _mm256_castsi256_ps(_mm256_set1_epi32(0x7fff_ffff));
    let limit = _mm256_set1_ps(65504.0);
    let mut in_range = _mm256_castsi256_ps(_mm256_set1_epi32(-1));
    for (group, half) in groups.iter().zip(halves) {
        // SAFETY: `group` is eight readable f32s and `half` sixteen writable bytes; both
        // intrinsics take unaligned addresses.
        unsafe {
            let v = _mm256_loadu_ps(group.as_ptr());
            // Ordered compare: NaN fails it.
            in_range = _mm256_and_ps(
                in_range,
                _mm256_cmp_ps::<_CMP_LE_OQ>(_mm256_and_ps(v, magnitude), limit),
            );
            _mm_storeu_si128(
                half.as_mut_ptr().cast(),
                _mm256_cvtps_ph::<_MM_FROUND_TO_NEAREST_INT>(v),
            );
        }
    }
    let valid = _mm256_movemask_ps(in_range) == 0xff;
    f32_to_f16_portable(rest, rest_out) && valid
}

/// One value, for finite input in -65504..=65504 (other input gives an unspecified result).
/// Round to nearest even through integer arithmetic, plus one `f32` add for subnormal halves
/// (Fabian Giesen's `float_to_half_fast3_rtne`), so it doesn't branch.
fn finite_f32_to_f16(value: f32) -> u16 {
    // 2^-1 above the half subnormal step: adding it puts the 10 subnormal bits at the bottom of
    // the significand, the add itself rounding to nearest even.
    const SUBNORMAL_MAGIC: u32 = ((127 - 15) + (23 - 10) + 1) << 23;
    let bits = value.to_bits();
    let sign = (bits >> 16) & 0x8000;
    let magnitude = bits & 0x7fff_ffff;
    let subnormal = (f32::from_bits(magnitude) + f32::from_bits(SUBNORMAL_MAGIC))
        .to_bits()
        .wrapping_sub(SUBNORMAL_MAGIC);
    // Rebias the exponent and round: add just under half an ulp, plus one if the kept bits are
    // odd, then drop the low 13 bits.
    let odd = (magnitude >> 13) & 1;
    let normal = magnitude
        .wrapping_sub((127 - 15) << 23)
        .wrapping_add(0xfff + odd)
        >> 13;
    let half = if magnitude < (113 << 23) {
        subnormal
    } else {
        normal
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a finite half fits 16 bits"
    )]
    let half = (half | sign) as u16;
    half
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
    fn packed_chain_levels_are_tight_whole_texels() {
        // Chain replacement stages levels back to back; Vulkan copy offsets rely on each level
        // being exactly its texels, eight bytes apiece.
        let (a, b, c) = ([[0.5; 4]; 15], [[1.0; 4]; 2], [[2.0; 4]; 1]);
        let packed = pack_float_levels(5, 3, &[&a, &b, &c], true).unwrap();
        let sizes: Vec<usize> = packed.iter().map(Vec::len).collect();
        assert_eq!(sizes, [15 * 8, 2 * 8, 8]);
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

    /// The converter Mulciber used before the branch-free one: integer rounding, one value at a
    /// time. The fast paths must match it bit for bit.
    fn reference(value: f32) -> u16 {
        let bits = value.to_bits();
        let sign = u16::try_from((bits >> 16) & 0x8000).expect("sign fits");
        let magnitude = bits & 0x7fff_ffff;
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
        let rounded =
            base + u32::from(remainder > halfway || (remainder == halfway && base & 1 != 0));
        sign | u16::try_from(rounded).expect("validated finite half range")
    }

    fn in_range(value: f32) -> bool {
        value.is_finite() && value.abs() <= 65504.0
    }

    /// Both paths over `values`: the range verdict, and for a whole valid slice every half.
    fn check_paths(values: &[f32]) {
        let expected_valid = values.iter().all(|v| in_range(*v));
        let mut portable = std::vec![0_u8; values.len() * 2];
        assert_eq!(f32_to_f16_portable(values, &mut portable), expected_valid);
        let mut dispatched = std::vec![0_u8; values.len() * 2];
        assert_eq!(f32_to_f16(values, &mut dispatched), expected_valid);
        if expected_valid {
            let expected: Vec<u8> = values
                .iter()
                .flat_map(|v| reference(*v).to_ne_bytes())
                .collect();
            assert_eq!(portable, expected);
            assert_eq!(dispatched, expected);
        }
    }

    /// Every 4099th bit pattern, in runs of 8 so the F16C path sees whole groups, plus odd-length
    /// tails and the boundaries.
    #[test]
    fn fast_paths_match_reference() {
        let mut run = Vec::with_capacity(8);
        for start in (0..=u32::MAX - 8).step_by(4099 * 8) {
            run.clear();
            run.extend((start..start + 8).map(f32::from_bits));
            check_paths(&run);
            check_paths(&run[..5]);
            for &v in &run {
                check_paths(&[v]);
            }
        }
        for v in [
            65504.0,
            65504.001,
            65519.99,
            65520.0,
            f32::INFINITY,
            f32::NAN,
            -0.0,
            f32::from_bits(1),
        ] {
            check_paths(&[v]);
            check_paths(&[v, -v, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
        }
    }

    /// Every `f32` bit pattern through both paths (about half a minute in release:
    /// `cargo test --release -p mulciber -- --ignored fast_paths`).
    #[test]
    #[ignore = "exhaustive; run by hand in release"]
    fn fast_paths_match_reference_exhaustively() {
        let mut run = Vec::with_capacity(1024);
        let mut start = 0_u64;
        while u32::try_from(start).is_ok() {
            let end = (start + 1024).min(1 << 32);
            run.clear();
            run.extend((start..end).map(|b| f32::from_bits(u32::try_from(b).unwrap())));
            for group in run.chunks(8) {
                check_paths(group);
            }
            start = end;
        }
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
            let midpoint = f32::midpoint(decode(bits), decode(bits + 1));
            assert_eq!(finite_f32_to_f16(midpoint), bits + (bits & 1));
            assert_eq!(finite_f32_to_f16(-midpoint), 0x8000 | (bits + (bits & 1)));
        }
    }
}
