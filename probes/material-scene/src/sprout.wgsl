struct SproutParams {
    view_projection: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> params: SproutParams;
@group(0) @binding(1) var leaf_texture: texture_2d<f32>;
@group(0) @binding(2) var leaf_sampler: sampler;

struct SproutVertex {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(4) model_x: vec4<f32>,
    @location(5) model_y: vec4<f32>,
    @location(6) model_z: vec4<f32>,
    @location(7) model_w: vec4<f32>,
}

struct SproutRaster {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn sprout_vertex(input: SproutVertex) -> SproutRaster {
    let model = mat4x4<f32>(input.model_x, input.model_y, input.model_z, input.model_w);
    var output: SproutRaster;
    output.clip_position = params.view_projection * model * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
    return output;
}

// The pipeline culls nothing, so each card's one face is drawn from both sides; the underside
// reads paler and yellower, as a leaf's does.
@fragment
fn sprout_fragment(input: SproutRaster, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let sample = textureSample(leaf_texture, leaf_sampler, input.uv);
    let underside = mix(sample.rgb, vec3<f32>(0.75, 0.8, 0.35), 0.55);
    let shaded = select(underside, sample.rgb, front) * (0.35 + 0.65 * input.uv.y);
    return vec4<f32>(shaded, sample.a);
}
