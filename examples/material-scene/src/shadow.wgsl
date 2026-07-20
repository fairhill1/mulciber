struct ShadowParams {
    light_from_model: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> params: ShadowParams;

@vertex
fn shadow_vertex(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return params.light_from_model * vec4<f32>(position, 1.0);
}
