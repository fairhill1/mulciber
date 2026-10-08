//! Tests of Mulciber's own WGSL library through host evaluators generated from it, so the WGSL
//! that shaders import is what is measured.
//!
//! The generated evaluators are checked in under `src/library_fixtures/` so review sees them.
//! After changing a module, regenerate them with
//! `MULCIBER_REGENERATE_HOST_FIXTURES=1 cargo test -p mulciber-shader library_fixtures`.

use std::f64::consts::PI;
use std::path::Path;

use crate::{ShaderModules, bake_dfg_table, dfg_value};

mod colorspace {
    include!("library_fixtures/colorspace.rs");
}
mod photometry {
    include!("library_fixtures/photometry.rs");
}
mod pbr {
    include!("library_fixtures/pbr.rs");
}
mod tonemap {
    include!("library_fixtures/tonemap.rs");
}

const FIXTURES: &[(&str, &str, &[&str])] = &[
    (
        "colorspace",
        "mulciber::colorspace",
        &["srgb_to_linear", "linear_to_srgb", "luminance"],
    ),
    (
        "photometry",
        "mulciber::photometry",
        &[
            "point_light_intensity",
            "spot_light_intensity",
            "focused_spot_light_intensity",
            "range_window",
            "distance_attenuation",
            "punctual_illuminance",
            "spot_angle_attenuation",
            "ev100_from_camera",
            "ev100_from_luminance",
            "exposure_from_ev100",
            "pre_expose",
            "pre_expose_intensity",
        ],
    ),
    (
        "pbr",
        "mulciber::pbr",
        &[
            "clamp_perceptual_roughness",
            "alpha_from_perceptual_roughness",
            "clamped_n_dot_v",
            "f0_from_metallic",
            "diffuse_color_from_metallic",
            "d_ggx",
            "v_smith_ggx_correlated",
            "f_schlick",
            "lambert",
            "specular_brdf",
            "punctual_diffuse",
            "punctual_specular",
            "specular_dfg",
            "energy_compensation",
            "environment_specular",
            "lod_from_roughness",
        ],
    ),
    ("tonemap", "mulciber::tonemap", &["hue_preserving_shoulder"]),
];

