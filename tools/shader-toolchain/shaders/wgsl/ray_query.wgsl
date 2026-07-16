// Milestone 4: inline ray tracing (ray query) against an acceleration structure from compute.
enable wgpu_ray_query;

struct RayHit {
    hit: u32,
    t: f32,
}

@group(0) @binding(0) var scene_acceleration: acceleration_structure;
@group(0) @binding(1) var<storage, read_write> hits: array<RayHit>;

@compute @workgroup_size(64)
fn cs_ray_query(@builtin(global_invocation_id) global_id: vec3<u32>) {
    var query: ray_query;
    rayQueryInitialize(
        &query,
        scene_acceleration,
        RayDesc(
            RAY_FLAG_TERMINATE_ON_FIRST_HIT,
            0xFFu,
            0.01,
            100.0,
            vec3<f32>(0.0),
            vec3<f32>(0.0, 0.0, 1.0),
        ),
    );
    while rayQueryProceed(&query) {}
    let intersection = rayQueryGetCommittedIntersection(&query);
    if global_id.x < arrayLength(&hits) {
        hits[global_id.x] = RayHit(
            u32(intersection.kind == RAY_QUERY_INTERSECTION_TRIANGLE),
            intersection.t,
        );
    }
}
