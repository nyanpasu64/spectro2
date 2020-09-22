#version 450

// Automatically interpolated between vertices.
// See https://www.khronos.org/opengl/wiki/Fragment_Shader .
layout(location=0) in vec2 v_position;

layout(location=0) out vec4 f_color;

// https://www.khronos.org/opengl/wiki/Layout_Qualifier_(GLSL)
layout(set=0, binding=0)
buffer Fft {
    vec2 spectrum[257];
};

const int MAX_FFT_SIZE = 257;

const float TWOPI = 6.28318530717958647693;

#define THROW throw(); return

void throw() {
    f_color = vec4(1, 0, 1, 1);
    return;
}

vec3 value(int freq, float xrel) {
    vec2 val = spectrum[freq] * 10.;

    float magnitude = sqrt(val.x * val.x + val.y * val.y);
    if (magnitude > 1) {
        return vec3(1, 0, 1);
    }
    float angle = atan(val.y, val.x);

    // Prevent division by 0.
    val /= (magnitude + 1e-9);

    float x_theta = TWOPI * xrel * freq;
    float unit = cos(angle + x_theta);
    unit = float(unit > 0);

    float value = unit * magnitude;
    return value.xxx;
}

void main() {
    f_color = vec4(0,0,0,0);

    float x = (v_position.x + 1) / 2;

    float y = (v_position.y + 1) / 2;
    y *= (MAX_FFT_SIZE - 1);
    int yint = int(y);
    float yexcess = y - yint;

    if (yint < 0 || yint + 1 >= MAX_FFT_SIZE) {
        THROW;
    }

    vec3 brightness = mix(value(yint, x), value(yint + 1, x), yexcess);
    f_color = vec4(brightness, 1.0);
}
