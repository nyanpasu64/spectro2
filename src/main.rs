// DFT/FFT math formulas have uppercase variables.
#![allow(non_snake_case)]
mod fft;
mod renderer;

use anyhow::{Error, Result};
use atomicbox::AtomicBox;
use core::sync::atomic::Ordering;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use fft::*;
use rustfft::num_traits::Zero;
use std::{cmp::min, sync::atomic::AtomicBool, sync::Arc};
use winit::{
    dpi::PhysicalSize,
    event::*,
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};

const MIN_FFT_SIZE: usize = 4;
const MAX_FFT_SIZE: usize = 16384;

use structopt::StructOpt;

fn parse_fft_size(src: &str) -> Result<usize> {
    let num: usize = src
        .parse()
        .map_err(|_| Error::msg(format!("FFT size {} must be an integer", src)))?;

    if num > MAX_FFT_SIZE {
        return Err(Error::msg(format!(
            "FFT size {} must be <= {}",
            num, MAX_FFT_SIZE
        )));
    }
    if num < MIN_FFT_SIZE {
        return Err(Error::msg(format!(
            "FFT size {} must be >= {}",
            num, MIN_FFT_SIZE
        )));
    }
    if num % 2 != 0 {
        return Err(Error::msg(format!("FFT size {} must be even", num)));
    }
    Ok(num)
}

fn parse_redraw_size(src: &str) -> Result<usize> {
    let num: usize = src
        .parse()
        .map_err(|_| Error::msg(format!("Redraw size {} must be an integer", src)))?;
    if num == 0 {
        return Err(Error::msg("Redraw size must be >= 0"));
    }
    Ok(num)
}

/// Real-time phase-magnitude spectrum viewer
#[derive(StructOpt, Debug)]
#[structopt(name = "spectro2")]
pub struct Opt {
    /// If passed, will listen to speaker instead of microphone.
    /// Note that this causes substantial latency (around 180ms),
    /// and you may wish to route speakers through VB-Audio Virtual Cable
    /// so both speakers and the visualization are delayed by the same amount.
    #[structopt(short, long)]
    loopback: bool,

    /// How much to amplify the incoming signal
    /// before sending it to the spectrum viewer.
    #[structopt(short, long, default_value = "20")]
    volume: f32,

    /// Number of samples to use in each FFT block.
    /// Increasing this value makes it easier to identify pitches,
    /// but increases audio latency and smearing in time.
    /// Must be a multiple of --redraw-size.
    #[structopt(short, long, default_value = "2048", parse(try_from_str = parse_fft_size))]
    fft_size: usize,

    /// Number of samples to advance time before recalculating FFT.
    /// Decreasing this value causes FFTs to be computed more often,
    /// increasing CPU usage but reducing latency and stuttering.
    ///
    /// If this value exceeds --fft-size, it is clamped to it.
    /// Otherwise must be a factor of --fft-size.
    #[structopt(short, long, default_value = "512", parse(try_from_str = parse_redraw_size))]
    redraw_size: usize,
}

impl Opt {
    fn parse_validate(&mut self) -> Result<()> {
        // Clamp redraw_size down to fft_size.
        self.redraw_size = min(self.redraw_size, self.fft_size);

        // Ensure redraw_size is a factor of fft_size.
        if self.fft_size / self.redraw_size * self.redraw_size != self.fft_size {
            return Err(Error::msg(format!(
                "FFT size {} must be a multiple of redraw size {}",
                self.fft_size, self.redraw_size
            )));
        }

        Ok(())
    }
}

/// The data to be rendered in one frame.
pub struct SpectrumFrame {
    spectrum: FftVec,
    prev_spectrum: FftVec,
}

impl SpectrumFrame {
    fn new(spectrum_size: usize) -> SpectrumFrame {
        SpectrumFrame {
            spectrum: vec![FftSample::zero(); spectrum_size],
            prev_spectrum: vec![FftSample::zero(); spectrum_size],
        }
    }
}

pub struct SpectrumFrameRef<'a> {
    spectrum: &'a FftSlice,
    prev_spectrum: &'a FftSlice,
}

struct AtomicSpectrum {
    data: AtomicBox<SpectrumFrame>,
    available: AtomicBool,
}

impl AtomicSpectrum {
    fn new(spectrum_size: usize) -> AtomicSpectrum {
        AtomicSpectrum {
            data: AtomicBox::new(Box::new(SpectrumFrame::new(spectrum_size))),
            available: false.into(),
        }
    }
}

