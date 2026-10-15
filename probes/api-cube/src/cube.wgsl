struct DrawConstants {
    model_view_projection: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> draw: DrawConstants;
@group(0) @binding(1) var color_texture: texture_2d<f32>;
@group(0) @binding(2) var color_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) uv: vec2<f32>,
}

struct RasterData {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>,
}

@vertex
fn cube_vertex(input: VertexInput) -> RasterData {
    var output: RasterData;
    output.clip_position = draw.model_view_projection * vec4<f32>(input.position, 1.0);
    output.color = input.color;
    output.uv = input.uv;
    return output;
}

@fragment
fn cube_fragment(input: RasterData) -> @location(0) vec4<f32> {
    return textureSample(color_texture, color_sampler, input.uv) * vec4<f32>(input.color, 1.0);
}
