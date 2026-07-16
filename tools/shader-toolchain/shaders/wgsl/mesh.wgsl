// Milestone 4: task + mesh shading pipeline emitting a single triangle with a task payload.
enable wgpu_mesh_shader;

struct TaskPayload {
    color_scale: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

struct PrimitiveOutput {
    @builtin(triangle_indices) indices: vec3<u32>,
    @builtin(cull_primitive) cull: bool,
}

struct MeshOutput {
    @builtin(vertices) vertices: array<VertexOutput, 3>,
    @builtin(primitives) primitives: array<PrimitiveOutput, 1>,
    @builtin(vertex_count) vertex_count: u32,
    @builtin(primitive_count) primitive_count: u32,
}

var<task_payload> mesh_task_payload: TaskPayload;
var<workgroup> mesh_output: MeshOutput;

const triangle_positions = array(
    vec4(0.0, 0.5, 0.0, 1.0),
    vec4(-0.5, -0.5, 0.0, 1.0),
    vec4(0.5, -0.5, 0.0, 1.0),
);

@task
@payload(mesh_task_payload)
@workgroup_size(1)
fn ts_main() -> @builtin(mesh_task_size) vec3<u32> {
    mesh_task_payload.color_scale = vec4(1.0, 0.8, 0.6, 1.0);
    return vec3(1u, 1u, 1u);
}

@mesh(mesh_output)
@payload(mesh_task_payload)
@workgroup_size(1)
fn ms_main() {
    mesh_output.vertex_count = 3u;
    mesh_output.primitive_count = 1u;
    for (var i = 0u; i < 3u; i += 1u) {
        mesh_output.vertices[i].position = triangle_positions[i];
        mesh_output.vertices[i].color = vec4(1.0) * mesh_task_payload.color_scale;
    }
    mesh_output.primitives[0].indices = vec3<u32>(0u, 1u, 2u);
    mesh_output.primitives[0].cull = false;
}

@fragment
fn fs_mesh(vertex: VertexOutput) -> @location(0) vec4<f32> {
    return vertex.color;
}
