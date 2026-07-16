// Milestone 4: full ray-tracing pipeline stages (ray generation, miss, any-hit, closest-hit)
// tracing into an acceleration structure and recording results through a storage buffer.
enable wgpu_ray_tracing_pipeline;

struct RayPayload {
    color: vec3<f32>,
    hit_count: u32,
}

@group(0) @binding(0) var scene_acceleration: acceleration_structure;
@group(0) @binding(1) var<storage, read_write> output_colors: array<vec4<f32>>;

var<ray_payload> payload: RayPayload;

@ray_generation
fn rg_main(
    @builtin(ray_invocation_id) id: vec3<u32>,
    @builtin(num_ray_invocations) count: vec3<u32>,
) {
    payload = RayPayload();
    let uv = vec2<f32>(f32(id.x) / f32(count.x), f32(id.y) / f32(count.y));
    let direction = normalize(vec3<f32>(uv * 2.0 - 1.0, 1.0));
    traceRay(
        scene_acceleration,
        RayDesc(RAY_FLAG_NONE, 0xFFu, 0.01, 100.0, vec3<f32>(0.0), direction),
        &payload,
    );
    output_colors[id.x + id.y * count.x] = vec4<f32>(payload.color, 1.0);
}

var<incoming_ray_payload> incoming: RayPayload;

@miss
@incoming_payload(incoming)
fn miss_main() {
    incoming.color = vec3<f32>(0.05, 0.05, 0.1);
}

@any_hit
@incoming_payload(incoming)
fn ah_main(@builtin(hit_kind) kind: u32) {
    incoming.hit_count += 1u;
}

@closest_hit
@incoming_payload(incoming)
fn ch_main(@builtin(ray_t_current_max) t: f32) {
    incoming.color = vec3<f32>(t / 100.0, 1.0, 0.0);
}
