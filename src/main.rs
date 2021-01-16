// DFT/FFT math formulas have uppercase variables.
#![allow(non_snake_case)]
mod common;
mod fft;
mod renderer;
mod sync;

use anyhow::{bail, Context, Error, Result};
use clap::AppSettings;
use common::SpectrumFrameRef;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use fft::*;
use indoc::formatdoc;
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
    /// If passed, prints a list of audio devices, and stream modes for the chosen device.
    #[structopt(short = "D", long)]
    show_devices: bool,

    /// If passed, will override which device is selected.
    ///
    /// This overrides --loopback for picking devices.
    /// However, you still need to pass --loopback if you pass an output device (speaker)
    /// to --device-index.
    #[structopt(short, long)]
    device_index: Option<usize>,

    /// Override the default sampling rate of the audio device.
    ///
    /// If not passed, on Linux PulseAudio setups, spectro2 opens the input device at 384000 Hz
    /// and not the actual PulseAudio sampling rate.
    #[structopt(short, long)]
    sample_rate: Option<u32>,

    /// Override the default channel count of the audio device.
    ///
    /// If not passed, on Linux PulseAudio setups, spectro2 opens the input device with 1 channel
    /// and not the actual PulseAudio channel count.
    #[structopt(short, long)]
    channels: Option<u32>,

    /// If passed, will listen to output device (speaker) instead of input (microphone).
    ///
    /// Primarily intended for Windows WASAPI. Does not work on Linux PulseAudio;
    /// instead use pavucontrol to switch the audio input to speaker loopback.
    #[structopt(short, long)]
    loopback: bool,

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
        .context("error when querying devices")?
        .collect();

    if opt.show_devices {
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
    }

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

    let device_name = device.name();
    let device_name = match &device_name {
        Ok(ref s) => s.as_ref(),
        Err(_) => "OOPSIE WOOPSIE!! Uwu We made a fucky wucky!!",
    };

    println!("Input device: {}", device_name);

    let supported_config_ranges: Vec<cpal::SupportedStreamConfigRange> = if opt.loopback {
        device
            .supported_output_configs()
            .context("error while querying configs")?
            .collect()
    } else {
        device
            .supported_input_configs()
            .context("error while querying configs")?
            .collect()
    };

    // ALSA expects options to be determined by the application, not the OS
    // (which supplies a range of possibilities).
    let is_alsa = host.id().name() == "ALSA";

    // If we're on ALSA and the user hasn't set both channel count and sampling rate, warn the user.
    let should_warn = is_alsa && !(opt.channels.is_some() && opt.sample_rate.is_some());

    if opt.show_devices || should_warn {
        println!("Supported configs:");
        for cfg in &supported_config_ranges {
            println!("- {:?}", cfg)
        }
        println!("");
    };

    if should_warn {
        // When cpal uses the ALSA API to talk to Pulse (or some other devices),
        // the OS-supplied options range is gibberish
        // (ranging from 1 to 384000 Hz, and 1 to 32 channels).
        // The application must pick options itself.
        //
        // The conditional is an arbitrarily chosen heuristic for detecting cases
        // where cpal's default rate or channel count are unacceptable.
        //
        // On my system, the "default" device points to "pulse" and has the same issues,
        // and "sysdefault" has sampling rates from 4000 to 2^32-1 Hz and 32 possible channel counts.
        let bad_alsa = device_name == "pulse"
            || supported_config_ranges.len() > 8
            || supported_config_ranges[0].max_sample_rate().0 >= 1_000_000;

        let mut args = Vec::with_capacity(2);
        if opt.sample_rate.is_none() {
            args.push("--sample-rate 48000");
        }
        if opt.channels.is_none() {
            args.push("--channels 2");
        }
        let args = args.join(" ");

        let msg = formatdoc!(
            "Try appending the following (or values of your choice) to the command line:
                {}
            If running from the Git repository, try:
                cargo run -- [ARGS]",
            args,
        );
        if bad_alsa {
            bail!(
                "The current ALSA device (eg. PulseAudio) requires specifying a sampling rate and channel count manually.\n{}",
                msg
            );
        } else {
            println!(
                "Warning: On ALSA, this app may not use the correct sampling rate and channel count.\n{}\n",
                msg
            );
        }
    }

    let supported_config: cpal::SupportedStreamConfig = {
        // In cpal, each SupportedStreamConfigRange has a single channel count.
        // Pick either the first SupportedStreamConfigRange,
        // or the first one with the user-specified channel count.
        let range: cpal::SupportedStreamConfigRange = {
            let first_range = supported_config_ranges
                .get(0)
                .context("no supported config?!")?;

            if let Some(channels) = opt.channels {
                let first_valid_range = supported_config_ranges
                    .iter()
                    .filter(|range| range.channels() as u32 == channels)
                    .next();
                match first_valid_range {
                    Some(range) => range,
                    None => {
                        println!(
                            "Requested channel count {} not supported, falling back to {}",
                            channels,
                            first_range.channels()
                        );
                        first_range
                    }
                }
            } else {
                first_range
            }
        }
        .clone();
        drop(supported_config_ranges);

        // In cpal, each SupportedStreamConfigRange has a range of sampling rates.
        // Pick the maximum sampling rate,
        // or clamp the user-specified sampling rate within the allowed range.
        if let Some(mut sample_rate) = opt.sample_rate {
            let min_sample_rate = range.min_sample_rate().0;
            let max_sample_rate = range.max_sample_rate().0;

            if !(min_sample_rate..=max_sample_rate).contains(&sample_rate) {
                let clamped_rate = num_traits::clamp(sample_rate, min_sample_rate, max_sample_rate);
                println!(
                    "Requested sample rate {} not supported, falling back to {}",
                    sample_rate, clamped_rate
                );
                sample_rate = clamped_rate;
            }
            range.with_sample_rate(cpal::SampleRate(sample_rate))
        } else {
            range.with_max_sample_rate()
        }
    };

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
            .context("Error building input stream")?
    };

    println!("Playing audio device...");
    stream.play().context("Error playing audio device")?;

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

        window_builder
            .build(&event_loop)
            .context("Error creating window")?
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
