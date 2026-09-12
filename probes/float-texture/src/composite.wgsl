@group(0) @binding(1) var scene: texture_2d<f32>;
@group(0) @binding(2) var scene_sampler: sampler;
struct Raster { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn post_vertex(@builtin(vertex_index) index: u32) -> Raster {
    var positions = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var out: Raster;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = positions[index] * 0.5 + 0.5;
    return out;
}
@fragment fn post_fragment(in: Raster) -> @location(0) vec4<f32> {
    return vec4<f32>(clamp(textureSampleLevel(scene, scene_sampler, in.uv, 0.0).rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
