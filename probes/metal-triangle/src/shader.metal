#include <metal_stdlib>
using namespace metal;

struct Vertex {
    float4 position;
    float4 color;
};

struct RasterData {
    float4 position [[position]];
    float4 color;
};

vertex RasterData vertex_main(const device Vertex* vertices [[buffer(0)]], uint vertex_id [[vertex_id]]) {
    RasterData output;
    output.position = vertices[vertex_id].position;
    output.color = vertices[vertex_id].color;
    return output;
}

fragment float4 fragment_main(RasterData input [[stage_in]]) {
    return input.color;
}
