// Milestone 2: GPU-written indexed-indirect draw arguments (mirrors the probe compute pass
// feeding vkCmdDrawIndexedIndirect).

struct DrawIndexedIndirectArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}

@group(0) @binding(0) var<storage, read_write> draw_args: DrawIndexedIndirectArgs;

@compute @workgroup_size(1)
fn cs_write_indirect() {
    draw_args = DrawIndexedIndirectArgs(6u, 1u, 0u, 0, 0u);
}
