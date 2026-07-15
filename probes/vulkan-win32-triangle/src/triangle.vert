#version 460

layout(location = 0) out vec3 color;

const vec2 POSITIONS[3] = vec2[](
    vec2( 0.00, -0.65),
    vec2(-0.62,  0.45),
    vec2( 0.62,  0.45)
);

const vec3 COLORS[3] = vec3[](
    vec3(1.00, 0.20, 0.15),
    vec3(0.15, 0.85, 0.35),
    vec3(0.20, 0.40, 1.00)
);

void main() {
    gl_Position = vec4(POSITIONS[gl_VertexIndex], 0.0, 1.0);
    color = COLORS[gl_VertexIndex];
}
