//! How an RGBA8 base level becomes a mip chain. Each [`Chain`] is named for what the texture
//! holds, because that is what decides the right filter.

use mulciber::BlockCompression;

/// Alpha at or above which a [`Chain::Cutout`] texel counts as covered.
pub const CUTOUT_THRESHOLD: f32 = 0.5;

/// An RGBA8 mip chain: the base extent and every level from the base down to 1×1, each halving
/// both axes and flooring at one texel, the rule Mulciber's mip uploads check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Levels {
    /// The base level's width in texels.
    pub width: u32,
    /// The base level's height in texels.
    pub height: u32,
    /// Tightly packed RGBA8 rows, base level first.
    pub levels: Vec<Vec<u8>>,
}

/// The number of levels from a `width`×`height` base down to 1×1.
#[must_use]
pub fn chain_len(width: u32, height: u32) -> usize {
    (32 - width.max(height).leading_zeros()) as usize
}

/// A level's extent along one axis.
#[must_use]
pub const fn mip_extent(base: u32, level: u32) -> u32 {
    let scaled = base >> level;
    if scaled == 0 { 1 } else { scaled }
}

/// The filter a texture's mips are built with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Chain {
    /// sRGB colour with straight alpha: RGB averaged in linear light, so mips do not darken,
    /// and alpha box filtered. Uploaded and sampled as sRGB.
    Color,
    /// [`Chain::Color`] for a texture tiled across distant ground: the coarse mips are pulled
    /// toward the texture's average so the tile period does not read as a grid. Full detail
    /// down to the fifth level from the end, half way at the fourth, a quarter at the third and
    /// flat for the last three (Isle of Rán's terrain).
    ColorFlatDistant,
    /// [`Chain::Color`] for an alpha-tested card: each level's alpha is rescaled so the fraction
    /// of texels at or above [`CUTOUT_THRESHOLD`] stays the base level's, which keeps foliage
    /// from thinning to nothing with distance.
    Cutout,
    /// A tangent-space normal in RGB with a scalar in alpha (Isle of Rán's packing puts
    /// perceptual roughness there): each 2×2 footprint is decoded, averaged as unit vectors and
    /// renormalised, and alpha is box filtered. Uploaded and sampled as stored.
    Normal,
    /// A tangent-space normal in RGB with perceptual roughness in alpha (Isle of Rán's packing):
    /// normals averaged as [`Chain::Normal`]'s, and roughness widened by how much they disagree,
    /// so bumps too fine for a level to show turn into the rougher surface they look like from
    /// afar instead of a smooth one that shines. Each level holds its footprint's mean squared
    /// GGX roughness (α², α the perceptual value squared) plus the spread of its base normals,
    /// read off the shortening of their mean (Toksvig's observation) through a von Mises–Fisher
    /// fit. Uploaded and sampled as stored.
    NormalRoughness,
    /// Four independent linear channels, each box filtered: masks and scalar maps such as
    /// metallic and ambient occlusion. Uploaded and sampled as stored.
    Linear,
    /// A tangent-space normal alone, baked as BC5: each level the mean of the base normals under
    /// it, renormalised, the base normalised too, because BC5 keeps only X and Y and the shader
    /// rebuilds Z as `sqrt(1 - x² - y²)`. Built from sources it uploads as RGBA8 with Z in blue;
    /// a shader reads red and green alone, so a bake and its fallback read the same.
    TangentNormal,
    /// A physically based surface's scalars: R = metallic, G = ambient occlusion, B = perceptual
    /// roughness, A kept. Red, green and alpha are box filtered. Blue is
    /// [`Chain::NormalRoughness`]'s roughness, its mean α² widened by how much the base normals
    /// under each texel disagree: bumps too fine for a level to show turn into the rougher
    /// surface they look like from afar. The normals are the recipe's
    /// [`normals`](crate::Recipe::normals), given to [`Chain::build_widened`]; [`Chain::build`]
    /// takes them as flat, widening nothing. Uploaded and sampled as stored.
    Surface,
}

impl Chain {
    /// Whether the texture holds sRGB-encoded colour.
    #[must_use]
    pub const fn is_srgb(self) -> bool {
        matches!(self, Self::Color | Self::ColorFlatDistant | Self::Cutout)
    }

