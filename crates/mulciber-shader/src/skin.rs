//! Pre-integrated skin diffusion (Penner, Pre-Integrated Skin Shading, SIGGRAPH 2011).
//!
//! Light scatters a little way under skin, red furthest, so where it falls away round a curve (a
//! nose, a cheek, a chin) skin takes it more softly than Lambert's `n·l`, warming toward the shade.
//! The table holds how much it takes, red, green and blue, by `n·l` and by how curved the surface
//! is there: `n·l` from −1 to 1 across, curvature from 0 to [`SKIN_MOST_CURVATURE`] per metre
//! down, the ends on texel centres. On the flat it is Lambert's, so it only softens and warms what
//! Lambert has. `mulciber::skin`'s `skin_diffuse` samples it; `mulciber-model`'s
//! `vertex_curvature` works out how curved a mesh is at each vertex.
//!
//! The table is one for every skin: bake it in a `build.rs` and upload it as an `RGBA16Float`
//! texture with a linear, clamp-to-edge sampler.

/// The table's width: `n·l` from −1 to 1.
pub const SKIN_TABLE_WIDTH: u32 = 128;

/// The table's height: curvature from 0 to [`SKIN_MOST_CURVATURE`].
pub const SKIN_TABLE_HEIGHT: u32 = 64;

/// The most curvature the table holds, per metre: `mulciber::skin`'s `SKIN_MOST_CURVATURE`.
pub const SKIN_MOST_CURVATURE: f32 = 500.0;

/// How far light spreads under skin, in metres, red, green and blue: the width of a Gaussian each.
/// An artist's profile (Isle of Rán's and Deadly Voyage's) rather than a measured one; a game may
/// bake its own.
pub const SKIN_SPREAD: [f32; 3] = [0.003, 0.0013, 0.0007];

/// Samples across the Gaussian, out to four widths either side.
const SAMPLES: u32 = 96;

/// A baked skin diffusion table, row-major, curvature increasing by row.
#[derive(Clone, Debug, PartialEq)]
pub struct SkinTable {
    texels: Vec<[f32; 3]>,
}

impl SkinTable {
    /// Width in texels: [`SKIN_TABLE_WIDTH`].
    #[must_use]
    pub const fn width(&self) -> u32 {
        SKIN_TABLE_WIDTH
    }

    /// Height in texels: [`SKIN_TABLE_HEIGHT`].
    #[must_use]
    pub const fn height(&self) -> u32 {
        SKIN_TABLE_HEIGHT
    }

    /// Every texel's red, green and blue, row-major from flat skin.
    #[must_use]
    pub fn texels(&self) -> &[[f32; 3]] {
        &self.texels
    }

    /// RGBA texels for `TexelData::Rgba16Float` and `Device::create_texture`: alpha 1.
    #[must_use]
    pub fn rgba(&self) -> Vec<[f32; 4]> {
        self.texels
            .iter()
            .map(|&[r, g, b]| [r, g, b, 1.0])
            .collect()
    }

    /// The texels as little-endian `f32` triples, for a `build.rs` to write and the game to
    /// `include_bytes!`: 12 bytes per texel, red, green, blue, in [`texels`](Self::texels) order.
    #[must_use]
    pub fn to_le_bytes(&self) -> Vec<u8> {
        self.texels
            .iter()
            .flatten()
            .flat_map(|channel| channel.to_le_bytes())
            .collect()
    }
}

/// Bakes the table for light spreading `spread` metres under skin, red, green and blue
/// ([`SKIN_SPREAD`] for the profile both games use).
#[must_use]
pub fn bake_skin_table(spread: [f32; 3]) -> SkinTable {
    let mut texels = Vec::with_capacity((SKIN_TABLE_WIDTH * SKIN_TABLE_HEIGHT) as usize);
    for y in 0..SKIN_TABLE_HEIGHT {
        #[allow(clippy::cast_precision_loss)]
        let curvature = y as f32 / (SKIN_TABLE_HEIGHT - 1) as f32 * SKIN_MOST_CURVATURE;
        for x in 0..SKIN_TABLE_WIDTH {
            #[allow(clippy::cast_precision_loss)]
            let cosine = x as f32 / (SKIN_TABLE_WIDTH - 1) as f32 * 2.0 - 1.0;
            texels.push(skin_diffusion(cosine, curvature, spread));
        }
    }
    SkinTable { texels }
}

