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
    float4 shadow_position;
};

struct FrameUniforms {
    float4 offset;
};

struct ShadowRasterData {
    float4 position [[position]];
};

struct PostRasterData {
    float4 position [[position]];
    float2 texture_coordinate;
};

float4 animated_position(Vertex input_vertex, constant FrameUniforms& uniforms) {
    float4 position = input_vertex.position;
    position.x += uniforms.offset.x * (1.0 - position.z);
    return position;
}

float4 shadow_position(Vertex input_vertex, constant FrameUniforms& uniforms) {
    float4 position = animated_position(input_vertex, uniforms);
    position.xy += float2(0.18, -0.14) * (1.0 - position.z);
    return position;
}

vertex ShadowRasterData shadow_vertex(
    const device Vertex* vertices [[buffer(0)]],
    constant FrameUniforms& uniforms [[buffer(1)]],
    uint vertex_id [[vertex_id]]) {
    ShadowRasterData output;
    output.position = shadow_position(vertices[vertex_id], uniforms);
    return output;
}

vertex RasterData vertex_main(
    const device Vertex* vertices [[buffer(0)]],
    constant FrameUniforms& uniforms [[buffer(1)]],
    uint vertex_id [[vertex_id]]) {
    RasterData output;
    output.position = animated_position(vertices[vertex_id], uniforms);
    output.color = vertices[vertex_id].color;
    output.texture_coordinate = vertices[vertex_id].texture_coordinate.xy;
    output.shadow_position = shadow_position(vertices[vertex_id], uniforms);
    return output;
}

fragment float4 fragment_main(
    RasterData input [[stage_in]],
    texture2d<float> color_texture [[texture(0)]],
    depth2d<float> shadow_texture [[texture(1)]],
    sampler color_sampler [[sampler(0)]]) {
    float3 shadow_ndc = input.shadow_position.xyz / input.shadow_position.w;
    float2 shadow_uv = float2(shadow_ndc.x * 0.5 + 0.5, 0.5 - shadow_ndc.y * 0.5);
    bool inside = all(shadow_uv >= 0.0) && all(shadow_uv <= 1.0);
    constexpr sampler shadow_sampler(coord::normalized, address::clamp_to_edge, filter::linear);
    float stored_depth = shadow_texture.sample(shadow_sampler, shadow_uv);
    float visibility = inside && shadow_ndc.z > stored_depth + 0.012 ? 0.48 : 1.0;
    return color_texture.sample(color_sampler, input.texture_coordinate) * input.color * visibility;
}

vertex PostRasterData post_vertex(uint vertex_id [[vertex_id]]) {
    const float2 positions[] = {
        float2(-1.0, -1.0),
        float2(3.0, -1.0),
        float2(-1.0, 3.0),
    };
    PostRasterData output;
    output.position = float4(positions[vertex_id], 0.0, 1.0);
    output.texture_coordinate = float2(
        output.position.x * 0.5 + 0.5,
        0.5 - output.position.y * 0.5);
    return output;
}

fragment float4 post_fragment(
    PostRasterData input [[stage_in]],
    texture2d<float> scene_texture [[texture(0)]]) {
    constexpr sampler scene_sampler(coord::normalized, address::clamp_to_edge, filter::linear);
    float4 color = scene_texture.sample(scene_sampler, input.texture_coordinate);
    float2 centered = input.texture_coordinate * 2.0 - 1.0;
    float vignette = 1.0 - 0.18 * dot(centered, centered);
    color.rgb = pow(max(color.rgb * vignette, 0.0), float3(0.96));
    return color;
}

kernel void copy_texture(
    texture2d<float, access::sample> source [[texture(0)]],
    texture2d<float, access::write> destination [[texture(1)]],
    device uint* texel_words [[buffer(0)]],
    uint2 position [[thread_position_in_grid]]) {
    if (position.x >= destination.get_width() || position.y >= destination.get_height()) {
        return;
    }
    constexpr sampler source_sampler(coord::pixel, address::clamp_to_edge, filter::nearest);
    float4 value = source.sample(source_sampler, float2(position) + 0.5);
    destination.write(value, position);
    uchar4 bytes = uchar4(round(clamp(value, 0.0, 1.0) * 255.0));
    texel_words[position.y * destination.get_width() + position.x] =
        uint(bytes.x) |
        (uint(bytes.y) << 8) |
        (uint(bytes.z) << 16) |
        (uint(bytes.w) << 24);
}
