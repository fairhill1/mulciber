#version 460

layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 output_color;
layout(set = 0, binding = 0) uniform sampler2D scene_color;

void main() {
    vec3 color = texture(scene_color, uv).rgb;
    vec2 centered = uv * 2.0 - 1.0;
    float vignette = 1.0 - 0.12 * clamp(dot(centered, centered), 0.0, 1.0);
    output_color = vec4(color * vignette, 1.0);
}
