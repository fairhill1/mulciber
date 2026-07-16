#version 460

layout(location = 0) in vec2 position;
layout(location = 1) in vec3 vertex_color;
layout(location = 2) in vec2 vertex_uv;
layout(location = 0) out vec3 color;
layout(location = 1) out vec2 uv;
layout(set = 0, binding = 1) uniform FrameData {
    mat4 transform;
    vec4 tint_time;
} frame;

void main() {
    gl_Position = frame.transform * vec4(position, 0.0, 1.0);
    color = vertex_color;
    uv = vertex_uv;
}
