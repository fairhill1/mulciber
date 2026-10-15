//! High dynamic range light: mip chains filtered in linear light, and BC6H's unsigned half floats.

use crate::TextureError;

/// The largest value BC6H's unsigned half floats hold.
pub const HDR_MAX: f32 = 65504.0;

/// The complete mip chain of a linear RGBA float image, `base` first and 1×1 last, each level the
/// mean of a 2×2 footprint of the one above. Odd extents clamp the footprint to the level's last
/// row and column, as the RGBA8 chains do. Light is averaged as it is, without a transfer
/// function: a level is how much light its footprint gave.
///
/// BC6H cannot be filtered on the GPU without decoding it, so a BC6H texture with mips is built
/// from a chain like this, every level encoded with [`encode_bc6h`](crate::encode_bc6h).
///
/// # Panics
///
/// If the base is empty or holds other than `width × height` texels.
#[must_use]
pub fn hdr_chain(width: u32, height: u32, base: Vec<[f32; 4]>) -> Vec<Vec<[f32; 4]>> {
    assert!(width > 0 && height > 0, "a texture needs texels");
    assert_eq!(
        base.len(),
        width as usize * height as usize,
        "the base level is not {width}x{height} texels"
    );
    let mut levels = vec![base];
    let (mut w, mut h) = (width as usize, height as usize);
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let previous = levels.last().expect("the chain starts with its base");
        let texel = |x: usize, y: usize| previous[y.min(h - 1) * w + x.min(w - 1)];
        let mut next = Vec::with_capacity(nw * nh);
        for y in 0..nh {
            for x in 0..nw {
                let footprint = [
                    texel(x * 2, y * 2),
                    texel(x * 2 + 1, y * 2),
                    texel(x * 2, y * 2 + 1),
                    texel(x * 2 + 1, y * 2 + 1),
                ];
                next.push(std::array::from_fn(|channel| {
                    footprint.iter().map(|texel| texel[channel]).sum::<f32>() * 0.25
                }));
            }
        }
        levels.push(next);
        (w, h) = (nw, nh);
    }
    levels
}

/// `texels`' RGB as unsigned half-float bits, alpha one, for BC6H's encoder: below zero clamps to
/// zero and above [`HDR_MAX`] to it, which BC6H's unsigned floats cannot hold.
#[cfg_attr(not(feature = "encode"), allow(dead_code))]
pub(crate) fn unsigned_halves(texels: &[[f32; 4]]) -> Result<Vec<[u16; 4]>, TextureError> {
    texels
        .iter()
        .enumerate()
        .map(|(index, &[r, g, b, _])| {
            if [r, g, b].iter().all(|c| c.is_finite()) {
                Ok([half(r), half(g), half(b), ONE])
            } else {
                Err(TextureError::new(format!(
                    "texel {index} is not finite: {r}, {g}, {b}"
                )))
            }
        })
        .collect()
}

/// 1.0 as a half float.
const ONE: u16 = 0x3c00;

/// A finite value as unsigned half-float bits, clamped to 0..=[`HDR_MAX`] and rounded to the
/// nearest half, ties to even, subnormals kept.
fn half(value: f32) -> u16 {
    let bits = value.clamp(0.0, HDR_MAX).to_bits();
    #[allow(clippy::cast_possible_wrap)]
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    let round = |value: u32, shift: u32, rest: u32| {
        let halfway = 1 << (shift - 1);
        if rest > halfway || (rest == halfway && value & 1 == 1) {
            value + 1
        } else {
            value
        }
    };
    let rounded = if exponent <= 0 {
        // A subnormal half: the implicit bit joins the mantissa, shifted down past the exponent.
        if exponent < -10 {
            return 0;
        }
        let full = mantissa | 0x80_0000;
        #[allow(clippy::cast_sign_loss)]
        let shift = (14 - exponent) as u32;
        round(full >> shift, shift, full & ((1 << shift) - 1))
    } else {
        // A carry out of the mantissa rounds into the exponent, which is what it should do.
        #[allow(clippy::cast_sign_loss)]
        let biased = (exponent as u32) << 10;
        round(biased | (mantissa >> 13), 13, mantissa & 0x1fff)
    };
    u16::try_from(rounded).expect("a clamped value rounds within half range")
}

#[cfg(test)]
mod tests {
    use super::{HDR_MAX, half, hdr_chain, unsigned_halves};

    /// The value of unsigned half-float bits.
    fn value(bits: u16) -> f32 {
        let exponent = i32::from(bits >> 10);
        let mantissa = f32::from(bits & 0x3ff);
        if exponent == 0 {
            mantissa * (2.0_f32).powi(-24)
        } else {
            (1.0 + mantissa / 1024.0) * (2.0_f32).powi(exponent - 15)
        }
    }

    #[test]
    fn halves_round_to_the_nearest_and_clamp_to_the_unsigned_range() {
        assert_eq!(half(1.0), 0x3c00);
        assert_eq!(half(0.0), 0);
        assert_eq!(half(-3.0), 0);
        assert_eq!(half(HDR_MAX), 0x7bff);
        assert_eq!(half(1.0e9), 0x7bff);
        // The smallest subnormal, and half of it ties to even (zero).
        assert_eq!(half(2.0_f32.powi(-24)), 1);
        assert_eq!(half(2.0_f32.powi(-25)), 0);
        // Every half's own value converts back to it, normals and subnormals alike.
        for bits in (0..=0x7bff_u16).step_by(7) {
            assert_eq!(half(value(bits)), bits, "{bits:#06x}");
        }
        // A value between two halves goes to the nearer.
        let (low, high) = (value(0x4000), value(0x4001));
        assert_eq!(half(low + (high - low) * 0.4), 0x4000);
        assert_eq!(half(low + (high - low) * 0.6), 0x4001);
    }

    #[test]
    fn non_finite_light_is_refused_by_texel() {
        let error = unsigned_halves(&[[1.0; 4], [f32::NAN, 0.0, 0.0, 1.0]]).unwrap_err();
        assert!(error.to_string().contains("texel 1"), "{error}");
        let [texel] = unsigned_halves(&[[2.0, 0.5, -1.0, 0.0]]).unwrap()[..] else {
            panic!("one texel");
        };
        assert_eq!(texel, [half(2.0), half(0.5), 0, 0x3c00]);
    }

    #[test]
    fn the_chain_averages_light_down_to_one_texel() {
        // 3×2: odd width clamps the footprint to the last column.
        let base = vec![
            [1.0, 0.0, 0.0, 1.0],
            [3.0, 0.0, 0.0, 1.0],
            [5.0, 0.0, 0.0, 1.0],
            [1.0, 4.0, 0.0, 1.0],
            [3.0, 4.0, 0.0, 1.0],
            [5.0, 4.0, 100.0, 1.0],
        ];
        let levels = hdr_chain(3, 2, base);
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[1], vec![[2.0, 2.0, 0.0, 1.0]]);
        // A bright texel's light survives into the mean rather than clipping.
        let mut base = vec![[0.0; 4]; 16];
        base[5] = [1600.0, 0.0, 0.0, 1.0];
        let levels = hdr_chain(4, 4, base);
        assert_eq!(levels.len(), 3);
        assert!((levels[2][0][0] - 100.0).abs() < 1.0e-3);
    }
}
