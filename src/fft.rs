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
#[allow(dead_code)]
#[derive(Debug, Copy, Clone)]
pub enum WindowType {
    Rect,
    Hann,
}

/// Normalization note: The FFT's output is divided by `size`
/// so a pure DC input will result in an output of `volume`.
/// As `size` increases, pure tones become thinner but not brighter,
/// and noise becomes dimmer.
#[derive(Debug, Copy, Clone)]
pub struct FftConfig {
    /// How much to amplify the incoming signal when performing the FFT.
    pub volume: f32,

    /// How many samples per FFT block.
    /// (It's probably nonsensical to use a size less than 32 or so.)
    pub size: usize,

    /// How many samples to advance before the next FFT.
    /// Must be <= size.
    pub redraw_interval: usize,

    /// The incoming wave is [frame][channel]i16.
    /// This stores the number of channels to average (or eventually separate out).
    /// Must be >= 1.
    pub channels: ChannelCount,

    /// How to window the input signal to reduce sidelobes.
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
        assert!(cfg.redraw_interval <= cfg.size);

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
            scratch: vec![0.; cfg.size],
            spectrum: vec![FftSample::zero(); cfg.size / 2 + 1],
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

                // Remove the first `redraw_interval` samples from the vector,
                // such that `redraw_interval` samples must be pushed
                // to trigger the next redraw.
                self.buffer.drain(..self.cfg.redraw_interval);
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
            *elem *= self.cfg.volume / self.buffer.len() as f32;
        }
    }
}
