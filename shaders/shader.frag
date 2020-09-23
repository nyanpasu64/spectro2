#version 450

// Automatically interpolated between vertices.
// See https://www.khronos.org/opengl/wiki/Fragment_Shader .
layout(location=0) in vec2 v_position;

layout(location=0) out vec4 f_color;

// https://www.khronos.org/opengl/wiki/Layout_Qualifier_(GLSL)
layout(set=0, binding=0)
uniform GpuFftLayout {
    uint screen_wx;
    uint screen_hy;
    uint fft_out_size;
};

layout(set=0, binding=1)
buffer Fft {
    vec2 spectrum[];
};

const float TWOPI = 6.28318530717958647693;

#define THROW throw(); return

void throw() {
    f_color = vec4(1, 0, 1, 1);
    return;
}

vec3 value(int k, float n_phase) {
    vec2 val = spectrum[k] * 10.;

    float val_mag = length(val);
    if (val_mag > 1) {
        // loud inputs. should this branch be removed?
        return vec3(1, 0, 1);
    }
    float val_angle = atan(val.y, val.x);

    // Compute real component of DFT.
    float unit = cos(val_angle + k * n_phase);

    // Convert to [0, 1] (in this case, hard threshold).
    unit = float(unit > 0);

    float value = unit * val_mag;
    return value.xxx;
}

void main() {
    f_color = vec4(0,0,0,0);

    // Between 0 and 1.
    float x = float(gl_FragCoord.x) / screen_wx;

    // Between 0 and 1.
    // y increases up. gl_FragCoord.y increases down.
    float y = 1 - (float(gl_FragCoord.y) / screen_hy);

    // time = n/N, between 0 and 2pi.
    float n_phase = x * TWOPI;

    // FFT bin.
    float k_float = y * (fft_out_size - 1) * (8000. / 24000.);
    int k = int(k_float);
    float k_frac = k_float - k;

    if (k < 0 || k + 1 >= fft_out_size) {
        // should never happen if we calculated k correctly.
        THROW;
    }

    vec3 brightness = mix(value(k, n_phase), value(k + 1, n_phase), k_frac);
    f_color = vec4(brightness, 1.0);
}
