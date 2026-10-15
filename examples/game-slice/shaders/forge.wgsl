// Forge Run's one material: a textured, vertex-coloured shape drawn once per instance, each
// instance carrying its own model-view-projection matrix.

@group(0) @binding(1) var color_texture: texture_2d<f32>;
@group(0) @binding(2) var color_sampler: sampler;

struct Vertex {
    @location(0) position: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) uv: vec2<f32>,
}

// The instance's column-major model-view-projection matrix, a column per location.
struct Instance {
    @location(3) column_0: vec4<f32>,
    @location(4) column_1: vec4<f32>,
    @location(5) column_2: vec4<f32>,
    @location(6) column_3: vec4<f32>,
}

struct Raster {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>,
}

@vertex
fn forge_vertex(vertex: Vertex, instance: Instance) -> Raster {
    let model_view_projection = mat4x4<f32>(
        instance.column_0,
        instance.column_1,
        instance.column_2,
        instance.column_3,
    );
    var output: Raster;
    output.clip_position = model_view_projection * vec4<f32>(vertex.position, 1.0);
    output.color = vertex.color;
    output.uv = vertex.uv;
    return output;
}

@fragment
fn forge_fragment(input: Raster) -> @location(0) vec4<f32> {
    return textureSample(color_texture, color_sampler, input.uv) * vec4<f32>(input.color, 1.0);
}