    /// The block compression a bake of this chain is encoded as: BC5 for
    /// [`Chain::TangentNormal`], BC7 otherwise, sRGB or not.
    #[must_use]
    pub const fn compression(self) -> BlockCompression {
        if matches!(self, Self::TangentNormal) {
            BlockCompression::Bc5Unorm
        } else if self.is_srgb() {
            BlockCompression::Bc7Srgb
        } else {
            BlockCompression::Bc7Unorm
        }
    }

    /// A stable number per chain, part of a bake's source digest.
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Color => 0,
            Self::ColorFlatDistant => 1,
            Self::Cutout => 2,
            Self::Normal => 3,
            Self::Linear => 4,
            Self::NormalRoughness => 5,
            Self::TangentNormal => 6,
            Self::Surface => 7,
        }
    }

    /// Builds the complete chain from a tightly packed RGBA8 base level.
    ///
    /// # Panics
    ///
    /// If the base is empty or its byte count is not `width × height × 4`.
    #[must_use]
    pub fn build(self, width: u32, height: u32, base: Vec<u8>) -> Levels {
        self.build_widened(width, height, base, None)
    }

    /// [`build`](Self::build), with the tightly packed RGBA8 tangent-space normals of the same
    /// extent that widen a [`Chain::Surface`]'s roughness. Other chains ignore them.
    ///
    /// # Panics
    ///
    /// If the base is empty, or it or the normals are not `width × height × 4` bytes.
    #[must_use]
    pub fn build_widened(
        self,
        width: u32,
        height: u32,
        base: Vec<u8>,
        normals: Option<&[u8]>,
    ) -> Levels {
        assert!(width > 0 && height > 0, "a texture needs texels");
        assert_eq!(
            base.len(),
            width as usize * height as usize * 4,
            "the base level is not {width}x{height} RGBA8"
        );
        if let Some(normals) = normals {
            assert_eq!(
                normals.len(),
                base.len(),
                "the normals are not {width}x{height} RGBA8"
            );
        }
        let levels = match self {
            Self::Color => chain(width, height, base, color_texel),
            Self::ColorFlatDistant => flatten_distant_mips(chain(width, height, base, color_texel)),
            Self::Cutout => {
                let target = covered_fraction(&base, 1.0);
                let mut levels = chain(width, height, base, color_texel);
                for level in levels.iter_mut().skip(1) {
                    let scale = coverage_scale(level, target);
                    for texel in level.as_chunks_mut::<4>().0 {
                        texel[3] = round_byte(f32::from(texel[3]) * scale);
                    }
                }
                levels
            }
            Self::Normal => chain(width, height, base, normal_texel),
            Self::NormalRoughness => normal_roughness_chain(width, height, base),
            Self::Linear => chain(width, height, base, linear_texel),
            Self::TangentNormal => tangent_normal_chain(width, height, &base),
            Self::Surface => surface_chain(width, height, base, normals),
        };
        Levels {
            width,
            height,
            levels,
        }
    }
}

