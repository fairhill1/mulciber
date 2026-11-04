// Texture coordinates in xy and explicit LOD in w; the layer in `selection.x`. A nonzero
// `selection.y` returns the texture's level and layer counts instead of a sample.
struct Request {
    uv_lod: vec4<f32>,
    selection: vec4<i32>,
}
@group(0) @binding(0) var<uniform> request: Request;
@group(0) @binding(1) var layers: texture_2d_array<f32>;
@group(0) @binding(2) var nearest: sampler;
@vertex fn sample_vertex(@location(0) position: vec2<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(position, 0.5, 1.0);
}
@fragment fn sample_fragment() -> @location(0) vec4<f32> {
    if request.selection.y != 0 {
        return vec4<f32>(f32(textureNumLevels(layers)), f32(textureNumLayers(layers)), 0.0, 1.0);
    }
    return textureSampleLevel(
        layers,
        nearest,
        request.uv_lod.xy,
        request.selection.x,
        request.uv_lod.w,
    );
}