#[test]
fn library_fixtures_match_the_wgsl() {
    let directory =
        std::env::temp_dir().join(format!("mulciber-shader-library-{}", std::process::id()));
    let regenerate = std::env::var_os("MULCIBER_REGENERATE_HOST_FIXTURES").is_some();
    let modules = ShaderModules::new();
    for (file, module, functions) in FIXTURES {
        let qualified: Vec<String> = functions
            .iter()
            .map(|function| format!("{module}::{function}"))
            .collect();
        let qualified: Vec<&str> = qualified.iter().map(String::as_str).collect();
        let output = directory.join(format!("{file}.rs"));
        modules
            .compile_host_field(&output, &qualified)
            .unwrap_or_else(|error| panic!("{module} generates: {error}"));
        let generated = std::fs::read_to_string(&output).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/library_fixtures")
            .join(format!("{file}.rs"));
        if regenerate {
            std::fs::write(&fixture, &generated).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&fixture).unwrap();
        assert!(
            generated == expected,
            "{} is stale; regenerate it with MULCIBER_REGENERATE_HOST_FIXTURES=1",
            fixture.display()
        );
    }
    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------------------------
// mulciber::colorspace

#[test]
fn colorspace_converts_and_weighs_as_specified() {
    // IEC 61966-2-1 reference points and the BT.709 luminance weights.
    let linear = colorspace::srgb_to_linear([0.0, 0.5, 1.0]);
    assert!(linear[0].abs() < 1e-7);
    assert!((linear[1] - 0.214_041_14).abs() < 1e-6, "{linear:?}");
    assert!((linear[2] - 1.0).abs() < 1e-6, "{linear:?}");
    assert!((colorspace::srgb_to_linear([0.04, 0.0, 0.0])[0] - 0.04 / 12.92).abs() < 1e-9);
    assert!((colorspace::srgb_to_linear([-0.5, 0.0, 0.0])[0] + 0.5 / 12.92).abs() < 1e-7);
    for value in [0.001_f32, 0.003_130_8, 0.02, 0.18, 0.5, 0.9, 1.0, 4.0] {
        let round_trip =
            colorspace::srgb_to_linear(colorspace::linear_to_srgb([value, value, value]))[0];
        assert!(
            (round_trip - value).abs() <= value * 1e-5,
            "{value} -> {round_trip}"
        );
    }
    assert!((colorspace::luminance([1.0, 1.0, 1.0]) - 1.0).abs() < 1e-6);
    assert!((colorspace::luminance([0.0, 1.0, 0.0]) - 0.7152).abs() < 1e-7);
}

// ---------------------------------------------------------------------------------------------
// mulciber::photometry

fn close(actual: f32, expected: f64, relative: f64) -> bool {
    (f64::from(actual) - expected).abs() <= expected.abs() * relative
}

#[test]
fn falloff_is_inverse_square_inside_the_range_and_zero_at_it() {
    let range = 100.0_f32;
    for distance in [0.5_f32, 1.0, 2.0, 5.0, 10.0] {
        let illuminance = photometry::punctual_illuminance(800.0, distance * distance, range);
        let inverse_square = 800.0 / f64::from(distance * distance);
        // (d/r)⁴ ≤ 1e-4 here, so the window costs at most 2e-4.
        assert!(
            close(illuminance, inverse_square, 2.5e-4),
            "{distance} m: {illuminance} lx against {inverse_square}"
        );
    }
    assert!(photometry::punctual_illuminance(800.0, range * range, range).abs() < 1e-9);
    assert!(photometry::punctual_illuminance(800.0, 4.0 * range * range, range).abs() < 1e-9);
    // Half the range: the window is (1 − 1/16)².
    assert!(close(
        photometry::range_window(25.0, 10.0),
        (15.0_f64 / 16.0).powi(2),
        1e-6
    ));
    // Closer than 1 cm the light is a 1 cm sphere.
    assert!(close(
        photometry::distance_attenuation(0.0, 10.0),
        1.0e4,
        1e-6
    ));
    assert!(close(
        photometry::distance_attenuation(1.0e-6, 10.0),
        1.0e4,
        1e-6
    ));
    // The window ends with zero slope: just inside the range it is nearly zero.
    let near_edge = photometry::range_window(0.999 * 0.999 * range * range, range);
    assert!(near_edge < 2e-5, "{near_edge}");
}

#[test]
fn lumens_become_candela_for_points_and_spots() {
    assert!(close(
        photometry::point_light_intensity(800.0),
        800.0 / (4.0 * PI),
        1e-6
    ));
    assert!(close(
        photometry::spot_light_intensity(800.0),
        800.0 / PI,
        1e-6
    ));
    // A focused spot with a 90° half-angle covers the hemisphere, 2π sr.
    assert!(close(
        photometry::focused_spot_light_intensity(800.0, 0.0),
        800.0 / (2.0 * PI),
        1e-6
    ));
    let cos_outer = 30.0_f64.to_radians().cos();
    #[allow(clippy::cast_possible_truncation)]
    let focused = photometry::focused_spot_light_intensity(800.0, cos_outer as f32);
    assert!(close(focused, 800.0 / (2.0 * PI * (1.0 - cos_outer)), 1e-5));

    let (cos_inner, cos_outer) = (0.9_f32, 0.8_f32);
    assert!((photometry::spot_angle_attenuation(0.95, cos_inner, cos_outer) - 1.0).abs() < 1e-6);
    assert!(photometry::spot_angle_attenuation(0.75, cos_inner, cos_outer).abs() < 1e-9);
    assert!((photometry::spot_angle_attenuation(0.85, cos_inner, cos_outer) - 0.25).abs() < 1e-5);
}

#[test]
fn exposure_follows_ev100() {
    assert!(close(photometry::exposure_from_ev100(0.0), 1.0 / 1.2, 1e-6));
    assert!(close(
        photometry::exposure_from_ev100(15.0),
        1.0 / (1.2 * 32768.0),
        1e-6
    ));
    // Each stop halves the exposure.
    for ev100 in [-2.0_f32, 3.5, 9.0, 14.0] {
        let ratio =
            photometry::exposure_from_ev100(ev100 + 1.0) / photometry::exposure_from_ev100(ev100);
        assert!((ratio - 0.5).abs() < 1e-6);
    }
    // Sunny 16: f/16, 1/100 s, ISO 100 is EV100 log2(25600) ≈ 14.64.
    assert!(close(
        photometry::ev100_from_camera(16.0, 0.01, 100.0),
        25_600.0_f64.log2(),
        1e-6
    ));
    // Doubling ISO lowers EV100 by one stop.
    assert!(
        (photometry::ev100_from_camera(16.0, 0.01, 200.0)
            - (photometry::ev100_from_camera(16.0, 0.01, 100.0) - 1.0))
            .abs()
            < 1e-5
    );
    // Metering with K = 12.5 maps the average luminance to 1/9.6 of the tone mapper's white.
    for luminance in [0.5_f32, 40.0, 8000.0] {
        let exposed = luminance
            * photometry::exposure_from_ev100(photometry::ev100_from_luminance(luminance));
        assert!(close(exposed, 1.0 / 9.6, 1e-5), "{luminance}: {exposed}");
    }
    let exposure = photometry::exposure_from_ev100(12.0);
    assert_eq!(
        photometry::pre_expose([2.0, 4.0, 8.0], exposure),
        [2.0 * exposure, 4.0 * exposure, 8.0 * exposure]
    );
    assert_eq!(
        photometry::pre_expose_intensity(100.0, exposure),
        100.0 * exposure
    );
}

// ---------------------------------------------------------------------------------------------
// mulciber::pbr

/// Midpoint quadrature over the hemisphere of half vectors, in `1 − n·h` on a logarithmic grid
/// (so even the sharpest clamped lobe gets thousands of samples across its peak) and azimuth.
/// `integrand(n_dot_h, phi)` is integrated against `dω_h`.
fn half_vector_quadrature(integrand: impl Fn(f64, f64) -> f64) -> f64 {
    const RADIAL: u32 = 3000;
    const AZIMUTH: u32 = 256;
    let (start, end) = (1e-10_f64.ln(), 0.0_f64);
    let step = (end - start) / f64::from(RADIAL);
    let mut sum = 0.0;
    for i in 0..RADIAL {
        // dμ = t ds with t = 1 − μ = e^s.
        let t = (start + (f64::from(i) + 0.5) * step).exp();
        let n_dot_h = 1.0 - t;
        let mut ring = 0.0;
        for j in 0..AZIMUTH {
            let phi = (f64::from(j) + 0.5) * PI / f64::from(AZIMUTH);
            ring += integrand(n_dot_h, phi);
        }
        // Symmetric in φ: integrate [0, π] and double.
        sum += ring * (PI / f64::from(AZIMUTH)) * 2.0 * t * step;
    }
    sum
}

#[allow(clippy::cast_possible_truncation)]
fn f32_of(value: f64) -> f32 {
    value as f32
}

/// Directional albedo `∫ f_spec n·l dω_l` of the WGSL specular BRDF for `f0`, by quadrature over
/// half vectors (`dω_l = 4 v·h dω_h`), with v in the xz-plane.
fn specular_albedo(n_dot_v: f64, perceptual_roughness: f64, f0: f32) -> f64 {
    let alpha = pbr::alpha_from_perceptual_roughness(f32_of(perceptual_roughness));
    let view = [(1.0 - n_dot_v * n_dot_v).sqrt(), 0.0, n_dot_v];
    half_vector_quadrature(|n_dot_h, phi| {
        let sin_h = (1.0 - n_dot_h * n_dot_h).max(0.0).sqrt();
        let half = [sin_h * phi.cos(), sin_h * phi.sin(), n_dot_h];
        let v_dot_h = view[0] * half[0] + view[2] * half[2];
        if v_dot_h <= 0.0 {
            return 0.0;
        }
        let n_dot_l = 2.0 * v_dot_h * half[2] - view[2];
        if n_dot_l <= 0.0 {
            return 0.0;
        }
        let brdf = pbr::specular_brdf(
            f32_of(n_dot_v),
            f32_of(n_dot_l),
            f32_of(n_dot_h),
            f32_of(v_dot_h),
            [f0; 3],
            alpha,
        )[0];
        f64::from(brdf) * n_dot_l * 4.0 * v_dot_h
    })
}

#[test]
fn ggx_distribution_is_normalised() {
    for perceptual in [0.0_f32, 0.089, 0.25, 0.5, 0.75, 1.0] {
        let alpha = pbr::alpha_from_perceptual_roughness(perceptual);
        let projected = half_vector_quadrature(|n_dot_h, _| {
            f64::from(pbr::d_ggx(f32_of(n_dot_h), alpha)) * n_dot_h
        });
        assert!(
            (projected - 1.0).abs() < 2e-3,
            "perceptual roughness {perceptual}: ∫ D (n·h) dω = {projected}"
        );
    }
}

#[test]
fn roughness_is_clamped_and_squared() {
    assert!((pbr::alpha_from_perceptual_roughness(0.0) - 0.089 * 0.089).abs() < 1e-9);
    assert!((pbr::alpha_from_perceptual_roughness(0.5) - 0.25).abs() < 1e-9);
    assert!((pbr::alpha_from_perceptual_roughness(2.0) - 1.0).abs() < 1e-9);
    assert!((pbr::lod_from_roughness(0.5, 6.0) - 3.0).abs() < 1e-9);
    let f0 = pbr::f0_from_metallic([0.9, 0.6, 0.2], 0.0);
    assert!(f0.iter().all(|value| (value - 0.04).abs() < 1e-7));
    assert_eq!(pbr::f0_from_metallic([0.9, 0.6, 0.2], 1.0), [0.9, 0.6, 0.2]);
    assert_eq!(
        pbr::diffuse_color_from_metallic([0.9, 0.6, 0.2], 1.0),
        [0.0; 3]
    );
}

#[test]
fn visibility_and_the_specular_brdf_are_reciprocal() {
    for alpha in [0.0079_f32, 0.1, 0.5, 1.0] {
        for a in [0.05_f32, 0.3, 0.7, 1.0] {
            for b in [0.02_f32, 0.4, 0.9] {
                assert_eq!(
                    pbr::v_smith_ggx_correlated(a, b, alpha),
                    pbr::v_smith_ggx_correlated(b, a, alpha)
                );
            }
        }
    }
    // f(v, l) n·l E / n·l = f(l, v) n·v E / n·v for a whole punctual evaluation.
    let n = [0.0, 0.0, 1.0];
    let normalize = |v: [f32; 3]| {
        let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        [v[0] / length, v[1] / length, v[2] / length]
    };
    let v = normalize([0.6, 0.1, 0.5]);
    let l = normalize([-0.3, 0.4, 0.8]);
    for roughness in [0.2_f32, 0.5, 0.9] {
        let forward = pbr::punctual_specular(n, v, l, [1.0; 3], [0.8, 0.5, 0.3], 0.7, roughness);
        let backward = pbr::punctual_specular(n, l, v, [1.0; 3], [0.8, 0.5, 0.3], 0.7, roughness);
        for channel in 0..3 {
            let there = forward[channel] / l[2];
            let back = backward[channel] / v[2];
            assert!(
                (there - back).abs() <= there * 1e-5,
                "roughness {roughness}: {there} against {back}"
            );
        }
    }
}

#[test]
fn white_furnace_conserves_energy_and_matches_the_dfg_table() {
    for perceptual in [0.089_f64, 0.25, 0.5, 0.75, 1.0] {
        for n_dot_v in [0.1_f64, 0.5, 0.9, 1.0] {
            // F = 1 everywhere: f0 = 1 makes Schlick's Fresnel 1.
            let albedo = specular_albedo(n_dot_v, perceptual, 1.0);
            assert!(
                albedo <= 1.0 + 1e-3,
                "roughness {perceptual}, n·v {n_dot_v}: the lobe reflects {albedo}"
            );
            // f0 = 0 leaves only Fc = (1 − v·h)⁵: the table's R channel.
            let fresnel = specular_albedo(n_dot_v, perceptual, 0.0);
            let [r, g] = dfg_value(f32_of(n_dot_v), f32_of(perceptual), 4096);
            assert!(
                (f64::from(g) - albedo).abs() < 4e-3,
                "roughness {perceptual}, n·v {n_dot_v}: DFG G {g} against {albedo}"
            );
            assert!(
                (f64::from(r) - fresnel).abs() < 4e-3,
                "roughness {perceptual}, n·v {n_dot_v}: DFG R {r} against {fresnel}"
            );
        }
    }
    // A rough lobe loses energy to single scattering; a smooth one barely does.
    assert!(specular_albedo(0.5, 1.0, 1.0) < 0.8);
    assert!(specular_albedo(0.5, 0.089, 1.0) > 0.99);
}

#[test]
fn dfg_table_has_the_smooth_limit_and_filaments_layout() {
    // For a near-mirror the lobe reflects everything (G → 1) and the Fresnel part is Schlick's
    // at n·v (R → (1 − n·v)⁵).
    for n_dot_v in [0.1_f32, 0.3, 0.5, 0.8, 1.0] {
        let [r, g] = dfg_value(n_dot_v, 0.02, 1024);
        assert!((g - 1.0).abs() < 2e-3, "n·v {n_dot_v}: G {g}");
        let schlick = (1.0 - n_dot_v).powi(5);
        assert!(
            (r - schlick).abs() < 2e-3,
            "n·v {n_dot_v}: R {r} against {schlick}"
        );
    }
    // G falls with roughness at every n·v, and R ≤ G.
    for n_dot_v in [0.2_f32, 0.6, 1.0] {
        let mut previous = f32::INFINITY;
        for perceptual in [0.1_f32, 0.3, 0.5, 0.7, 0.9, 1.0] {
            let [r, g] = dfg_value(n_dot_v, perceptual, 1024);
            assert!(
                g < previous && r <= g,
                "n·v {n_dot_v}, roughness {perceptual}"
            );
            previous = g;
        }
    }

    // Texel centres, deterministic bakes, and the upload layouts.
    let table = bake_dfg_table(8, 256);
    assert_eq!(table, bake_dfg_table(8, 256));
    assert_eq!(table.size(), 8);
    assert_eq!(table.texels().len(), 64);
    assert_eq!(table.texel(2, 5), dfg_value(2.5 / 8.0, 5.5 / 8.0, 256));
    let rgba = table.rgba();
    assert_eq!(
        rgba[5 * 8 + 2],
        [table.texel(2, 5)[0], table.texel(2, 5)[1], 0.0, 1.0]
    );
    let bytes = table.to_le_bytes();
    assert_eq!(bytes.len(), 64 * 8);
    let at = (5 * 8 + 2) * 8;
    assert_eq!(
        f32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()),
        table.texel(2, 5)[1]
    );
}

