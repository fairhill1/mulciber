#define_import_path mulciber::pbr

// Physically based shading: Lambert diffuse and a Cook-Torrance GGX specular lobe, after Filament
// ("Physically Based Rendering in Filament") and Karis ("Real Shading in Unreal Engine 4").
//
// Conventions:
// - The BRDF is in 1/sr and keeps its π: Lambert is albedo/π, and a punctual light's outgoing
//   luminance is BRDF · E · n·l, with E the light's perpendicular illuminance in lux (see
//   mulciber::photometry). Results are luminance in nits, or pre-exposed if E was.
// - n, v and l are unit vectors pointing away from the surface: v towards the eye, l towards the
//   light.
// - Materials use the metallic workflow: a linear base colour, metallic in [0, 1], and perceptual
//   roughness in [0, 1] as authored. The GGX α is perceptual², with perceptual roughness clamped
//   to 0.089 so α² stays representable in 16-bit floats.
// - Specular uses Schlick's Fresnel with f90 = 1 and F0 = mix(0.04, base colour, metallic);
//   diffuse uses base colour · (1 − metallic).
//
// Image-based specular uses the split sum with Filament's multiple-scattering DFG table, baked
// by mulciber_shader::bake_dfg_table: R holds ∫ Fc V n·l and G ∫ V n·l (Fc = (1 − v·h)⁵), indexed
// by (n·v, perceptual roughness).

const PI: f32 = 3.14159265358979;

// Smallest perceptual roughness: α² = 0.089⁴ ≈ 6.3e-5 is the smallest that fp16 represents.
const MIN_PERCEPTUAL_ROUGHNESS: f32 = 0.089;

// Smallest n·v, so grazing and back-facing views never divide by zero.
const MIN_N_DOT_V: f32 = 0.0001;

// Reflectance at normal incidence of common dielectrics (4%).
const DIELECTRIC_F0: f32 = 0.04;

// Perceptual roughness clamped to [0.089, 1]. Use it for the DFG lookup and the prefiltered
// environment's level, so they agree with the punctual lobe.
fn clamp_perceptual_roughness(perceptual_roughness: f32) -> f32 {
    return clamp(perceptual_roughness, MIN_PERCEPTUAL_ROUGHNESS, 1.0);
}

// The GGX α of an authored perceptual roughness: clamped perceptual².
fn alpha_from_perceptual_roughness(perceptual_roughness: f32) -> f32 {
    let clamped = clamp_perceptual_roughness(perceptual_roughness);
    return clamped * clamped;
}

// n·v kept above zero for the visibility term.
fn clamped_n_dot_v(n: vec3<f32>, v: vec3<f32>) -> f32 {
    return max(dot(n, v), MIN_N_DOT_V);
}

// F0 for the metallic workflow: 4% for dielectrics, the base colour for metals.
fn f0_from_metallic(base_color: vec3<f32>, metallic: f32) -> vec3<f32> {
    return mix(vec3<f32>(DIELECTRIC_F0), base_color, metallic);
}

// Diffuse albedo for the metallic workflow: metals have none.
fn diffuse_color_from_metallic(base_color: vec3<f32>, metallic: f32) -> vec3<f32> {
    return base_color * (1.0 - metallic);
}

// GGX (Trowbridge-Reitz) normal distribution, in 1/sr (Walter et al. 2007):
// α² / (π ((n·h)² (α² − 1) + 1)²). Normalised so ∫ D (n·h) dω = 1 over the hemisphere.
fn d_ggx(n_dot_h: f32, alpha: f32) -> f32 {
    let a2 = alpha * alpha;
    let f = (n_dot_h * a2 - n_dot_h) * n_dot_h + 1.0;
    return a2 / (PI * f * f);
}

// Height-correlated Smith visibility for GGX, exact form (Heitz 2014): the masking-shadowing G
// divided by 4 (n·v)(n·l), so the specular BRDF is D · V · F. Symmetric in n·v and n·l.
fn v_smith_ggx_correlated(n_dot_v: f32, n_dot_l: f32, alpha: f32) -> f32 {
    let a2 = alpha * alpha;
    let lambda_v = n_dot_l * sqrt((n_dot_v - a2 * n_dot_v) * n_dot_v + a2);
    let lambda_l = n_dot_v * sqrt((n_dot_l - a2 * n_dot_l) * n_dot_l + a2);
    return 0.5 / (lambda_v + lambda_l);
}

// Schlick's Fresnel: f0 + (f90 − f0)(1 − v·h)⁵.
fn f_schlick(f0: vec3<f32>, f90: f32, v_dot_h: f32) -> vec3<f32> {
    let x = 1.0 - v_dot_h;
    let x2 = x * x;
    return f0 + (vec3<f32>(f90) - f0) * (x2 * x2 * x);
}

