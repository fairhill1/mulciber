// One module, two vertex entry points sharing one fragment entry point. Only `skinned_vertex`
// reaches the bone palette, so a pipeline of `prop_vertex` declares no storage slot.
struct Draw {
    tint: vec4<f32>,
}
@group(0) @binding(0) var<uniform> draw: Draw;
@group(0) @binding(1) var<storage, read> bones: array<vec4<f32>, 4>;

struct Surface {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(flat) color: vec4<f32>,
}

// Reached from `skinned_vertex` through a call: the interface still attributes it.
fn blend(index: vec4<u32>, weight: vec4<f32>) -> vec4<f32> {
    return bones[index.x] * weight.x + bones[index.y] * weight.y + bones[index.z] * weight.z
        + bones[index.w] * weight.w;
}

@vertex fn prop_vertex(@location(0) position: vec2<f32>) -> Surface {
    return Surface(vec4<f32>(position, 0.5, 1.0), vec4<f32>(0.25, 0.5, 0.75, 1.0));
}

@vertex fn skinned_vertex(
    @location(0) position: vec2<f32>,
    @location(1) bone: vec4<u32>,
    @location(2) weight: vec4<f32>,
) -> Surface {
    return Surface(vec4<f32>(position, 0.5, 1.0), blend(bone, weight));
}

@fragment fn prop_fragment(surface: Surface) -> @location(0) vec4<f32> {
    return surface.color * draw.tint;
}
