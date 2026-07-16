// Milestone 4: amplification (task) + mesh shading pipeline emitting a single triangle with
// a task payload.

struct TaskPayload
{
    float4 colorScale;
};

groupshared TaskPayload gTaskPayload;

[shader("amplification")]
[numthreads(1, 1, 1)]
void tsMain()
{
    gTaskPayload.colorScale = float4(1.0, 0.8, 0.6, 1.0);
    DispatchMesh(1, 1, 1, gTaskPayload);
}

struct MeshVertex
{
    float4 position : SV_Position;
    float4 color : COLOR0;
};

static const float4 kTrianglePositions[3] = {
    float4(0.0, 0.5, 0.0, 1.0),
    float4(-0.5, -0.5, 0.0, 1.0),
    float4(0.5, -0.5, 0.0, 1.0),
};

[shader("mesh")]
[numthreads(1, 1, 1)]
[outputtopology("triangle")]
void msMain(
    in payload TaskPayload taskPayload,
    out vertices MeshVertex meshVertices[3],
    out indices uint3 meshIndices[1])
{
    SetMeshOutputCounts(3, 1);
    for (uint i = 0; i < 3; ++i)
    {
        meshVertices[i].position = kTrianglePositions[i];
        meshVertices[i].color = float4(1.0, 1.0, 1.0, 1.0) * taskPayload.colorScale;
    }
    meshIndices[0] = uint3(0, 1, 2);
}

[shader("fragment")]
float4 fsMesh(MeshVertex vertex) : SV_Target
{
    return vertex.color;
}
