#include <metal_stdlib>
using namespace metal;

struct Vertex {
    float4 position;
    float4 color;
    float4 texture_coordinate;
};

struct RasterData {
    float4 position [[position]];
    float4 color;
    float2 texture_coordinate;
};

vertex RasterData vertex_main(const device Vertex* vertices [[buffer(0)]], uint vertex_id [[vertex_id]]) {
    RasterData output;
    output.position = vertices[vertex_id].position;
    output.color = vertices[vertex_id].color;
    output.texture_coordinate = vertices[vertex_id].texture_coordinate.xy;
    return output;
}

fragment float4 fragment_main(
    RasterData input [[stage_in]],
    texture2d<float> color_texture [[texture(0)]],
    sampler color_sampler [[sampler(0)]]) {
    return color_texture.sample(color_sampler, input.texture_coordinate) * input.color;
}

kernel void copy_texture(
    texture2d<float, access::read> source [[texture(0)]],
    texture2d<float, access::write> destination [[texture(1)]],
    uint2 position [[thread_position_in_grid]]) {
    if (position.x >= destination.get_width() || position.y >= destination.get_height()) {
        return;
    }
    destination.write(source.read(position), position);
}