fn main() -> Result<()> {
    let mut opt = Opt::from_args();
    opt.parse_validate()?;

    println!("");

    let host = cpal::default_host();

    let devices: Vec<cpal::Device> = host
        .devices()
        .expect("error when querying devices")
        .collect();

    println!("Devices:");
    for dev in &devices {
        println!(
            "- {}",
            match &dev.name() {
                Ok(s) => s.as_ref(),
                Err(_) => "OOPSIE WOOPSIE!! Uwu We made a fucky wucky!!",
            }
        );
        println!("    Input: {:?}", dev.default_input_config());
        println!("    Output: {:?}", dev.default_output_config());
    }
    println!("");

    // TODO add checkbox for toggling between input and loopback capture
    let device = if opt.loopback {
        host.default_output_device()
    } else {
        host.default_input_device()
    }
    .expect("no input device available");

    println!(
        "Default input device: {}",
        match &device.name() {
            Ok(s) => s.as_ref(),
            Err(_) => "OOPSIE WOOPSIE!! Uwu We made a fucky wucky!!",
        }
    );

    let supported_configs_range: Vec<cpal::SupportedStreamConfigRange> = if opt.loopback {
        device
            .supported_output_configs()
            .expect("error while querying configs")
            .collect()
    } else {
        device
            .supported_input_configs()
            .expect("error while querying configs")
            .collect()
    };

    println!("Supported configs:");
    for cfg in &supported_configs_range {
        println!("- {:?}", cfg)
    }
    println!("");

    let supported_config: cpal::SupportedStreamConfig = supported_configs_range
        .into_iter()
        .next()
        .expect("no supported config?!")
        .with_max_sample_rate();

    println!(
        "Supported buffer size: {:?}",
        supported_config.buffer_size()
    );

    let err_fn = |err| eprintln!("an error occurred on the input audio stream: {}", err);
    let config: cpal::StreamConfig = supported_config.into();

    // cpal::BufferSize::Fixed(FrameCount) is not supported on WASAPI:
    // https://github.com/RustAudio/cpal/blob/b78ff83c03a0d0b40d51dc24f49369205f022b0a/src/host/wasapi/device.rs#L650-L658
    println!("Picked buffer size: {:?}", config.buffer_size);

    let mut fft_vec_buffer = FftBuffer::new(FftConfig {
        volume: opt.volume,
        size: opt.fft_size,
        redraw_interval: opt.redraw_size,
        channels: config.channels,
        window_type: WindowType::Hann,
    });
    let spectrum_size = fft_vec_buffer.spectrum_size();
    let new_frame = || Box::new(SpectrumFrame::new(spectrum_size));

    let atomic_fft = Arc::new(AtomicSpectrum::new(spectrum_size));

    let stream = {
        let mut scratch_fft = Some(new_frame());
        let atomic_fft = atomic_fft.clone();
        let mut spectrum_callback = move |frame: SpectrumFrameRef| {
            {
                let scratch_fft = scratch_fft.as_deref_mut().unwrap();
                scratch_fft.spectrum.copy_from_slice(frame.spectrum);
                scratch_fft
                    .prev_spectrum
                    .copy_from_slice(frame.prev_spectrum);
            }

            scratch_fft = Some(
                atomic_fft
                    .data
                    .swap(scratch_fft.take().unwrap(), Ordering::AcqRel),
            );
            // If atomic_fft.data gets read and swapped before we write true,
            // the next swap will receive stale data.
            // This is possible but rare and probably doesn't matter.
            atomic_fft.available.store(true, Ordering::Release);
        };

        device
            .build_input_stream(
                &config,
                move |data, _| fft_vec_buffer.push(data, &mut spectrum_callback),
                err_fn,
            )
            .unwrap()
    };

    println!("before");
    stream.play().unwrap();

    let event_loop = EventLoop::new();
    let window = {
        let window_builder = WindowBuilder::new().with_inner_size(PhysicalSize {
            width: 1024,
            height: 768,
        });
        #[cfg(target_os = "windows")]
        let window_builder = {
            // Work around cpal/winit crash.
            // https://github.com/amethyst/amethyst/issues/2218
            use winit::platform::windows::WindowBuilderExtWindows;
            window_builder.with_drag_and_drop(false)
        };

        window_builder.build(&event_loop).unwrap()
    };

    use futures::executor::block_on;

    // Since main can't be async, we're going to need to block
    let mut state = block_on(renderer::State::new(&window, &opt, config.sample_rate.0))?;
    let mut received_fft = Some(new_frame());

    event_loop.run(move |event, _, control_flow| match event {
        Event::WindowEvent {
            ref event,
            window_id,
        } if window_id == window.id() => {
            if !state.input(event) {
                // UPDATED!
                match event {
                    WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,
                    WindowEvent::KeyboardInput { input, .. } => match input {
                        KeyboardInput {
                            state: ElementState::Pressed,
                            virtual_keycode: Some(VirtualKeyCode::Escape),
                            ..
                        } => *control_flow = ControlFlow::Exit,
                        _ => {}
                    },
                    WindowEvent::Resized(physical_size) => {
                        state.resize(*physical_size);
                    }
                    WindowEvent::ScaleFactorChanged { new_inner_size, .. } => {
                        state.resize(**new_inner_size);
                    }
                    _ => {}
                }
            }
        }
        Event::RedrawRequested(_) => {
            // might as well take the "yolo" approach,
            // and just ignore the possibility of occasional single-frame desyncs
            // and stale/missing updates.
            // this code needs to be rewritten once I add multi-frame history.
            if atomic_fft.available.swap(false, Ordering::Acquire) {
                received_fft = Some(
                    atomic_fft
                        .data
                        .swap(received_fft.take().unwrap(), Ordering::AcqRel),
                );
            }

            {
                let received_fft = received_fft.as_deref().unwrap();
                state.update(received_fft);
            }
            state.render();
        }
        Event::MainEventsCleared => {
            // RedrawRequested will only trigger once, unless we manually
            // request it.
            window.request_redraw();
        }
        _ => {}
    });
}
