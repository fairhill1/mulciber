// Sampling direction in xyz (not normalized) and explicit LOD in w.
@group(0) @binding(0) var<uniform> request: vec4<f32>;
@group(0) @binding(1) var environment: texture_cube<f32>;
@group(0) @binding(2) var nearest: sampler;
@vertex fn sample_vertex(@location(0) position: vec2<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(position, 0.5, 1.0);
}
@fragment fn sample_fragment() -> @location(0) vec4<f32> {
    return textureSampleLevel(environment, nearest, request.xyz, request.w);
}
