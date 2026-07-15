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

struct FrameUniforms {
    float4 offset;
};

vertex RasterData vertex_main(
    const device Vertex* vertices [[buffer(0)]],
    constant FrameUniforms& uniforms [[buffer(1)]],
    uint vertex_id [[vertex_id]]) {
    RasterData output;
    output.position = vertices[vertex_id].position;
    output.position.x += uniforms.offset.x * (1.0 - output.position.z);
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
    device uint* texel_words [[buffer(0)]],
    uint2 position [[thread_position_in_grid]]) {
    if (position.x >= destination.get_width() || position.y >= destination.get_height()) {
        return;
    }
    float4 value = source.read(position);
    destination.write(value, position);
    uchar4 bytes = uchar4(round(clamp(value, 0.0, 1.0) * 255.0));
    texel_words[position.y * destination.get_width() + position.x] =
        uint(bytes.x) |
        (uint(bytes.y) << 8) |
        (uint(bytes.z) << 16) |
        (uint(bytes.w) << 24);
}
