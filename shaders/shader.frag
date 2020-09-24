#version 450

// Automatically interpolated between vertices.
// See https://www.khronos.org/opengl/wiki/Fragment_Shader .
layout(location=0) in vec2 v_position;

layout(location=0) out vec4 f_color;

// https://www.khronos.org/opengl/wiki/Layout_Qualifier_(GLSL)
layout(set=0, binding=0)
uniform GpuFftLayout {
    uint screen_x_px;
    uint screen_y_px;
    uint sample_rate;
    uint fft_out_K;
};

layout(set=0, binding=1)
buffer Fft {
    vec2 spectrum[];
};

const float TWOPI = 6.28318530717958647693;

#define THROW f_color = vec4(1, 0, 1, 1); return

const float BACKGROUND = 0;
const bool RESCALE = true;

const float CENTER_ANGLE = 0;
const bool HIDE_SIDES = false;  // Hide side of window (where signal is out of phase).

const vec2 OFFSET = vec2(0.75, 0);

float unipolar(float bipolar) {
    return (bipolar + 1) / 2;
}

vec3 value(int k, float n_phase) {
    vec2 val = spectrum[k];

    float val_mag = length(val);
    if (HIDE_SIDES) {
        val_mag *= unipolar(cos(n_phase + TWOPI / 2));
    }

    float val_angle = atan(val.y, val.x);

    // Compute real component of DFT.
    float unit = cos(val_angle + k * n_phase);
    if (RESCALE) {
        unit = unipolar(unit);
    }

    float value = BACKGROUND + unit * val_mag;
    return value.xxx;
}

#define NYQUIST_HZ  (sample_rate / 2.)

float square(float x) {
    return x * x;
}

float sqrt_scale_k(float px_radius) {
    // unit: half-cycle
    const float FREQ_FLOOR = 20. / NYQUIST_HZ;
    // unit: half-cycle
    const float FREQ_REL = 6000. / NYQUIST_HZ;
    // unit: rel-screen
    const float RADIUS_REL = 0.8;

    return square(FREQ_FLOOR + px_radius / RADIUS_REL * FREQ_REL) * (fft_out_K - 1);
}

float lin_scale_k(float px_radius) {
    // unit: half-cycle
    const float FREQ_REL = 4000. / NYQUIST_HZ;
    // unit: rel-screen
    const float RADIUS_REL = 0.8;

    return px_radius / RADIUS_REL * FREQ_REL * (fft_out_K - 1);
}

float log_scale_k(float px_radius) {
    // unit: half-cycle
    const float FREQ_REL = 20. / NYQUIST_HZ;
    // unit: rel-screen
    const float RADIUS_REL = 0.;
    const float OCTAVES = 6;

    return pow(2, OCTAVES * (px_radius - RADIUS_REL)) * FREQ_REL * (fft_out_K - 1);
}

void main() {
    f_color = vec4(0, 0, 0, 1);

    // # Draw a circular spectrum analyzer,
    // where the -x axis is zero phase (edge of the window),
    // the +x axis is 2pi/2 phase (center of the window),
    // and distance from the center of the screen determines k.

    // unit: px
    uint screen_diameter_px = min(screen_x_px, screen_y_px);
    screen_diameter_px = max(screen_diameter_px, 1);

    uint screen_radius_px = screen_diameter_px / 2;
    screen_radius_px = max(screen_radius_px, 1);

    vec2 screen_px = vec2(screen_x_px, screen_y_px);

    // Between -1 and 1 (or slightly more, depending on aspect ratio).
    // unit: rel-screen
    vec2 position_rel = (v_position + OFFSET) * screen_px / screen_diameter_px;

    // time = n/N, between 0 and 2pi.
    // CENTER_ANGLE should map to TWOPI/2 (center of window)
    float n_phase = atan(position_rel.y, position_rel.x) + TWOPI / 2 - CENTER_ANGLE;

    // FFT bin.
    float k_float = lin_scale_k(length(position_rel));
    int k = int(k_float);
    float k_frac = k_float - k;

    if (k < 0 || k + 1 >= fft_out_K) {
        // Out of bounds. May happen if screen is very tall or wide, and circle is small.
        return;
    }

    vec3 brightness = mix(value(k, n_phase), value(k + 1, n_phase), k_frac);
    f_color = vec4(brightness, 1.0);
}