/// Builds levels with `filter` combining each 2×2 footprint. Odd extents clamp the footprint to
/// the level's last row and column.
fn chain(
    width: u32,
    height: u32,
    base: Vec<u8>,
    filter: fn(&[[u8; 4]; 4]) -> [u8; 4],
) -> Vec<Vec<u8>> {
    let mut levels = vec![base];
    let (mut w, mut h) = (width as usize, height as usize);
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let previous = levels.last().expect("the chain starts with its base");
        let texel = |x: usize, y: usize| -> [u8; 4] {
            let at = (y.min(h - 1) * w + x.min(w - 1)) * 4;
            [
                previous[at],
                previous[at + 1],
                previous[at + 2],
                previous[at + 3],
            ]
        };
        let mut next = Vec::with_capacity(nw * nh * 4);
        for y in 0..nh {
            for x in 0..nw {
                let footprint = [
                    texel(x * 2, y * 2),
                    texel(x * 2 + 1, y * 2),
                    texel(x * 2, y * 2 + 1),
                    texel(x * 2 + 1, y * 2 + 1),
                ];
                next.extend_from_slice(&filter(&footprint));
            }
        }
        levels.push(next);
        (w, h) = (nw, nh);
    }
    levels
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn round_byte(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// The sRGB transfer function's inverse (IEC 61966-2-1), for one byte.
#[must_use]
pub fn srgb_to_linear(byte: u8) -> f32 {
    let c = f32::from(byte) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The sRGB transfer function (IEC 61966-2-1), to the nearest byte.
#[must_use]
pub fn linear_to_srgb(linear: f32) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let encoded = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    round_byte(encoded * 255.0)
}

fn box_byte(footprint: &[[u8; 4]; 4], channel: usize) -> u8 {
    let sum: u32 = footprint.iter().map(|t| u32::from(t[channel])).sum();
    u8::try_from((sum + 2) / 4).expect("an average of bytes is a byte")
}

fn color_texel(footprint: &[[u8; 4]; 4]) -> [u8; 4] {
    let mut out = [0; 4];
    for (channel, value) in out.iter_mut().take(3).enumerate() {
        let sum: f32 = footprint.iter().map(|t| srgb_to_linear(t[channel])).sum();
        *value = linear_to_srgb(sum / 4.0);
    }
    out[3] = box_byte(footprint, 3);
    out
}

fn linear_texel(footprint: &[[u8; 4]; 4]) -> [u8; 4] {
    [0, 1, 2, 3].map(|channel| box_byte(footprint, channel))
}

/// Decodes an RGB byte triple as a vector in [-1, 1]³.
fn decode_normal(texel: [u8; 4]) -> [f32; 3] {
    [0, 1, 2].map(|c| f32::from(texel[c]) * (2.0 / 255.0) - 1.0)
}

fn normalize_or_z(v: [f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length > 1e-6 {
        v.map(|c| c / length)
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn normal_texel(footprint: &[[u8; 4]; 4]) -> [u8; 4] {
    let mut sum = [0.0_f32; 3];
    for texel in footprint {
        let n = normalize_or_z(decode_normal(*texel));
        for c in 0..3 {
            sum[c] += n[c];
        }
    }
    let n = normalize_or_z(sum);
    [
        round_byte((n[0] * 0.5 + 0.5) * 255.0),
        round_byte((n[1] * 0.5 + 0.5) * 255.0),
        round_byte((n[2] * 0.5 + 0.5) * 255.0),
        box_byte(footprint, 3),
    ]
}

/// Reduces per-texel moments level by level in floats, calling `each` with every level below the
/// base, so a texel's mean normal is the mean of every base normal under it and keeps the
/// shortening a renormalised level would lose.
fn reduce_moments(
    width: u32,
    height: u32,
    mut moments: Vec<[f32; 4]>,
    mut each: impl FnMut(&[[f32; 4]]),
) {
    let (mut w, mut h) = (width as usize, height as usize);
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let at = |x: usize, y: usize| moments[y.min(h - 1) * w + x.min(w - 1)];
        let mut next = Vec::with_capacity(nw * nh);
        for y in 0..nh {
            for x in 0..nw {
                let footprint = [
                    at(x * 2, y * 2),
                    at(x * 2 + 1, y * 2),
                    at(x * 2, y * 2 + 1),
                    at(x * 2 + 1, y * 2 + 1),
                ];
                let mut mean = [0.0_f32; 4];
                for moment in footprint {
                    for (sum, value) in mean.iter_mut().zip(moment) {
                        *sum += value * 0.25;
                    }
                }
                next.push(mean);
            }
        }
        each(&next);
        moments = next;
        (w, h) = (nw, nh);
    }
}

/// A texel's moments: its unit normal, then its α² from `perceptual` roughness (0..=255).
fn moment(normal: [u8; 4], perceptual: u8) -> [f32; 4] {
    let [x, y, z] = normalize_or_z(decode_normal(normal));
    [x, y, z, (f32::from(perceptual) / 255.0).powi(4)]
}

/// [`Chain::NormalRoughness`]'s levels.
fn normal_roughness_chain(width: u32, height: u32, base: Vec<u8>) -> Vec<Vec<u8>> {
    let moments = base
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&texel| moment(texel, texel[3]))
        .collect();
    let mut levels = vec![base];
    reduce_moments(width, height, moments, |level| {
        levels.push(
            level
                .iter()
                .flat_map(|&moment| {
                    let [x, y, z] = encode_normal(moment);
                    [x, y, z, widened_roughness(moment)]
                })
                .collect(),
        );
    });
    levels
}

/// [`Chain::TangentNormal`]'s levels. Alpha is opaque throughout.
fn tangent_normal_chain(width: u32, height: u32, base: &[u8]) -> Vec<Vec<u8>> {
    let moments: Vec<[f32; 4]> = base
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&texel| moment(texel, 0))
        .collect();
    let encoded = |level: &[[f32; 4]]| -> Vec<u8> {
        level
            .iter()
            .flat_map(|&moment| {
                let [x, y, z] = encode_normal(moment);
                [x, y, z, 255]
            })
            .collect()
    };
    let mut levels = vec![encoded(&moments)];
    reduce_moments(width, height, moments, |level| levels.push(encoded(level)));
    levels
}

/// [`Chain::Surface`]'s levels: box filtered, then blue replaced below the base by the roughness
/// widened by `normals`, flat when there are none.
fn surface_chain(width: u32, height: u32, base: Vec<u8>, normals: Option<&[u8]>) -> Vec<Vec<u8>> {
    const FLAT: [u8; 4] = [128, 128, 255, 255];
    let texels = base.as_chunks::<4>().0;
    let moments: Vec<[f32; 4]> = match normals {
        Some(normals) => normals
            .as_chunks::<4>()
            .0
            .iter()
            .zip(texels)
            .map(|(&normal, texel)| moment(normal, texel[2]))
            .collect(),
        None => texels.iter().map(|texel| moment(FLAT, texel[2])).collect(),
    };
    let mut levels = chain(width, height, base, linear_texel);
    let mut below = levels.iter_mut().skip(1);
    reduce_moments(width, height, moments, |level| {
        let out = below.next().expect("both chains halve alike");
        for (texel, &moment) in out.as_chunks_mut::<4>().0.iter_mut().zip(level) {
            texel[2] = widened_roughness(moment);
        }
    });
    levels
}

/// A mean normal's direction as RGB bytes.
fn encode_normal([x, y, z, _]: [f32; 4]) -> [u8; 3] {
    normalize_or_z([x, y, z]).map(|c| round_byte((c * 0.5 + 0.5) * 255.0))
}

/// The perceptual roughness of a texel's mean normal and mean α²: α² widened by the normal's
/// spread.
fn widened_roughness([x, y, z, alpha_squared]: [f32; 4]) -> u8 {
    let length = (x * x + y * y + z * z).sqrt();
    let widened = (alpha_squared + normal_spread(length)).min(1.0);
    round_byte(widened.sqrt().sqrt() * 255.0)
}

/// The α² that normals whose unit vectors average to `length` add: a von Mises–Fisher spread
/// of concentration κ ≈ (3L − L³) / (1 − L²) (Banerjee et al.) has a per-axis slope variance of
/// about 1/κ, and GGX's α² is twice its slope variance.
fn normal_spread(length: f32) -> f32 {
    let l = length.clamp(0.0, 1.0);
    let missing = 1.0 - l * l;
    if missing <= 1e-6 {
        return 0.0;
    }
    let kappa = (3.0 * l - l * l * l) / missing;
    if kappa <= 1e-6 {
        return 1.0;
    }
    (2.0 / kappa).min(1.0)
}

/// Pulls a chain's coarse tail toward its 1×1 average (see [`Chain::ColorFlatDistant`]).
fn flatten_distant_mips(mut levels: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let average = levels.last().expect("the chain ends at 1x1").clone();
    let count = levels.len();
    for (index, level) in levels.iter_mut().enumerate() {
        let toward_average = match count - 1 - index {
            0..=2 => 1.0,
            3 => 0.5,
            4 => 0.25,
            _ => continue,
        };
        for (texel, mean) in level.iter_mut().zip(average.iter().cycle()) {
            let (t, m) = (f32::from(*texel), f32::from(*mean));
            *texel = round_byte(t + (m - t) * toward_average);
        }
    }
    levels
}

/// Fraction of texels whose alpha, scaled by `scale`, is at or above [`CUTOUT_THRESHOLD`].
fn covered_fraction(level: &[u8], scale: f32) -> f32 {
    let threshold = CUTOUT_THRESHOLD * 255.0;
    let texels = level.as_chunks::<4>().0;
    let covered = texels
        .iter()
        .filter(|texel| f32::from(texel[3]) * scale >= threshold)
        .count();
    #[allow(clippy::cast_precision_loss)]
    let fraction = covered as f32 / texels.len().max(1) as f32;
    fraction
}

/// The alpha scale at which `level` covers `target` of its texels, by bisection: coverage only
/// grows with the scale. A level of all-opaque or all-clear texels is left alone.
fn coverage_scale(level: &[u8], target: f32) -> f32 {
    let (mut low, mut high) = (1.0_f32, 1.0_f32);
    if covered_fraction(level, 1.0) < target {
        while covered_fraction(level, high) < target && high < 64.0 {
            high *= 2.0;
        }
    } else {
        while covered_fraction(level, low) > target && low > 1.0 / 64.0 {
            low *= 0.5;
        }
    }
    for _ in 0..12 {
        let middle = f32::midpoint(low, high);
        if covered_fraction(level, middle) < target {
            low = middle;
        } else {
            high = middle;
        }
    }
    high
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, texel: [u8; 4]) -> Vec<u8> {
        texel.repeat(width as usize * height as usize)
    }

    #[test]
    fn chains_halve_each_axis_to_one_texel() {
        let levels = Chain::Linear.build(12, 5, solid(12, 5, [1, 2, 3, 4]));
        let extents: Vec<usize> = levels.levels.iter().map(|l| l.len() / 4).collect();
        // 12x5, 6x2, 3x1, 1x1.
        assert_eq!(extents, [60, 12, 3, 1]);
        assert_eq!(chain_len(12, 5), 4);
        assert_eq!(chain_len(768, 768), 10);
        assert_eq!(chain_len(384, 512), 10);
        assert_eq!(levels.levels[3], [1, 2, 3, 4]);
    }

    #[test]
    fn colour_averages_in_linear_light() {
        // Black and white average to linear 0.5, which is sRGB 188, not 128.
        let base = [[0, 0, 0, 0], [255, 255, 255, 255]].concat().repeat(2);
        let levels = Chain::Color.build(2, 2, base.clone());
        assert_eq!(levels.levels[1], [188, 188, 188, 128]);
        let linear = Chain::Linear.build(2, 2, base);
        assert_eq!(linear.levels[1], [128, 128, 128, 128]);
        for byte in 0..=255 {
            assert_eq!(linear_to_srgb(srgb_to_linear(byte)), byte);
        }
    }

    #[test]
    fn normals_average_as_unit_vectors() {
        // Two normals leaning ±45° about Y average to straight up, at full length.
        let lean = |x: f32| {
            [
                round_byte((x * 0.5 + 0.5) * 255.0),
                128,
                round_byte((std::f32::consts::FRAC_1_SQRT_2 * 0.5 + 0.5) * 255.0),
                200,
            ]
        };
        let a = lean(std::f32::consts::FRAC_1_SQRT_2);
        let b = lean(-std::f32::consts::FRAC_1_SQRT_2);
        let base = [a, b, a, b].concat();
        let levels = Chain::Normal.build(2, 2, base);
        let top = &levels.levels[1];
        assert!((i32::from(top[0]) - 128).abs() <= 1, "{top:?}");
        assert_eq!(top[2], 255, "renormalised to unit length: {top:?}");
        assert_eq!(top[3], 200, "alpha is a scalar, box filtered");
    }

    #[test]
    fn disagreeing_normals_roughen_their_mip() {
        let lean = |x: f32, roughness: u8| {
            let z = (1.0 - x * x).sqrt();
            [
                round_byte((x * 0.5 + 0.5) * 255.0),
                128,
                round_byte((z * 0.5 + 0.5) * 255.0),
                roughness,
            ]
        };
        // Normals ±45° about Y: the mean points straight up and is far rougher.
        let a = lean(std::f32::consts::FRAC_1_SQRT_2, 100);
        let b = lean(-std::f32::consts::FRAC_1_SQRT_2, 100);
        let bumpy = Chain::NormalRoughness.build(2, 2, [a, b, a, b].concat());
        let top = &bumpy.levels[1];
        assert!((i32::from(top[0]) - 128).abs() <= 1, "{top:?}");
        assert_eq!(top[2], 255, "renormalised to unit length: {top:?}");
        assert!(top[3] > 180, "45° of disagreement reads rough: {top:?}");
        // Agreeing normals keep their roughness.
        let flat = Chain::NormalRoughness.build(2, 2, [a, a, a, a].concat());
        assert!(
            (i32::from(flat.levels[1][3]) - 100).abs() <= 1,
            "{:?}",
            flat.levels[1]
        );
    }

    #[test]
    fn spread_accumulates_from_the_base_through_every_level() {
        // A 4×4 checkerboard of slightly tilted normals: the 1×1 level sees the base's spread,
        // not the zero spread between its already renormalised 2×2 parents.
        let lean = |x: f32| {
            let z = (1.0 - x * x).sqrt();
            [
                round_byte((x * 0.5 + 0.5) * 255.0),
                128,
                round_byte((z * 0.5 + 0.5) * 255.0),
                0,
            ]
        };
        let base: Vec<u8> = (0..16)
            .flat_map(|i| lean(if (i + i / 4) % 2 == 0 { 0.3 } else { -0.3 }))
            .collect();
        let levels = Chain::NormalRoughness.build(4, 4, base);
        let level_one = levels.levels[1][3];
        let level_two = levels.levels[2][3];
        assert!(level_one > 60, "{level_one}");
        assert!(
            (i32::from(level_two) - i32::from(level_one)).abs() <= 2,
            "{level_one} {level_two}"
        );
    }

    #[test]
    fn tangent_normals_stay_unit_and_opaque_for_z_to_be_rebuilt() {
        // A long normal is shortened to unit length at the base, so x and y alone still say it.
        let long = [255, 128, 255, 7];
        let levels = Chain::TangentNormal.build(2, 2, [long, long, long, long].concat());
        let base = decode_normal(levels.levels[0][..4].try_into().unwrap());
        assert!(
            (base[0] - base[2]).abs() < 0.02 && base[0] < 0.72,
            "{base:?}"
        );
        assert_eq!(levels.levels[0][3], 255);
        // Two opposed tilts average to straight up.
        let (a, b) = ([200, 128, 200, 255], [55, 128, 200, 255]);
        let top = Chain::TangentNormal.build(2, 2, [a, b, a, b].concat());
        assert_eq!(top.levels[1], vec![128, 128, 255, 255]);
    }

    #[test]
    fn a_surface_box_filters_all_but_its_widened_roughness() {
        let (a, b) = ([200, 128, 200, 255], [55, 128, 200, 255]);
        let bumpy = [a, b, a, b].concat();
        let flat = [[128, 128, 255, 255]; 4].concat();
        let base = [
            [0, 200, 60, 255],
            [100, 100, 60, 255],
            [0, 200, 60, 255],
            [100, 100, 60, 255],
        ]
        .concat();
        let widened = Chain::Surface.build_widened(2, 2, base.clone(), Some(&bumpy));
        assert_eq!(widened.levels[0], base, "the base is as authored");
        let top = &widened.levels[1];
        assert_eq!((top[0], top[1], top[3]), (50, 150, 255), "box filtered");
        assert!(top[2] > 100, "disagreeing normals roughen it: {}", top[2]);
        // The roughness of one chain is the other's, however it's packed.
        let packed: Vec<u8> = bumpy
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|t| [t[0], t[1], t[2], 60])
            .collect();
        assert_eq!(
            Chain::NormalRoughness.build(2, 2, packed).levels[1][3],
            top[2]
        );
        // Flat normals widen nothing, and neither does building it with none.
        let still = Chain::Surface.build_widened(2, 2, base.clone(), Some(&flat));
        assert_eq!(still.levels[1][2], 60);
        assert_eq!(Chain::Surface.build(2, 2, base).levels[1][2], 60);
    }

    #[test]
    fn cutouts_keep_their_covered_fraction() {
        // A sparse 16x16 card: hashed alpha, skewed toward clear, about a fifth covered.
        let base: Vec<u8> = (0_u32..256)
            .flat_map(|i| {
                let u = f32::from((i.wrapping_mul(2_654_435_761) >> 24) as u8) / 255.0;
                [40, 120, 30, round_byte(u * u * u * 255.0)]
            })
            .collect();
        let target = covered_fraction(&base, 1.0);
        let cutout = Chain::Cutout.build(16, 16, base.clone());
        let plain = Chain::Color.build(16, 16, base);
        for level in 1..4 {
            let kept = covered_fraction(&cutout.levels[level], 1.0);
            assert!(
                (kept - target).abs() < 0.07,
                "level {level}: {kept} vs {target}"
            );
            assert!(covered_fraction(&plain.levels[level], 1.0) < kept);
        }
    }

    #[test]
    fn distant_ground_flattens_its_coarse_tail() {
        let base: Vec<u8> = (0..64 * 64)
            .flat_map(|i| [u8::try_from(i % 2 * 255).unwrap(), 0, 0, 255])
            .collect();
        let levels = Chain::ColorFlatDistant.build(64, 64, base);
        let average = levels.levels.last().unwrap().clone();
        // 4x4 is fully flat; 64x64 untouched.
        assert!(levels.levels[4].chunks(4).all(|t| t == average.as_slice()));
        assert!(levels.levels[0].chunks(4).any(|t| t != average.as_slice()));
    }
}
