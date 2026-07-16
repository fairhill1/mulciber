#version 460

layout(location = 0) in vec3 color;
layout(location = 1) in vec2 uv;
layout(location = 2) in vec4 shadow_position;
layout(location = 0) out vec4 output_color;
layout(set = 0, binding = 0) uniform sampler2D checkerboard;
layout(set = 0, binding = 1) uniform FrameData {
    mat4 transform;
    vec4 tint_time;
} frame;
layout(set = 0, binding = 2) uniform sampler2D generated_texture;
layout(set = 0, binding = 3) uniform sampler2D shadow_map;

float shadow_visibility() {
    vec3 projected = shadow_position.xyz / shadow_position.w;
    vec2 coordinates = projected.xy * 0.5 + 0.5;
    if (any(lessThan(coordinates, vec2(0.0))) || any(greaterThan(coordinates, vec2(1.0)))) {
        return 1.0;
    }

    vec2 texel = 1.0 / vec2(textureSize(shadow_map, 0));
    float lit_samples = 0.0;
    for (int y = -1; y <= 1; ++y) {
        for (int x = -1; x <= 1; ++x) {
            float stored_depth = texture(shadow_map, coordinates + vec2(x, y) * texel).r;
            lit_samples += projected.z - 0.01 <= stored_depth ? 1.0 : 0.0;
        }
    }
    return mix(0.55, 1.0, lit_samples / 9.0);
}

void main() {
    float pulse = 0.9 + 0.1 * sin(frame.tint_time.w * 2.0);
    vec3 generated = textureLod(generated_texture, uv, 1.0).rgb;
    vec3 generated_tint = vec3(0.55) + 0.45 * generated;
    output_color = vec4(
        color * texture(checkerboard, uv).rgb * generated_tint * pulse * shadow_visibility(),
        1.0
    );
}
