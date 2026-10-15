struct CrystalParams {
    model_view_projection: mat4x4<f32>,
    model: mat4x4<f32>,
    // x: seconds, y: pulse strength, zw: unused.
    pulse: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: CrystalParams;
@group(0) @binding(1) var base_texture: texture_2d<f32>;
@group(0) @binding(2) var glow_texture: texture_2d<f32>;
@group(0) @binding(3) var crystal_sampler: sampler;

struct CrystalVertex {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) glow: f32,
}

struct CrystalRaster {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) glow: f32,
}

@vertex
fn crystal_vertex(input: CrystalVertex) -> CrystalRaster {
    var output: CrystalRaster;
    output.clip_position = params.model_view_projection * vec4<f32>(input.position, 1.0);
    output.normal = (params.model * vec4<f32>(input.normal, 0.0)).xyz;
    output.uv = input.uv;
    output.glow = input.glow;
    return output;
}

@fragment
fn crystal_fragment(input: CrystalRaster) -> @location(0) vec4<f32> {
    let base = textureSample(base_texture, crystal_sampler, input.uv);
    let glow = textureSample(glow_texture, crystal_sampler, input.uv);
    let light = max(dot(normalize(input.normal), normalize(vec3<f32>(0.4, 0.8, 0.45))), 0.0);
    let pulse = 0.5 + 0.5 * sin(params.pulse.x * 3.0 + input.glow * 6.2832);
    let lit = base.rgb * (0.25 + 0.75 * light);
    let emissive = glow.rgb * pulse * params.pulse.y * input.glow;
    return vec4<f32>(lit + emissive, 1.0);
}
