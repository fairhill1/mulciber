//! The environment BRDF's DFG lookup table for split-sum image-based specular.
//!
//! The table follows Filament's multiple-scattering layout (`DFV_Multiscatter` in its
//! `CubemapIBL.cpp`): for a view at `n·v` and a GGX lobe of `α = perceptual roughness²`, with
//! `Fc = (1 − v·h)⁵` and `V` the height-correlated Smith visibility,
//!
//! - R = ∫ Fc · D · V · n·l dω, the Fresnel-weighted part, and
//! - G = ∫ D · V · n·l dω, the lobe's directional albedo with F = 1,
//!
//! so a shader reconstructs the split sum's directional albedo for any `F0` (f90 = 1) as
//! `mix(R, G, F0)` and Filament's energy compensation as `1 + F0 (1/G − 1)`. `mulciber::pbr`'s
//! `specular_dfg`, `energy_compensation` and `environment_specular` do exactly that.
//!
//! Each texel is estimated with Filament's GGX importance sampling over a Hammersley sequence, so
//! the bake is deterministic. Texel `(i, j)` of a `size × size` table holds `n·v = (i + ½)/size`
//! and perceptual roughness `(j + ½)/size`, row 0 first: sampling at `(n·v, perceptual
//! roughness)` with a linear clamp-to-edge sampler interpolates between texel centres.

/// Filament's table size: 128 × 128.
pub const DFG_TABLE_SIZE: u32 = 128;

/// Filament's sample count per texel: 1024.
pub const DFG_SAMPLE_COUNT: u32 = 1024;

/// A baked DFG table, row-major, perceptual roughness increasing by row.
#[derive(Clone, Debug, PartialEq)]
pub struct DfgTable {
    size: u32,
    texels: Vec<[f32; 2]>,
}

impl DfgTable {
    /// Width and height in texels.
    #[must_use]
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Every texel's (R, G), row-major from `n·v`, roughness 0 upwards.
    #[must_use]
    pub fn texels(&self) -> &[[f32; 2]] {
        &self.texels
    }

    /// The texel at column `x` (n·v) and row `y` (perceptual roughness).
    ///
    /// # Panics
    ///
    /// If `x` or `y` is outside the table.
    #[must_use]
    pub fn texel(&self, x: u32, y: u32) -> [f32; 2] {
        assert!(
            x < self.size && y < self.size,
            "DFG texel outside the table"
        );
        self.texels[(y * self.size + x) as usize]
    }

    /// RGBA texels for `Device::create_rgba16_float_texture`: (R, G, 0, 1).
    #[must_use]
    pub fn rgba(&self) -> Vec<[f32; 4]> {
        self.texels
            .iter()
            .map(|&[fresnel, albedo]| [fresnel, albedo, 0.0, 1.0])
            .collect()
    }

    /// The texels as little-endian `f32` pairs, for a `build.rs` to write and the game to
    /// `include_bytes!`: 8 bytes per texel, R then G, in [`texels`](Self::texels) order.
    #[must_use]
    pub fn to_le_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.texels.len() * 8);
        for [fresnel, albedo] in &self.texels {
            bytes.extend_from_slice(&fresnel.to_le_bytes());
            bytes.extend_from_slice(&albedo.to_le_bytes());
        }
        bytes
    }
}

/// Bakes a `size × size` DFG table with `samples` importance samples per texel. Filament uses
/// [`DFG_TABLE_SIZE`] and [`DFG_SAMPLE_COUNT`]; at those values the bake is about 17 M samples
/// and took 0.7 s unoptimised on a desktop CPU.
///
/// # Panics
///
/// If `size` or `samples` is zero.
#[must_use]
pub fn bake_dfg_table(size: u32, samples: u32) -> DfgTable {
    assert!(
        size > 0 && samples > 0,
        "a DFG table needs texels and samples"
    );
    let mut texels = Vec::with_capacity((size * size) as usize);
    for y in 0..size {
        let perceptual_roughness = (f64::from(y) + 0.5) / f64::from(size);
        for x in 0..size {
            let n_dot_v = (f64::from(x) + 0.5) / f64::from(size);
            texels.push(dfg_f64(n_dot_v, perceptual_roughness, samples));
        }
    }
    DfgTable { size, texels }
}

