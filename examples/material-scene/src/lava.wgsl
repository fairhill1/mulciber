struct LavaParams {
    model_view_projection: mat4x4<f32>,
    // x: seconds, yzw: unused.
    flow: vec4<f32>,
}

struct LavaCascades {
    // One light-from-model matrix per shadow cascade, near to far.
    light_from_model: array<mat4x4<f32>, 3>,
}

@group(0) @binding(0) var<uniform> params: LavaParams;
@group(0) @binding(1) var lava_texture: texture_2d<f32>;
@group(0) @binding(2) var lava_sampler: sampler;
@group(0) @binding(3) var shadow_map: texture_depth_2d_array;
@group(0) @binding(4) var shadow_sampler: sampler_comparison;
@group(0) @binding(5) var<storage, read> cascades: LavaCascades;

struct LavaVertex {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
}

struct LavaRaster {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) light_position_near: vec3<f32>,
    @location(2) light_position_mid: vec3<f32>,
    @location(3) light_position_far: vec3<f32>,
}

@vertex
fn lava_vertex(input: LavaVertex) -> LavaRaster {
    var output: LavaRaster;
    let position = vec4<f32>(input.position, 1.0);
    output.clip_position = params.model_view_projection * position;
    output.uv = input.uv;
    output.light_position_near = (cascades.light_from_model[0] * position).xyz;
    output.light_position_mid = (cascades.light_from_model[1] * position).xyz;
    output.light_position_far = (cascades.light_from_model[2] * position).xyz;
    return output;
}

fn shadow_uv(light_position: vec3<f32>) -> vec2<f32> {
    return light_position.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
}

fn inside_cascade(light_position: vec3<f32>) -> bool {
    let uv = shadow_uv(light_position);
    let margin = vec2<f32>(0.01, 0.01);
    return all(uv > margin) && all(uv < vec2<f32>(1.0, 1.0) - margin)
        && light_position.z > 0.0 && light_position.z < 1.0;
}

// The depth bias per cascade is application policy, applied to the comparison reference;
// coarser cascades cover more world per texel and need a slightly larger bias.
fn cascade_visibility(light_position: vec3<f32>, layer: i32, bias: f32) -> f32 {
    return textureSampleCompareLevel(
        shadow_map,
        shadow_sampler,
        shadow_uv(light_position),
        layer,
        light_position.z - bias,
    );
}

@fragment
fn lava_fragment(input: LavaRaster) -> @location(0) vec4<f32> {
    let seconds = params.flow.x;
    let wave = sin((input.uv.x + input.uv.y) * 9.0 + seconds * 1.7) * 0.035;
    let flowing = input.uv + vec2<f32>(seconds * 0.045 + wave, seconds * 0.09 + wave);
    let near = textureSample(lava_texture, lava_sampler, flowing);
    let far = textureSample(lava_texture, lava_sampler, flowing * 0.53 + vec2<f32>(0.31, 0.17));
    let churn = 0.5 + 0.5 * sin(seconds * 0.8 + (input.uv.x - input.uv.y) * 4.0);
    // Cascade selection is application policy: the tightest cascade whose light volume
    // contains the fragment wins, and anything beyond the last cascade stays lit.
    var lit = 1.0;
    if inside_cascade(input.light_position_near) {
        lit = cascade_visibility(input.light_position_near, 0, 0.002);
    } else if inside_cascade(input.light_position_mid) {
        lit = cascade_visibility(input.light_position_mid, 1, 0.003);
    } else if inside_cascade(input.light_position_far) {
        lit = cascade_visibility(input.light_position_far, 2, 0.004);
    }
    let color = mix(near.rgb, far.rgb, churn * 0.45) * mix(0.35, 1.0, lit);
    return vec4<f32>(color, 1.0);
}
