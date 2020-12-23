use rustfft::num_complex::Complex;
use rustfft::num_traits::Zero;

pub type RealVec = Vec<f32>;

pub type FftSample = Complex<f32>;
pub type FftVec = Vec<FftSample>;
pub type FftSlice = [FftSample];

/// The data to be rendered in one frame.
pub struct SpectrumFrame {
    pub spectrum: FftVec,
    pub prev_spectrum: FftVec,
}

impl SpectrumFrame {
    pub fn new(spectrum_size: usize) -> SpectrumFrame {
        SpectrumFrame {
            spectrum: vec![FftSample::zero(); spectrum_size],
            prev_spectrum: vec![FftSample::zero(); spectrum_size],
        }
    }
}

pub struct SpectrumFrameRef<'a> {
    pub spectrum: &'a FftSlice,
    pub prev_spectrum: &'a FftSlice,
}
