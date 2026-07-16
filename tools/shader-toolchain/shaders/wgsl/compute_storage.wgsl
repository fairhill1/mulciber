// Milestone 2: compute pipeline writing a runtime-sized storage buffer and a storage image,
// with workgroup-shared data and a barrier (mirrors the probe storage and storage-image passes).

@group(0) @binding(0) var<storage, read_write> counters: array<u32>;
@group(0) @binding(1) var output_image: texture_storage_2d<rgba8unorm, write>;

var<workgroup> tile_sums: array<u32, 64>;

@compute @workgroup_size(8, 8, 1)
fn cs_storage(
    @builtin(global_invocation_id) global_id: vec3<u32>,
    @builtin(local_invocation_index) local_index: u32,
) {
    tile_sums[local_index] = global_id.x + global_id.y * 8u;
    workgroupBarrier();
    let mirrored = tile_sums[63u - local_index];
    if local_index < arrayLength(&counters) {
        counters[local_index] = mirrored;
    }
    textureStore(
        output_image,
        vec2<u32>(global_id.xy),
        vec4<f32>(f32(mirrored) / 255.0, 0.0, 0.0, 1.0),
    );
}
