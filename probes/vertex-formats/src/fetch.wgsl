// x selects the attribute the vertex stage forwards; the rest is unused.
@group(0) @binding(0) var<uniform> request: vec4<f32>;
struct Packed {
    @location(0) position: vec2<f32>,
    @location(1) bones: vec4<u32>,
    @location(2) weights: vec4<f32>,
    @location(3) pair: vec2<u32>,
    @location(4) quad: vec4<u32>,
    @location(5) unit_pair: vec2<f32>,
    @location(6) unit_quad: vec4<f32>,
}
struct Raster {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) fetched: vec4<f32>,
}
@vertex fn fetch_vertex(in: Packed) -> Raster {
    var out: Raster;
    out.position = vec4<f32>(in.position, 0.5, 1.0);
    let selector = u32(request.x);
    var fetched = vec4<f32>(in.weights);
    switch selector {
        case 0u: { fetched = vec4<f32>(in.bones); }
        case 1u: { fetched = in.weights; }
        case 2u: { fetched = vec4<f32>(vec2<f32>(in.pair), 0.0, 0.0); }
        case 3u: { fetched = vec4<f32>(in.quad); }
        case 4u: { fetched = vec4<f32>(in.unit_pair, 0.0, 0.0); }
        default: { fetched = in.unit_quad; }
    }
    out.fetched = fetched;
    return out;
}
@fragment fn fetch_fragment(in: Raster) -> @location(0) vec4<f32> {
    return in.fetched;
}
