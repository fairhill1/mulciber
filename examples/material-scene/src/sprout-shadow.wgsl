struct SproutShadowParams {
    light_view_projection: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> params: SproutShadowParams;
@group(0) @binding(1) var leaf_texture: texture_2d<f32>;
@group(0) @binding(2) var leaf_sampler: sampler;

struct SproutShadowVertex {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(4) model_x: vec4<f32>,
    @location(5) model_y: vec4<f32>,
    @location(6) model_z: vec4<f32>,
    @location(7) model_w: vec4<f32>,
}

struct SproutShadowRaster {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn sprout_shadow_vertex(input: SproutShadowVertex) -> SproutShadowRaster {
    let model = mat4x4<f32>(input.model_x, input.model_y, input.model_z, input.model_w);
    var output: SproutShadowRaster;
    output.clip_position = params.light_view_projection * model * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
    return output;
}

@fragment
fn sprout_shadow_fragment(input: SproutShadowRaster) {
    let coverage = textureSample(leaf_texture, leaf_sampler, input.uv).a;
    if coverage < 0.5 {
        discard;
    }
}
