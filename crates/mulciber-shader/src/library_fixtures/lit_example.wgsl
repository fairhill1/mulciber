#import mulciber::photometry
#import mulciber::pbr
#import mulciber::tonemap

struct Frame {
    camera_position: vec3<f32>,
    // photometry::exposure_from_ev100(ev100), computed on the CPU.
    exposure: f32,
    light_position: vec3<f32>,
    // Candelas: photometry::point_light_intensity(lumens), computed on the CPU.
    light_intensity: f32,
    light_color: vec3<f32>,
    light_range: f32,
    environment_max_lod: f32,
}

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var albedo_map: texture_2d<f32>;
@group(0) @binding(2) var material_sampler: sampler;
@group(0) @binding(3) var dfg_table: texture_2d<f32>;
@group(0) @binding(4) var dfg_sampler: sampler;
@group(0) @binding(5) var environment: texture_cube<f32>;

struct Surface {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
}

@fragment fn lit_fragment(surface: Surface) -> @location(0) vec4<f32> {
    let base_color = textureSample(albedo_map, material_sampler, surface.uv).rgb;
    let metallic = 0.0;
    let roughness = mulciber::pbr::clamp_perceptual_roughness(0.5);

    let n = normalize(surface.normal);
    let v = normalize(frame.camera_position - surface.world_position);
    let to_light = frame.light_position - surface.world_position;
    let l = normalize(to_light);

    // Lux at the surface, pre-exposed so values stay small in a 16-bit target.
    let illuminance = frame.light_color * mulciber::photometry::punctual_illuminance(
        frame.light_intensity * frame.exposure,
        dot(to_light, to_light),
        frame.light_range,
    );

    let n_dot_v = mulciber::pbr::clamped_n_dot_v(n, v);
    let dfg = mulciber::pbr::sample_dfg(dfg_table, dfg_sampler, n_dot_v, roughness);
    let f0 = mulciber::pbr::f0_from_metallic(base_color, metallic);
    let lit = mulciber::pbr::punctual_light(n, v, l, illuminance, base_color, metallic, roughness);
    var luminance = lit.diffuse + lit.specular * mulciber::pbr::energy_compensation(f0, dfg);

    // The environment is stored in nits; expose it like the light.
    let lod = mulciber::pbr::lod_from_roughness(roughness, frame.environment_max_lod);
    let prefiltered = textureSampleLevel(environment, material_sampler, reflect(-v, n), lod).rgb;
    luminance += mulciber::pbr::environment_specular(prefiltered * frame.exposure, f0, dfg);

    return vec4<f32>(mulciber::tonemap::hue_preserving_shoulder(luminance), 1.0);
}
