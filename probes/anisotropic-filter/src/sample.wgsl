// Texture coordinates in `uv_dx.xy` and their x derivative in `uv_dx.zw`; the y derivative in
// `dy_filter.xy`, and in `dy_filter.z` the sampler: 0 trilinear, 1 anisotropic 4x, 2 anisotropic
// 16x. Every texel of level n holds n in red, so red reads back the level the sampler chose.
struct Request {
    uv_dx: vec4<f32>,
    dy_filter: vec4<f32>,
}
@group(0) @binding(0) var<uniform> request: Request;
@group(0) @binding(1) var levels: texture_2d<f32>;
@group(0) @binding(2) var trilinear: sampler;
@group(0) @binding(3) var anisotropic_4: sampler;
@group(0) @binding(4) var anisotropic_16: sampler;
@vertex fn sample_vertex(@location(0) position: vec2<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(position, 0.5, 1.0);
}
@fragment fn sample_fragment() -> @location(0) vec4<f32> {
    let uv = request.uv_dx.xy;
    let dx = request.uv_dx.zw;
    let dy = request.dy_filter.xy;
    if request.dy_filter.z == 1.0 {
        return textureSampleGrad(levels, anisotropic_4, uv, dx, dy);
    }
    if request.dy_filter.z == 2.0 {
        return textureSampleGrad(levels, anisotropic_16, uv, dx, dy);
    }
    return textureSampleGrad(levels, trilinear, uv, dx, dy);
}
