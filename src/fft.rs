use crate::common::{FftSample, FftVec, RealVec, SpectrumFrameRef};
use cpal::ChannelCount;
use dsp::window::Window;
use rustfft::num_traits::Zero;

pub type FftCallback<'a> = &'a mut dyn FnMut(SpectrumFrameRef);

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
    /// Must be a factor of size.
    pub redraw_interval: usize,

    /// The incoming wave is \[frame\]\[channel\]i16.
    /// This stores the number of channels to average (or eventually separate out).
    /// Must be >= 1.
    pub channels: ChannelCount,

    /// How to window the input signal to reduce sidelobes.
    pub window_type: WindowType,
    // TODO downmix: bool,
    // TODO add option for whether to allow multiple calls in the same push.
}

mod history {
    /// Always-full circular buffer used as a delay line.
    pub struct History<T> {
        items: Vec<T>,
        index: usize,
    }

    impl<T> History<T> {
        pub fn new(item: T, count: usize) -> History<T>
        where
            T: Clone,
        {
            History {
                items: vec![item; count],
                index: 0,
            }
        }

        /// The oldest item is the one which will be overwritten next.
        fn oldest_idx(&self) -> usize {
            (self.index + 1) % self.items.len()
        }

        /// The newest item is the one currently accessible.
        fn newest_index(&self) -> usize {
            self.index
        }

        pub fn oldest(&self) -> &T {
            &self.items[self.oldest_idx()]
        }

        pub fn newest(&self) -> &T {
            &self.items[self.newest_index()]
        }

        pub fn newest_mut(&mut self) -> &mut T {
            let i = self.newest_index();
            &mut self.items[i]
        }

        pub fn advance_newest(&mut self) {
            self.index = (self.index + 1) % self.items.len();
        }
    }
}
use history::History;

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
    // We store a history of spectrums,
    // so we can compare the phase of non-overlapping portions of the signal.
    spectrum_history: History<FftVec>,
}

impl FftBuffer {
    pub fn new(cfg: FftConfig) -> FftBuffer {
        assert!(cfg.size >= 2);
        assert!(cfg.channels >= 1);
        assert!(cfg.redraw_interval <= cfg.size);
        assert_eq!(
            cfg.size / cfg.redraw_interval * cfg.redraw_interval,
            cfg.size
        );

        // Each FFT is cfg.size long in the time domain.
        // We compute FFTs every cfg.redraw_interval.
        // So it takes (history_len * cfg.redraw_interval) FFTs
        // to get another one which doesn't overlap in the time domain.
        let history_len = cfg.size / cfg.redraw_interval;
        let spectrum_size = cfg.size / 2 + 1;
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
            // Store entries from 0 through `history_len` ago, inclusive.
            spectrum_history: History::new(vec![FftSample::zero(); spectrum_size], history_len + 1),
        }
    }

    pub fn spectrum_size(&self) -> usize {
        self.spectrum_history.newest().len()
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
                self.run_fft(); // mutates self
                fft_callback(SpectrumFrameRef {
                    spectrum: self.spectrum_history.newest(),
                    prev_spectrum: self.spectrum_history.oldest(),
                });

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
    /// - self.spectrum_history is rotated, and the newest entry has been overwritten.
    /// - self.buffer is unchanged.
    fn run_fft(&mut self) {
        if let Some(window) = &self.window {
            // Precondition: LHS, input, and output have same length.
            window.apply(&self.buffer, &mut self.scratch);
        } else {
            // Precondition: LHS and src have same length.
            (&mut self.scratch).copy_from_slice(&self.buffer);
        }

        // Phase-shift in time domain, so peak of window lies at sample 0.
        let N = self.scratch.len();
        self.scratch.rotate_right(N / 2);

        self.spectrum_history.advance_newest();
        let spectrum = self.spectrum_history.newest_mut();
        self.fft.process(&mut self.scratch, spectrum).unwrap();

        // Normalize transform, so longer inputs don't produce larger spectrum values.
        for elem in spectrum {
            *elem *= self.cfg.volume / self.buffer.len() as f32;
        }
    }
}
