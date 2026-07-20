struct LavaParams {
    model_view_projection: mat4x4<f32>,
    // x: seconds, yzw: unused.
    flow: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: LavaParams;
@group(0) @binding(1) var lava_texture: texture_2d<f32>;
@group(0) @binding(2) var lava_sampler: sampler;

struct LavaVertex {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
}

struct LavaRaster {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn lava_vertex(input: LavaVertex) -> LavaRaster {
    var output: LavaRaster;
    output.clip_position = params.model_view_projection * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
    return output;
}

@fragment
fn lava_fragment(input: LavaRaster) -> @location(0) vec4<f32> {
    let seconds = params.flow.x;
    let wave = sin((input.uv.x + input.uv.y) * 9.0 + seconds * 1.7) * 0.035;
    let flowing = input.uv + vec2<f32>(seconds * 0.045 + wave, seconds * 0.09 + wave);
    let near = textureSample(lava_texture, lava_sampler, flowing);
    let far = textureSample(lava_texture, lava_sampler, flowing * 0.53 + vec2<f32>(0.31, 0.17));
    let churn = 0.5 + 0.5 * sin(seconds * 0.8 + (input.uv.x - input.uv.y) * 4.0);
    return vec4<f32>(mix(near.rgb, far.rgb, churn * 0.45), 1.0);
}
