// The skinned kelp strand: every vertex blends two bone matrices from a read-only storage
// palette the application repacks each frame. The bones already place the strand in the world,
// so the uniform carries only the camera's view-projection.

struct SkinnedParams {
    view_projection: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> params: SkinnedParams;
@group(0) @binding(1) var<storage, read> bones: array<mat4x4<f32>, 6>;

struct SkinnedRaster {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) height: f32,
}

@vertex
fn skinned_vertex(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) joints: vec4<u32>,
    @location(3) weights: vec4<f32>,
) -> SkinnedRaster {
    let skin = bones[joints.x] * weights.x + bones[joints.y] * weights.y;
    var raster: SkinnedRaster;
    raster.position = params.view_projection * (skin * vec4<f32>(position, 1.0));
    raster.normal = normalize((skin * vec4<f32>(normal, 0.0)).xyz);
    raster.height = position.y;
    return raster;
}

@fragment
fn skinned_fragment(raster: SkinnedRaster) -> @location(0) vec4<f32> {
    let sun = normalize(vec3<f32>(4.0, 7.0, 3.0));
    let light = max(dot(normalize(raster.normal), sun), 0.0);
    let base = mix(
        vec3<f32>(0.04, 0.22, 0.12),
        vec3<f32>(0.22, 0.72, 0.38),
        clamp(raster.height / 4.5, 0.0, 1.0),
    );
    return vec4<f32>(base * (0.3 + 0.7 * light), 1.0);
}
