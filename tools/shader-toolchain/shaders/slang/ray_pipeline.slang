// Milestone 4: full ray-tracing pipeline stages (ray generation, miss, any-hit, closest-hit)
// tracing into an acceleration structure and recording results through a storage buffer.

RaytracingAccelerationStructure gSceneAcceleration;
RWStructuredBuffer<float4> gOutputColors;

struct RayPayload
{
    float3 color;
    uint hitCount;
};

[shader("raygeneration")]
void rgMain()
{
    uint3 id = DispatchRaysIndex();
    uint3 dims = DispatchRaysDimensions();
    float2 uv = float2(id.xy) / float2(dims.xy);

    RayDesc ray;
    ray.Origin = float3(0.0, 0.0, 0.0);
    ray.Direction = normalize(float3(uv * 2.0 - 1.0, 1.0));
    ray.TMin = 0.01;
    ray.TMax = 100.0;

    RayPayload payload;
    payload.color = float3(0.0, 0.0, 0.0);
    payload.hitCount = 0;
    TraceRay(gSceneAcceleration, RAY_FLAG_NONE, 0xFF, 0, 0, 0, ray, payload);
    gOutputColors[id.x + id.y * dims.x] = float4(payload.color, 1.0);
}

[shader("miss")]
void missMain(inout RayPayload payload)
{
    payload.color = float3(0.05, 0.05, 0.1);
}

[shader("anyhit")]
void ahMain(inout RayPayload payload, BuiltInTriangleIntersectionAttributes attributes)
{
    payload.hitCount += 1;
}

[shader("closesthit")]
void chMain(inout RayPayload payload, BuiltInTriangleIntersectionAttributes attributes)
{
    payload.color = float3(RayTCurrent() / 100.0, attributes.barycentrics);
}
