// Milestone 2: compute pipeline writing a runtime-sized storage buffer and a storage image,
// with workgroup-shared data and a barrier (mirrors the probe storage and storage-image passes).

RWStructuredBuffer<uint> gCounters;
[format("rgba8")]
RWTexture2D<float4> gOutputImage;

groupshared uint gTileSums[64];

[shader("compute")]
[numthreads(8, 8, 1)]
void csStorage(uint3 globalId : SV_DispatchThreadID, uint localIndex : SV_GroupIndex)
{
    gTileSums[localIndex] = globalId.x + globalId.y * 8;
    GroupMemoryBarrierWithGroupSync();
    uint mirrored = gTileSums[63 - localIndex];
    uint count;
    uint stride;
    gCounters.GetDimensions(count, stride);
    if (localIndex < count)
    {
        gCounters[localIndex] = mirrored;
    }
    gOutputImage[globalId.xy] = float4(float(mirrored) / 255.0, 0.0, 0.0, 1.0);
}
