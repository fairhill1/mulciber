// uv, explicit LOD, and stage selector (zero = vertex; one = fragment).
@group(0) @binding(0) var<uniform> request: vec4<f32>;
@group(0) @binding(1) var coefficients: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;
struct Raster {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) vertex_sample: vec4<f32>,
}
@vertex fn sample_vertex(@location(0) position: vec2<f32>) -> Raster {
    var out: Raster;
    out.position = vec4<f32>(position, 0.5, 1.0);
    out.vertex_sample = textureSampleLevel(coefficients, linear_sampler, request.xy, request.z);
    return out;
}
@fragment fn sample_fragment(in: Raster) -> @location(0) vec4<f32> {
    let fragment_sample = textureSampleLevel(coefficients, linear_sampler, request.xy, request.z);
    let sampled = select(in.vertex_sample, fragment_sample, request.w > 0.5);
    // Exact power-of-two amplification avoids subnormal render-target storage hiding sampling errors.
    return sampled * vec4<f32>(1.0, 1.0, 1024.0, 1024.0);
}
