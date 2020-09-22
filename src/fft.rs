use cpal::ChannelCount;
use dsp::window::Window;
use rustfft::num_complex::Complex;
use rustfft::num_traits::Zero;

pub type RealVec = Vec<f32>;

pub type FftSample = Complex<f32>;
pub type FftVec = Vec<FftSample>;
pub type FftSlice = [FftSample];

pub type FftCallback<'a> = &'a mut dyn FnMut(&FftSlice);

/// How to window the FFT to reduce sidelobes.
#[derive(Debug, Copy, Clone)]
pub enum WindowType {
    Rect,
    Hann,
}

/// size must be a power of 2, at least 2.
/// (It's probably nonsensical to use a size less than 32 or so.)
///
/// channels must be 1 or more.
#[derive(Debug, Copy, Clone)]
pub struct FftConfig {
    pub size: usize,
    pub channels: ChannelCount,
    pub window_type: WindowType,
    // TODO downmix: bool,
    // TODO add option to overlap by 50%.
    // TODO add option for whether to allow multiple calls in the same push.
}

/// Accepts data from the audio thread, buffers to full FFT blocks, and runs FFT.
pub struct FftBuffer {
    // User parameters. Do not mutate.
    cfg: FftConfig,

    // Derived/cached data. Do not mutate.
    fft: realfft::RealToComplex<f32>,
    window: Option<Window>,

    // Mutable state.
    buffer: RealVec,
    scratch: RealVec,
    spectrum: FftVec,
}

impl FftBuffer {
    pub fn new(cfg: FftConfig) -> FftBuffer {
        assert!(cfg.size >= 2);
        assert!(cfg.channels >= 1);

        let fft = realfft::RealToComplex::<f32>::new(cfg.size).unwrap();

        FftBuffer {
            cfg,

            // downmix,
            fft,
            window: match cfg.window_type {
                WindowType::Rect => None,
                WindowType::Hann => Some(dsp::window::hann(cfg.size, 0, cfg.size)),
            },

            // current: Vec::with_capacity(size),
            buffer: Vec::with_capacity(cfg.size),
            scratch: vec![Zero::zero(); cfg.size],
            spectrum: vec![Zero::zero(); cfg.size / 2 + 1],
        }
    }

    pub fn spectrum_size(&self) -> usize {
        self.spectrum.len()
    }

    /// input.len() must be a multiple of channels.
    /// Samples are assumed to be interleaved.
    ///
    /// fft_callback() is called on a (len/2 + 1) vector of complex values,
    /// where elements 0 and len/2 are purely real.
    pub fn push(&mut self, input: &[i16], fft_callback: FftCallback) {
        let frames = input.chunks_exact(self.cfg.channels as usize);
        for frame in frames {
            let avg = {
                let mut sum: f32 = 0.;
                for &sample in frame {
                    sum += (sample as f32) / 32768.0;
                }
                sum / (self.cfg.channels as f32)
            };
            self.buffer.push(avg);

            if self.buffer.len() == self.buffer.capacity() {
                (&mut *self).run_fft();
                fft_callback(&self.spectrum);
                self.buffer.clear();
            }
        }

        assert_eq!(self.buffer.capacity(), self.cfg.size);
    }

    /// Preconditions:
    /// - self.buffer.len() == self.cfg.size (via pushing).
    /// - self.scratch.len() == self.cfg.size (via initialization).
    ///
    /// Postconditions:
    /// - self.spectrum contains the windowed FFT of self.buffer.
    /// - self.buffer is unchanged.
    fn run_fft(&mut self) {
        if let Some(window) = &self.window {
            // Precondition: LHS, input, and output have same length.
            window.apply(&self.buffer, &mut self.scratch);
        } else {
            // Precondition: LHS and src have same length.
            (&mut self.scratch).copy_from_slice(&self.buffer);
        }

        self.fft
            .process(&mut self.scratch, &mut self.spectrum)
            .unwrap();
        for elem in self.spectrum.iter_mut() {
            *elem /= self.buffer.len() as f32;
        }
    }
}
