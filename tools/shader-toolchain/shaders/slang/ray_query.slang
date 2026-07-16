// Milestone 4: inline ray tracing (ray query) against an acceleration structure from compute.

RaytracingAccelerationStructure gSceneAcceleration;

struct RayHit
{
    uint hit;
    float t;
};

RWStructuredBuffer<RayHit> gHits;

[shader("compute")]
[numthreads(64, 1, 1)]
void csRayQuery(uint3 globalId : SV_DispatchThreadID)
{
    RayDesc ray;
    ray.Origin = float3(0.0, 0.0, 0.0);
    ray.Direction = float3(0.0, 0.0, 1.0);
    ray.TMin = 0.01;
    ray.TMax = 100.0;

    RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH> query;
    query.TraceRayInline(gSceneAcceleration, RAY_FLAG_NONE, 0xFF, ray);
    while (query.Proceed()) {}

    uint count;
    uint stride;
    gHits.GetDimensions(count, stride);
    if (globalId.x < count)
    {
        RayHit result;
        result.hit = query.CommittedStatus() == COMMITTED_TRIANGLE_HIT ? 1 : 0;
        result.t = query.CommittedRayT();
        gHits[globalId.x] = result;
    }
}
