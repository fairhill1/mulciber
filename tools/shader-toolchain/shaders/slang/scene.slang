// Milestone 2 baseline: uniform-driven textured scene vertex/fragment pair mirroring the
// probe triangle scene (MVP transform, shader-visible elapsed time, sampled texture).

struct SceneUniforms
{
    float4x4 mvp;
    float timeSeconds;
};

ConstantBuffer<SceneUniforms> gScene;
Texture2D gSceneTexture;
SamplerState gSceneSampler;

struct VertexInput
{
    float3 position;
    float2 uv;
};

struct VertexOutput
{
    float4 position : SV_Position;
    float2 uv;
};

[shader("vertex")]
VertexOutput vsScene(VertexInput input)
{
    VertexOutput output;
    output.position = mul(gScene.mvp, float4(input.position, 1.0));
    output.uv = input.uv;
    return output;
}

[shader("fragment")]
float4 fsScene(VertexOutput input) : SV_Target
{
    float4 base = gSceneTexture.Sample(gSceneSampler, input.uv);
    float pulse = 0.5 + 0.5 * sin(gScene.timeSeconds);
    return float4(base.rgb * pulse, base.a);
}