/// One DFG value, (R, G), at `n_dot_v` in (0, 1] and `perceptual_roughness` in [0, 1], with
/// `samples` importance samples: what [`bake_dfg_table`] stores at a texel centre.
///
/// # Panics
///
/// If `samples` is zero.
#[must_use]
pub fn dfg_value(n_dot_v: f32, perceptual_roughness: f32, samples: u32) -> [f32; 2] {
    assert!(samples > 0, "a DFG value needs samples");
    dfg_f64(f64::from(n_dot_v), f64::from(perceptual_roughness), samples)
}

#[allow(clippy::cast_possible_truncation)]
fn dfg_f64(n_dot_v: f64, perceptual_roughness: f64, samples: u32) -> [f32; 2] {
    let alpha = perceptual_roughness * perceptual_roughness;
    let view = [(1.0 - n_dot_v * n_dot_v).max(0.0).sqrt(), 0.0, n_dot_v];
    let inverse_count = 1.0 / f64::from(samples);
    let (mut fresnel, mut albedo) = (0.0, 0.0);
    for index in 0..samples {
        let u = hammersley(index, inverse_count);
        let half = importance_sample_ggx(u, alpha);
        let v_dot_h = dot(view, half);
        let light_z = 2.0 * v_dot_h * half[2] - view[2];
        let n_dot_l = light_z.clamp(0.0, 1.0);
        let v_dot_h = v_dot_h.clamp(0.0, 1.0);
        let n_dot_h = half[2].clamp(0.0, 1.0);
        if n_dot_l > 0.0 {
            // pdf(l) = D (n·h) / (4 v·h), so each sample of D V n·l weighs V n·l 4 v·h / n·h.
            let weight = visibility(n_dot_v, n_dot_l, alpha) * n_dot_l * (v_dot_h / n_dot_h);
            let fc = (1.0 - v_dot_h).powi(5);
            fresnel += weight * fc;
            albedo += weight;
        }
    }
    let scale = 4.0 * inverse_count;
    [(fresnel * scale) as f32, (albedo * scale) as f32]
}

/// Point `index` of an `n`-point Hammersley set: (index/n, radical inverse of index in base 2).
fn hammersley(index: u32, inverse_count: f64) -> [f64; 2] {
    let radical_inverse = f64::from(index.reverse_bits()) * (1.0 / 4_294_967_296.0);
    [f64::from(index) * inverse_count, radical_inverse]
}

/// A half vector distributed as D(h) (n·h) for GGX of `alpha`, around +Z (Filament's
/// `hemisphereImportanceSampleDggx`).
fn importance_sample_ggx(u: [f64; 2], alpha: f64) -> [f64; 3] {
    let phi = 2.0 * std::f64::consts::PI * u[0];
    // (α² − 1) is written (α − 1)(α + 1) for accuracy, as Filament does.
    let cos_theta2 = (1.0 - u[1]) / (1.0 + (alpha + 1.0) * ((alpha - 1.0) * u[1]));
    let cos_theta = cos_theta2.sqrt();
    let sin_theta = (1.0 - cos_theta2).max(0.0).sqrt();
    [sin_theta * phi.cos(), sin_theta * phi.sin(), cos_theta]
}

/// Height-correlated Smith visibility, the same expression as `mulciber::pbr`.
fn visibility(n_dot_v: f64, n_dot_l: f64, alpha: f64) -> f64 {
    let a2 = alpha * alpha;
    let lambda_v = n_dot_l * ((n_dot_v - a2 * n_dot_v) * n_dot_v + a2).sqrt();
    let lambda_l = n_dot_v * ((n_dot_l - a2 * n_dot_l) * n_dot_l + a2).sqrt();
    0.5 / (lambda_v + lambda_l)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
