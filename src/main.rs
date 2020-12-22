// DFT/FFT math formulas have uppercase variables.
#![allow(non_snake_case)]
mod common;
mod fft;
mod renderer;
mod sync;

use anyhow::{Context, Error, Result};
use clap::AppSettings;
use common::SpectrumFrameRef;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use fft::*;
use spin_sleep::LoopHelper;
use std::cmp::min;
use std::io::{self, Write};
use sync::new_spectrum_cell;
use winit::{
    dpi::PhysicalSize,
    event::*,
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};

const MIN_FFT_SIZE: usize = 4;
const MAX_FFT_SIZE: usize = 16384;

const APP_NAME: &'static str = env!("CARGO_PKG_NAME");

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
#[structopt(
    name = APP_NAME,
    global_settings(&[AppSettings::DeriveDisplayOrder, AppSettings::UnifiedHelpMessage]),
)]
pub struct Opt {
    /// If passed, will listen to output device (speaker) instead of input (microphone).
    #[structopt(short, long)]
    loopback: bool,

    /// If passed, will override which device is selected.
    ///
    /// This overrides --loopback for picking devices.
    /// However, you still need to pass --loopback if you pass an output device (speaker)
    /// to --device-index.
    #[structopt(short, long)]
    device_index: Option<usize>,

    /// How much to amplify the incoming signal before sending it to the spectrum viewer.
    #[structopt(short, long, default_value = "20")]
    volume: f32,

    /// Number of samples to use in each FFT block.
    ///
    /// Increasing this value makes it easier to identify pitches,
    /// but increases audio latency and smearing in time.
    /// Must be a multiple of --redraw-size.
    #[structopt(short, long, default_value = "2048", parse(try_from_str = parse_fft_size))]
    fft_size: usize,

    /// Number of samples to advance time before recalculating FFT.
    ///
    /// Decreasing this value causes FFTs to be computed more often,
    /// increasing CPU usage but reducing latency and stuttering.
    ///
    /// If this value exceeds --fft-size, it is clamped to it.
    /// Otherwise must be a factor of --fft-size.
    #[structopt(short, long, default_value = "512", parse(try_from_str = parse_redraw_size))]
    redraw_size: usize,

    /// Limit the FPS of the rendering thread.
    ///
    /// If set to 0, FPS is unbounded and this program will max out the CPU and/or GPU.
    ///
    /// This program does not support vsync because it adds around 3 frames of latency.
    #[structopt(long, default_value = "200")]
    fps: u32,

    /// If passed, prints FPS to the terminal.
    #[structopt(long)]
    print_fps: bool,

    /// [DEBUG] If passed, prints a peak meter to the terminal.
    ///
    /// Terminal output may have lower latency than the spectrum viewer.
    /// This will generate a lot of terminal output.
    #[structopt(short, long)]
    terminal_print: bool,

