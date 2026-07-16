#version 460

layout(location = 0) in vec3 color;
layout(location = 1) in vec2 uv;
layout(location = 0) out vec4 output_color;
layout(set = 0, binding = 0) uniform sampler2D checkerboard;
layout(set = 0, binding = 1) uniform FrameData {
    mat4 transform;
    vec4 tint_time;
} frame;
layout(set = 0, binding = 2) uniform sampler2D generated_texture;

void main() {
    float pulse = 0.9 + 0.1 * sin(frame.tint_time.w * 2.0);
    vec3 generated = texture(generated_texture, uv).rgb;
    vec3 generated_tint = vec3(0.55) + 0.45 * generated;
    output_color = vec4(color * texture(checkerboard, uv).rgb * generated_tint * pulse, 1.0);
}
