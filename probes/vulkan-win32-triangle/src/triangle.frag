#version 460

layout(location = 0) in vec3 color;
layout(location = 1) in vec2 uv;
layout(location = 0) out vec4 output_color;
layout(set = 0, binding = 0) uniform sampler2D checkerboard;

void main() {
    output_color = vec4(color * texture(checkerboard, uv).rgb, 1.0);
}