    /// [DEBUG] If passed, always renders new frames, even if the spectrum is unchanged.
    ///
    /// (A new spectrum is computed every --redraw-size samples.)
    ///
    /// Increases GPU usage and has no real benefit.
    #[structopt(long)]
    render_unchanged: bool,
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

fn vec_take<T>(mut vec: Vec<T>, index: usize) -> Option<T> {
    if index < vec.len() {
        Some(vec.swap_remove(index))
    } else {
        None
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
    for (i, dev) in devices.iter().enumerate() {
        println!(
            "{}. {}",
            i,
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
    let device = if let Some(device_index) = opt.device_index {
        vec_take(devices, device_index)
            .with_context(|| format!("Invalid --device-index {}", device_index))?
    } else {
        if opt.loopback {
            host.default_output_device()
        } else {
            host.default_input_device()
        }
        .context("no input device available")?
    };

    println!(
        "Input device: {}",
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

    // TODO pick native sampling rate.
    // ALSA Pipewire has a max_sample_rate of 384000,
    // even if device doesn't run at that rate.
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

    // For some reason, converting SupportedStreamConfig into StreamConfig
    // (SupportedStreamConfig::config())
    // throws away buffer_size and replaces with BufferSize::Default.
    let config: cpal::StreamConfig = supported_config.into();

    // cpal::BufferSize::Fixed(FrameCount) is not supported on WASAPI:
    // https://github.com/RustAudio/cpal/blob/b78ff83c03a0d0b40d51dc24f49369205f022b0a/src/host/wasapi/device.rs#L650-L658
    println!("Picked buffer size: {:?}", config.buffer_size);
    println!("Picked sample rate: {}", config.sample_rate.0);

    let mut fft_vec_buffer = FftBuffer::new(FftConfig {
        volume: opt.volume,
        size: opt.fft_size,
        redraw_interval: opt.redraw_size,
        channels: config.channels,
        window_type: WindowType::Hann,
    });
    let spectrum_size = fft_vec_buffer.spectrum_size();

    let (mut writer, mut reader) = new_spectrum_cell(spectrum_size);

    let stream = {
        let mut spectrum_callback = move |frame: SpectrumFrameRef| {
            {
                let scratch_fft = writer.get_mut();
                scratch_fft.spectrum.copy_from_slice(frame.spectrum);
                scratch_fft
                    .prev_spectrum
                    .copy_from_slice(frame.prev_spectrum);
            }

            writer.publish();
        };

        let print_to_terminal = opt.terminal_print;
        device
            .build_input_stream(
                &config,
                move |data, _| {
                    if print_to_terminal {
                        let peak = data
                            .iter()
                            .map(|&x| (x as isize).abs() as usize)
                            .fold(0, |x, y| x.max(y));
                        let nchar = peak * 100 / 32768;

                        let stdout = io::stdout();
                        let mut handle = stdout.lock();

                        handle.write_all(&b"X".repeat(nchar)).unwrap();
                        handle.write_all(b"\n").unwrap();
                    }

                    fft_vec_buffer.push(data, &mut spectrum_callback);
                },
                err_fn,
            )
            .unwrap()
    };

    println!("before");
    stream.play().unwrap();

    let event_loop = EventLoop::new();
    let window = {
        let window_builder = WindowBuilder::new()
            .with_inner_size(PhysicalSize {
                width: 1024,
                height: 768,
            })
            .with_title(APP_NAME);
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
    let mut state = block_on(renderer::State::new(&window, &opt, config.sample_rate.0))
        .context("Failed to initialize renderer")?;

    println!("GPU backend: {:?}", state.adapter_info().backend);

    // State used to track and limit FPS.
    let mut loop_helper = {
        let builder = LoopHelper::builder().report_interval_s(1.0);

        let fps_limit = opt.fps;
        if fps_limit > 0 {
            builder.build_with_target_rate(fps_limit)
        } else {
            builder.build_without_target_rate()
        }
    };

    let print_fps = opt.print_fps;
    let render_unchanged = opt.render_unchanged;

    event_loop.run(move |event, _, control_flow| match event {
        Event::WindowEvent {
            ref event,
            window_id,
        } if window_id == window.id() => {
            if !state.input(event) {
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
        Event::MainEventsCleared => {
            // apparently it's unnecessary to request_redraw() and RedrawRequested
            // when drawing on every frame, idk?

            let changed = reader.fetch();
            if changed || render_unchanged {
                let received_fft = reader.get();
                state.update(received_fft);
                state.render();
            }

            // Print FPS.
            if print_fps {
                if let Some(fps) = loop_helper.report_rate() {
                    println!("FPS: {}", fps);
                }
            }

            // Limit FPS.
            // Because renderer.rs uses PresentMode::Immediate,
            // it will render frames as fast as the CPU and GPU will allow.
            // So sleep the graphics thread to limit FPS.
            // (If loop_helper is constructed via build_without_target_rate(),
            // this is a no-op.)
            loop_helper.loop_sleep();
        }
        _ => {}
    });
}