#[test]
fn split_sum_terms_follow_filament() {
    let dfg = [0.1_f32, 0.8];
    // mix(R, G, F0): a dielectric reflects mostly the Fresnel part, a white metal all of G.
    assert!((pbr::specular_dfg([0.0; 3], dfg)[0] - 0.1).abs() < 1e-7);
    assert!((pbr::specular_dfg([1.0; 3], dfg)[0] - 0.8).abs() < 1e-7);
    // With compensation a white metal reflects everything, whatever single scattering lost.
    let compensation = pbr::energy_compensation([1.0; 3], dfg);
    assert!((compensation[0] - 1.25).abs() < 1e-6);
    let white = pbr::environment_specular([1.0; 3], [1.0; 3], dfg);
    assert!((white[0] - 1.0).abs() < 1e-6, "{white:?}");
    // A dielectric's compensation is small: 1 + 0.04 (1/G − 1).
    assert!((pbr::energy_compensation([0.04; 3], dfg)[0] - 1.01).abs() < 1e-6);
}

#[test]
fn punctual_light_is_lambert_plus_the_lobe_times_illuminance() {
    let n = [0.0, 0.0, 1.0];
    let l = [0.0, 0.6, 0.8];
    let v = [0.0, -0.6, 0.8];
    let illuminance = [1000.0, 800.0, 600.0];
    let base = [0.5, 0.4, 0.3];
    let diffuse = pbr::punctual_diffuse(n, l, illuminance, base, 0.0);
    for channel in 0..3 {
        let expected = f64::from(base[channel]) / PI * f64::from(illuminance[channel]) * 0.8;
        assert!(close(diffuse[channel], expected, 1e-5));
    }
    assert_eq!(
        pbr::punctual_diffuse(n, l, illuminance, base, 1.0),
        [0.0; 3]
    );
    // Mirror geometry puts h on n: D(1) V F(v·h) E n·l.
    let specular = pbr::punctual_specular(n, v, l, illuminance, base, 0.0, 0.5);
    let alpha = 0.25_f32;
    let lobe = pbr::d_ggx(1.0, alpha)
        * pbr::v_smith_ggx_correlated(0.8, 0.8, alpha)
        * pbr::f_schlick([0.04; 3], 1.0, 0.8)[0];
    assert!(close(specular[0], f64::from(lobe * 1000.0 * 0.8), 1e-5));
    // A light behind the surface adds nothing.
    let behind = [0.0, 0.6, -0.8];
    assert_eq!(
        pbr::punctual_diffuse(n, behind, illuminance, base, 0.0),
        [0.0; 3]
    );
    assert_eq!(
        pbr::punctual_specular(n, v, behind, illuminance, base, 0.0, 0.5),
        [0.0; 3]
    );
}

