@group(0) @binding(1) var color_texture: texture_2d<f32>;
@group(0) @binding(2) var color_sampler: sampler;

struct InstancedVertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) model_view_projection_0: vec4<f32>,
    @location(4) model_view_projection_1: vec4<f32>,
    @location(5) model_view_projection_2: vec4<f32>,
    @location(6) model_view_projection_3: vec4<f32>,
}

struct RasterData {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>,
}

@vertex
fn instanced_vertex(input: InstancedVertexInput) -> RasterData {
    let model_view_projection = mat4x4<f32>(
        input.model_view_projection_0,
        input.model_view_projection_1,
        input.model_view_projection_2,
        input.model_view_projection_3,
    );
    var output: RasterData;
    output.clip_position = model_view_projection * vec4<f32>(input.position, 1.0);
    output.color = input.color;
    output.uv = input.uv;
    return output;
}

@fragment
fn cube_fragment(input: RasterData) -> @location(0) vec4<f32> {
    return textureSample(color_texture, color_sampler, input.uv) * vec4<f32>(input.color, 1.0);
}

struct PostRasterData {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn post_vertex(@builtin(vertex_index) index: u32) -> PostRasterData {
    let positions = array(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let position = positions[index];
    var output: PostRasterData;
    output.clip_position = vec4<f32>(position, 0.0, 1.0);
    output.uv = vec2<f32>(position.x * 0.5 + 0.5, 0.5 - position.y * 0.5);
    return output;
}

@fragment
fn post_fragment(input: PostRasterData) -> @location(0) vec4<f32> {
    let source = textureSample(color_texture, color_sampler, input.uv);
    let luminance = dot(source.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let graded = mix(source.rgb, vec3<f32>(luminance), 0.35) * vec3<f32>(1.08, 0.98, 1.12);
    let centered = input.uv * 2.0 - vec2<f32>(1.0);
    let vignette = 1.0 - 0.22 * dot(centered, centered);
    return vec4<f32>(graded * vignette, source.a);
}
