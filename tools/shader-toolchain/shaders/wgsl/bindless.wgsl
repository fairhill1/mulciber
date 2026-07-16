// Milestone 4: bindless resource tables through binding arrays with uniform and
// non-uniform indexing (descriptor-indexing tier).
enable wgpu_binding_array;

struct MaterialIndex {
    index: u32,
}

@group(0) @binding(0) var material_textures: binding_array<texture_2d<f32>>;
@group(0) @binding(1) var material_samplers: binding_array<sampler, 4>;
@group(0) @binding(2) var<uniform> material: MaterialIndex;

struct FragmentInput {
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) instance_material: u32,
}

@fragment
fn fs_bindless(input: FragmentInput) -> @location(0) vec4<f32> {
    let uniform_sample = textureSampleLevel(
        material_textures[material.index],
        material_samplers[material.index % 4u],
        input.uv,
        0.0,
    );
    let non_uniform_sample = textureSampleLevel(
        material_textures[input.instance_material],
        material_samplers[input.instance_material % 4u],
        input.uv,
        0.0,
    );
    return uniform_sample + non_uniform_sample;
}