// ---------------------------------------------------------------------------------------------
// mulciber::tonemap

#[test]
fn shoulder_is_identity_below_it_monotonic_and_hue_preserving() {
    for color in [
        [0.0_f32, 0.0, 0.0],
        [0.6, 0.3, 0.1],
        [0.2, 0.59, 0.4],
        [0.05; 3],
    ] {
        assert_eq!(tonemap::hue_preserving_shoulder(color), color);
    }
    assert_eq!(
        tonemap::hue_preserving_shoulder([-1.0, 0.3, 0.2]),
        [0.0, 0.3, 0.2]
    );
    // Isle of Rán's tuning: 1 → 0.8, 2 → 0.911, 10 → 0.984, never reaching 1.
    for (peak, expected) in [
        (1.0_f32, 0.8_f64),
        (2.0, 0.6 + 0.4 * 1.4 / 1.8),
        (10.0, 0.6 + 0.4 * 9.4 / 9.8),
    ] {
        assert!(close(
            tonemap::hue_preserving_shoulder([peak, 0.0, 0.0])[0],
            expected,
            1e-6
        ));
    }
    assert!(tonemap::hue_preserving_shoulder([1.0e6, 0.0, 0.0])[0] < 1.0);

    let hue = [1.0_f32, 0.55, 0.2];
    let mut previous = [0.0_f32; 3];
    for step in 1..4000 {
        #[allow(clippy::cast_precision_loss)]
        let scale = step as f32 * 0.005;
        let input = hue.map(|channel| channel * scale);
        let output = tonemap::hue_preserving_shoulder(input);
        for channel in 0..3 {
            assert!(
                output[channel] >= previous[channel],
                "not monotonic at {scale}"
            );
            // Channel ratios are those of the input.
            let ratio = output[channel] / output[0];
            assert!((ratio - hue[channel]).abs() < 1e-5, "hue drifts at {scale}");
        }
        assert!(output[0] < 1.0);
        previous = output;
    }
    // Slope 1 where the shoulder meets the identity.
    let just_above = tonemap::hue_preserving_shoulder([0.6001, 0.0, 0.0])[0];
    assert!((just_above - 0.6001).abs() < 1e-6, "{just_above}");
}

// ---------------------------------------------------------------------------------------------
// The modules together, as a game's lit shader uses them.

#[test]
fn a_lit_fragment_shader_composes_with_every_library_module() {
    let modules = ShaderModules::new();
    let shader = modules.shader_source(
        "lit.wgsl",
        include_str!("library_fixtures/lit_example.wgsl"),
    );
    let (module, info) = shader
        .compose(&[])
        .expect("the README's lit shader composes");
    let words = naga::back::spv::write_vec(
        &module,
        &info,
        &naga::back::spv::Options {
            lang_version: (1, 4),
            ..Default::default()
        },
        None,
    )
    .expect("SPIR-V generation");
    assert_eq!(words.first().copied(), Some(0x0723_0203));
    crate::metal_source(&module, &info).expect("MSL generation");
    assert!(
        include_str!("../README.md").contains(include_str!("library_fixtures/lit_example.wgsl")),
        "the README's lit shader differs from src/library_fixtures/lit_example.wgsl"
    );
}