/// How much of a light at `cosine` (`n·l`) skin curved `curvature` (per metre) takes, red, green
/// and blue, for light spreading `spread` metres under it: Lambert's over the curve, blurred by a
/// Gaussian that wide. What [`bake_skin_table`] stores at a texel.
#[must_use]
pub fn skin_diffusion(cosine: f32, curvature: f32, spread: [f32; 3]) -> [f32; 3] {
    let angle = cosine.clamp(-1.0, 1.0).acos();
    let mut sum = [0.0_f32; 3];
    let mut weight = 0.0_f32;
    for i in 0..SAMPLES {
        #[allow(clippy::cast_precision_loss)]
        let t = ((i as f32 + 0.5) / SAMPLES as f32 * 2.0 - 1.0) * 4.0;
        let w = (-0.5 * t * t).exp();
        weight += w;
        for (channel, spread) in spread.iter().enumerate() {
            sum[channel] += w * (angle + curvature * spread * t).cos().max(0.0);
        }
    }
    sum.map(|channel| channel / weight)
}

#[cfg(test)]
mod tests {
    use super::{
        SKIN_MOST_CURVATURE, SKIN_SPREAD, SKIN_TABLE_HEIGHT, SKIN_TABLE_WIDTH, bake_skin_table,
        skin_diffusion,
    };

    /// Flat, skin takes a light as Lambert has it; round a curve, red reaches furthest past where
    /// the light falls away, and nothing is taken from straight behind.
    #[test]
    fn flat_skin_is_lambert_and_red_spreads_furthest_at_the_edge() {
        for cosine in [-1.0, -0.5, 0.0, 0.3, 0.7, 1.0_f32] {
            let taken = skin_diffusion(cosine, 0.0, SKIN_SPREAD);
            assert!(
                taken.iter().all(|c| (c - cosine.max(0.0)).abs() < 1.0e-5),
                "{cosine}: {taken:?}"
            );
        }
        let [r, g, b] = skin_diffusion(0.0, 40.0, SKIN_SPREAD);
        assert!(r > g && g > b && r < 0.1, "{r} {g} {b}");
        assert!(
            skin_diffusion(-1.0, 40.0, SKIN_SPREAD)
                .iter()
                .all(|c| *c < 1.0e-5)
        );
        for curvature in [0.0, 20.0, 100.0, SKIN_MOST_CURVATURE] {
            for i in 0..101_u8 {
                let taken = skin_diffusion(f32::from(i) / 50.0 - 1.0, curvature, SKIN_SPREAD);
                assert!(
                    taken
                        .iter()
                        .all(|c| c.is_finite() && (0.0..=1.00001).contains(c))
                );
            }
        }
    }

    #[test]
    fn the_table_holds_the_diffusion_at_its_texels() {
        let table = bake_skin_table(SKIN_SPREAD);
        assert_eq!(
            table.texels().len(),
            (SKIN_TABLE_WIDTH * SKIN_TABLE_HEIGHT) as usize
        );
        let last = table.texels().len() - 1;
        assert_eq!(
            table.texels()[last],
            skin_diffusion(1.0, SKIN_MOST_CURVATURE, SKIN_SPREAD)
        );
        assert_eq!(table.texels()[0], skin_diffusion(-1.0, 0.0, SKIN_SPREAD));
        assert_eq!(table.rgba()[last][3], 1.0);
        let bytes = table.to_le_bytes();
        assert_eq!(bytes.len(), table.texels().len() * 12);
        assert_eq!(
            f32::from_le_bytes(bytes[bytes.len() - 12..][..4].try_into().unwrap()),
            table.texels()[last][0]
        );
    }

    #[test]
    fn the_wgsl_holds_the_same_curvature() {
        assert!(
            include_str!("../wgsl/mulciber/skin.wgsl").contains(&format!(
                "const SKIN_MOST_CURVATURE: f32 = {SKIN_MOST_CURVATURE:.1};"
            ))
        );
    }
}
