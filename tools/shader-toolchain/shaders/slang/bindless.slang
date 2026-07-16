// Milestone 4: bindless resource tables through an unbounded texture array with uniform and
// non-uniform indexing (descriptor-indexing tier).

Texture2D gMaterialTextures[];
SamplerState gMaterialSamplers[4];

cbuffer MaterialConstants
{
    uint gMaterialIndex;
}

struct FragmentInput
{
    float2 uv : TEXCOORD0;
    nointerpolation uint instanceMaterial : TEXCOORD1;
};

[shader("fragment")]
float4 fsBindless(FragmentInput input) : SV_Target
{
    float4 uniformSample = gMaterialTextures[gMaterialIndex]
        .SampleLevel(gMaterialSamplers[gMaterialIndex % 4], input.uv, 0.0);
    float4 nonUniformSample = gMaterialTextures[NonUniformResourceIndex(input.instanceMaterial)]
        .SampleLevel(gMaterialSamplers[NonUniformResourceIndex(input.instanceMaterial % 4)], input.uv, 0.0);
    return uniformSample + nonUniformSample;
}
