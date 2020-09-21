#version 450

// left, right = x=(-1, 1)
// bottom, top = y=(-1, 1)
const vec2 positions[] = vec2[](
    vec2(-1, -1),
    vec2(4, -1),
    vec2(-1, 4)
);

layout(location=0) out vec2 v_position;

void main() {
    v_position = positions[gl_VertexIndex];
    gl_Position = vec4(v_position, 0.0, 1.0);
}
