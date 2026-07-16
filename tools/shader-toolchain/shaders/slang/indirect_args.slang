// Milestone 2: GPU-written indexed-indirect draw arguments (mirrors the probe compute pass
// feeding vkCmdDrawIndexedIndirect).

struct DrawIndexedIndirectArgs
{
    uint indexCount;
    uint instanceCount;
    uint firstIndex;
    int baseVertex;
    uint firstInstance;
};

RWStructuredBuffer<DrawIndexedIndirectArgs> gDrawArgs;

[shader("compute")]
[numthreads(1, 1, 1)]
void csWriteIndirect()
{
    DrawIndexedIndirectArgs args;
    args.indexCount = 6;
    args.instanceCount = 1;
    args.firstIndex = 0;
    args.baseVertex = 0;
    args.firstInstance = 0;
    gDrawArgs[0] = args;
}
