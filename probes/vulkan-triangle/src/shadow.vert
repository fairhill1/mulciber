#version 460

layout(location = 0) in vec2 position;

void main() {
    vec2 light_space_position = position * 0.72 + vec2(0.10, -0.08);
    gl_Position = vec4(light_space_position, 0.25, 1.0);
}
