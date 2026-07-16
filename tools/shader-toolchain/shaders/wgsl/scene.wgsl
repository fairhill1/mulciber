// Milestone 2 baseline: uniform-driven textured scene vertex/fragment pair mirroring the
// probe triangle scene (MVP transform, shader-visible elapsed time, sampled texture).

struct SceneUniforms {
    mvp: mat4x4<f32>,
    time_seconds: f32,
}

@group(0) @binding(0) var<uniform> scene: SceneUniforms;
@group(0) @binding(1) var scene_texture: texture_2d<f32>;
@group(0) @binding(2) var scene_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_scene(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = scene.mvp * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
    return output;
}

@fragment
fn fs_scene(input: VertexOutput) -> @location(0) vec4<f32> {
    let base = textureSample(scene_texture, scene_sampler, input.uv);
    let pulse = 0.5 + 0.5 * sin(scene.time_seconds);
    return vec4<f32>(base.rgb * pulse, base.a);
}