// Lambert diffuse BRDF, in 1/sr: albedo/π.
fn lambert(diffuse_color: vec3<f32>) -> vec3<f32> {
    return diffuse_color / PI;
}

// The Cook-Torrance specular BRDF D · V · F in 1/sr, with f90 = 1.
fn specular_brdf(
    n_dot_v: f32,
    n_dot_l: f32,
    n_dot_h: f32,
    l_dot_h: f32,
    f0: vec3<f32>,
    alpha: f32,
) -> vec3<f32> {
    let d = d_ggx(n_dot_h, alpha);
    let visibility = v_smith_ggx_correlated(n_dot_v, n_dot_l, alpha);
    return d * visibility * f_schlick(f0, 1.0, l_dot_h);
}

// Diffuse luminance a punctual light adds: lambert(diffuse colour) · E · n·l. `illuminance` is
// the light's colour times its perpendicular illuminance in lux.
fn punctual_diffuse(
    n: vec3<f32>,
    l: vec3<f32>,
    illuminance: vec3<f32>,
    base_color: vec3<f32>,
    metallic: f32,
) -> vec3<f32> {
    let n_dot_l = saturate(dot(n, l));
    return lambert(diffuse_color_from_metallic(base_color, metallic)) * illuminance * n_dot_l;
}

// Specular luminance a punctual light adds: D · V · F · E · n·l. Multiply by energy_compensation
// when the DFG table is bound, as Filament does, so rough metals keep their energy.
fn punctual_specular(
    n: vec3<f32>,
    v: vec3<f32>,
    l: vec3<f32>,
    illuminance: vec3<f32>,
    base_color: vec3<f32>,
    metallic: f32,
    perceptual_roughness: f32,
) -> vec3<f32> {
    let n_dot_l = saturate(dot(n, l));
    if n_dot_l <= 0.0 {
        // Also keeps normalize(v + l) away from l = −v.
        return vec3<f32>(0.0);
    }
    let h = normalize(v + l);
    let lobe = specular_brdf(
        clamped_n_dot_v(n, v),
        n_dot_l,
        saturate(dot(n, h)),
        saturate(dot(l, h)),
        f0_from_metallic(base_color, metallic),
        alpha_from_perceptual_roughness(perceptual_roughness),
    );
    return lobe * illuminance * n_dot_l;
}

// A punctual light's diffuse and specular luminance, kept apart so callers can weight, occlude or
// compensate them separately.
struct PunctualLighting {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
}

fn punctual_light(
    n: vec3<f32>,
    v: vec3<f32>,
    l: vec3<f32>,
    illuminance: vec3<f32>,
    base_color: vec3<f32>,
    metallic: f32,
    perceptual_roughness: f32,
) -> PunctualLighting {
    return PunctualLighting(
        punctual_diffuse(n, l, illuminance, base_color, metallic),
        punctual_specular(n, v, l, illuminance, base_color, metallic, perceptual_roughness),
    );
}

// Reads the DFG table (R = ∫ Fc V n·l, G = ∫ V n·l) at (n·v, clamped perceptual roughness). Bind
// it with a linear, clamp-to-edge sampler.
fn sample_dfg(
    table: texture_2d<f32>,
    table_sampler: sampler,
    n_dot_v: f32,
    perceptual_roughness: f32,
) -> vec2<f32> {
    return textureSampleLevel(table, table_sampler, vec2<f32>(n_dot_v, perceptual_roughness), 0.0).rg;
}

// The split sum's directional albedo for F0, with f90 = 1: mix(dfg.r, dfg.g, F0).
fn specular_dfg(f0: vec3<f32>, dfg: vec2<f32>) -> vec3<f32> {
    return mix(vec3<f32>(dfg.x), vec3<f32>(dfg.y), f0);
}

// Filament's multiple-scattering energy compensation, 1 + F0 (1/dfg.g − 1): scales single-
// scattering specular back up to the energy a rough lobe loses between microfacets.
fn energy_compensation(f0: vec3<f32>, dfg: vec2<f32>) -> vec3<f32> {
    return 1.0 + f0 * (1.0 / dfg.y - 1.0);
}

// Split-sum specular from a prefiltered environment sample: prefiltered · specular_dfg · energy
// compensation.
fn environment_specular(prefiltered: vec3<f32>, f0: vec3<f32>, dfg: vec2<f32>) -> vec3<f32> {
    return prefiltered * specular_dfg(f0, dfg) * energy_compensation(f0, dfg);
}

// Mip level of a prefiltered cubemap for a perceptual roughness, when level k was filtered for
// roughness k / max_lod: max_lod · perceptual roughness.
fn lod_from_roughness(perceptual_roughness: f32, max_lod: f32) -> f32 {
    return max_lod * perceptual_roughness;
}
