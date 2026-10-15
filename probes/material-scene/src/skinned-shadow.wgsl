// Depth-only skinned caster: the same bone palette as the strand's material record, applied
// under the light's view-projection. The entry consumes a subset of the strand's vertex layout,
// skipping the normal at location 1.

struct SkinnedShadowParams {
    light_view_projection: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> params: SkinnedShadowParams;
@group(0) @binding(1) var<storage, read> bones: array<mat4x4<f32>, 6>;

@vertex
fn skinned_shadow_vertex(
    @location(0) position: vec3<f32>,
    @location(2) joints: vec4<u32>,
    @location(3) weights: vec4<f32>,
) -> @builtin(position) vec4<f32> {
    let skin = bones[joints.x] * weights.x + bones[joints.y] * weights.y;
    return params.light_view_projection * (skin * vec4<f32>(position, 1.0));
}
