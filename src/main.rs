mod fft;

use anyhow::{Error, Result};
use atomicbox::AtomicOptionBox;
use core::sync::atomic::Ordering;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use fft::*;
use rustfft::num_traits::Zero;
use std::{fs::File, io::Read, path::PathBuf, slice, sync::atomic::AtomicBool};
use wgpu::util::DeviceExt;
use winit::{
    dpi::PhysicalSize,
    event::*,
    event_loop::{ControlFlow, EventLoop},
    window::{Window, WindowBuilder},
};

const MIN_FFT_SIZE: usize = 4;
const MAX_FFT_SIZE: usize = 16384;

static NEXT_FFT: AtomicOptionBox<FftVec> = AtomicOptionBox::new_none();
static FFT_AVAILABLE: AtomicBool = AtomicBool::new(false);

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

/// Real-time phase-magnitude spectrum viewer
#[derive(StructOpt, Debug)]
#[structopt(name = "basic")]
struct Opt {
    /// Number of samples to use in each FFT block
    #[structopt(short, long, default_value = "1024", parse(try_from_str = parse_fft_size))]
    fft_size: usize,
}

fn main() -> Result<()> {
    let opt = Opt::from_args();

    println!("");

    const LOOPBACK_CAPTURE: bool = true;

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
    let device = if LOOPBACK_CAPTURE {
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

    let supported_configs_range: Vec<cpal::SupportedStreamConfigRange> = if LOOPBACK_CAPTURE {
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
        size: opt.fft_size,
        channels: config.channels,
        window_type: WindowType::Hann,
    });
    let spectrum_size = fft_vec_buffer.spectrum_size();
    let new_spectrum_box =
        || -> Option<Box<FftVec>> { Some(Box::new(vec![Zero::zero(); spectrum_size])) };

    let stream = {
        {
            let old_global = NEXT_FFT.swap(new_spectrum_box(), Ordering::AcqRel);
            assert_eq!(old_global, None);
        }

        let mut scratch_fft: Option<Box<FftVec>> = new_spectrum_box();
        let mut spectrum_callback = move |spectrum: &FftSlice| {
            {
                let scratch_fft = scratch_fft
                    .as_deref_mut()
                    .expect("extra_fft/NEXT_FFT should never be None");
                scratch_fft.copy_from_slice(spectrum);
            }

            scratch_fft = NEXT_FFT.swap(scratch_fft.take(), Ordering::AcqRel);
            FFT_AVAILABLE.store(true, Ordering::Release);
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

    env_logger::init();

    let event_loop = EventLoop::new();
    let window = {
        let mut window_builder = WindowBuilder::new().with_inner_size(PhysicalSize {
            width: 1024,
            height: 1024,
        });
        if cfg!(windows) {
            // Work around cpal/winit crash.
            // https://github.com/amethyst/amethyst/issues/2218
            use winit::platform::windows::WindowBuilderExtWindows;
            window_builder = window_builder.with_drag_and_drop(false);
        }
        window_builder.build(&event_loop).unwrap()
    };

    use futures::executor::block_on;

    // Since main can't be async, we're going to need to block
    let mut state = block_on(State::new(&window, &opt))?;
    let mut received_fft = new_spectrum_box();

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
            if FFT_AVAILABLE.load(Ordering::Acquire) {
                FFT_AVAILABLE.store(false, Ordering::Relaxed);
                received_fft = NEXT_FFT.swap(received_fft.take(), Ordering::AcqRel);
                assert!(
                    received_fft.is_some(),
                    "FFT_AVAILABLE is true yet NEXT_FFT is None"
                );
            }

            state.update(
                received_fft
                    .as_ref()
                    .expect("stale_fft should never be None")
                    .as_slice(),
            );
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

#[repr(transparent)]
#[derive(Copy, Clone)]
struct PodComplex(FftSample);

unsafe impl bytemuck::Zeroable for PodComplex {}

/// Safety: Complex<f32> is a repr(C) struct of two f32, and has alignment 4.
unsafe impl bytemuck::Pod for PodComplex {}

// PodComplex is casted to vec2 and requires alignment 8 when sent to the GPU.
// This is not a problem as long as the start position within the Buffer is aligned.
type PodVec = Vec<PodComplex>;
type PodSlice = [PodComplex];

fn fft_as_pod(my_slice: &FftSlice) -> &PodSlice {
    unsafe { std::slice::from_raw_parts(my_slice.as_ptr() as *const _, my_slice.len()) }
}

/// Sent to GPU. The value equals FFT_OUT_SIZE (but is a different type).
#[repr(C)]
#[derive(Copy, Clone)]
struct GpuFftLayout {
    screen_wx: u32,
    screen_hy: u32,
    fft_out_size: u32,
}

unsafe impl bytemuck::Zeroable for GpuFftLayout {}
unsafe impl bytemuck::Pod for GpuFftLayout {}

/// The longest allowed FFT is ???.
/// The real FFT produces ??? complex bins.
fn fft_out_size(fft_input_size: usize) -> usize {
    fft_input_size / 2 + 1
}

// Docs: https://sotrh.github.io/learn-wgpu/beginner/tutorial2-swapchain/
// Code: https://github.com/sotrh/learn-wgpu/blob/master/code/beginner/tutorial2-swapchain/src/main.rs
// - https://github.com/sotrh/learn-wgpu/blob/3a46a215/code/beginner/tutorial2-swapchain/src/main.rs

struct State {
    surface: wgpu::Surface,
    device: wgpu::Device,
    queue: wgpu::Queue,
    sc_desc: wgpu::SwapChainDescriptor,
    swap_chain: wgpu::SwapChain,
    size: winit::dpi::PhysicalSize<u32>,
    render_pipeline: wgpu::RenderPipeline,

    fft_layout: GpuFftLayout,
    fft_vec: PodVec,

    fft_layout_buffer: wgpu::Buffer,
    fft_vec_buffer: wgpu::Buffer,

    bind_group: wgpu::BindGroup,
}

fn load_from_file(fname: &str) -> Result<String> {
    let mut buf: Vec<u8> = vec![];
    File::open(PathBuf::from(fname))?.read_to_end(&mut buf)?;
    Ok(String::from_utf8(buf)?)
}

impl State {
    // Creating some of the wgpu types requires async code
    async fn new(window: &Window, opt: &Opt) -> anyhow::Result<State> {
        let size = window.inner_size();

        // The instance is a handle to our GPU
        // BackendBit::PRIMARY => Vulkan + Metal + DX12 + Browser WebGPU
        let instance = wgpu::Instance::new(wgpu::BackendBit::PRIMARY);
        let surface = unsafe { instance.create_surface(window) };
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::Default,
                compatible_surface: Some(&surface),
            })
            .await
            .unwrap();

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    features: wgpu::Features::empty(),
                    limits: wgpu::Limits::default(),
                    shader_validation: true,
                },
                None, // Trace path
            )
            .await
            .unwrap();

        let sc_desc = wgpu::SwapChainDescriptor {
            usage: wgpu::TextureUsage::OUTPUT_ATTACHMENT,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::Fifo, // TODO change to Mailbox?
        };
        let swap_chain = device.create_swap_chain(&surface, &sc_desc);

        let vs_src = load_from_file("shaders/shader.vert")?;
        let fs_src = load_from_file("shaders/shader.frag")?;
        let mut compiler = shaderc::Compiler::new().unwrap();
        let vs_spirv = compiler.compile_into_spirv(
            &vs_src,
            shaderc::ShaderKind::Vertex,
            "shader.vert",
            "main",
            None,
        )?;
        let fs_spirv = compiler.compile_into_spirv(
            &fs_src,
            shaderc::ShaderKind::Fragment,
            "shader.frag",
            "main",
            None,
        )?;
        let vs_module =
            device.create_shader_module(wgpu::util::make_spirv(&vs_spirv.as_binary_u8()));
        let fs_module =
            device.create_shader_module(wgpu::util::make_spirv(&fs_spirv.as_binary_u8()));

        // # FFT SSBO
        let fft_out_size = fft_out_size(opt.fft_size);
        let fft_layout = GpuFftLayout {
            screen_wx: size.width,
            screen_hy: size.height,
            fft_out_size: fft_out_size as u32,
        };
        let fft_vec: PodVec = vec![PodComplex(Zero::zero()); fft_out_size];

        let fft_layout_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("FFT layout (size)"),
            contents: bytemuck::cast_slice(slice::from_ref(&fft_layout)),
            usage: wgpu::BufferUsage::UNIFORM | wgpu::BufferUsage::COPY_DST,
        });
        let fft_vec_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("FFT data"),
            contents: bytemuck::cast_slice(&fft_vec),
            usage: wgpu::BufferUsage::STORAGE | wgpu::BufferUsage::COPY_DST,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStage::FRAGMENT,
                    ty: wgpu::BindingType::UniformBuffer {
                        dynamic: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStage::FRAGMENT,
                    ty: wgpu::BindingType::StorageBuffer {
                        dynamic: false,
                        readonly: true,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
            label: Some("bind_group_layout"),
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(fft_layout_buffer.slice(..)),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(fft_vec_buffer.slice(..)),
                },
            ],
            label: Some("bind_group"),
        });

        // # Shader pipeline

        let render_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Render Pipeline Layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Render Pipeline"),
            layout: Some(&render_pipeline_layout),
            vertex_stage: wgpu::ProgrammableStageDescriptor {
                module: &vs_module,
                entry_point: "main", // 1.
            },
            fragment_stage: Some(wgpu::ProgrammableStageDescriptor {
                // 2.
                module: &fs_module,
                entry_point: "main",
            }),
            rasterization_state: Some(wgpu::RasterizationStateDescriptor {
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: wgpu::CullMode::Back,
                clamp_depth: false,
                depth_bias: 0,
                depth_bias_slope_scale: 0.0,
                depth_bias_clamp: 0.0,
            }),
            color_states: &[wgpu::ColorStateDescriptor {
                format: sc_desc.format,
                color_blend: wgpu::BlendDescriptor::REPLACE,
                alpha_blend: wgpu::BlendDescriptor::REPLACE,
                write_mask: wgpu::ColorWrite::ALL,
            }],
            primitive_topology: wgpu::PrimitiveTopology::TriangleList, // 1.
            depth_stencil_state: None,                                 // 2.
            vertex_state: wgpu::VertexStateDescriptor {
                index_format: wgpu::IndexFormat::Uint16, // 3.
                vertex_buffers: &[],                     // 4.
            },
            sample_count: 1,                  // 5.
            sample_mask: !0,                  // 6.
            alpha_to_coverage_enabled: false, // 7.
        });

        Ok(State {
            surface,
            device,
            queue,
            sc_desc,
            swap_chain,
            size,
            render_pipeline,
            fft_layout,
            fft_vec,
            fft_layout_buffer,
            fft_vec_buffer,
            bind_group,
        })
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        self.size = new_size;
        self.sc_desc.width = new_size.width;
        self.sc_desc.height = new_size.height;
        self.swap_chain = self.device.create_swap_chain(&self.surface, &self.sc_desc);
    }

    fn input(&mut self, event: &WindowEvent) -> bool {
        false
    }

    fn update(&mut self, spectrum: &FftSlice) {
        self.fft_layout = GpuFftLayout {
            screen_wx: self.size.width,
            screen_hy: self.size.height,
            ..self.fft_layout
        };
        self.queue.write_buffer(
            &self.fft_layout_buffer,
            0,
            bytemuck::cast_slice(slice::from_ref(&self.fft_layout)),
        );

        self.fft_vec.copy_from_slice(fft_as_pod(spectrum));
        self.queue
            .write_buffer(&self.fft_vec_buffer, 0, bytemuck::cast_slice(&self.fft_vec));
    }
    fn render(&mut self) {
        let frame = self
            .swap_chain
            .get_current_frame()
            .expect("Timeout getting texture")
            .output;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[wgpu::RenderPassColorAttachmentDescriptor {
                    attachment: &frame.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: true,
                    },
                }],
                depth_stencil_attachment: None,
            });

            render_pass.set_pipeline(&self.render_pipeline); // 2.
            render_pass.set_bind_group(0, &self.bind_group, &[]);
            render_pass.draw(0..6, 0..1); // 3.
        }

        // submit will accept anything that implements IntoIter
        self.queue.submit(std::iter::once(encoder.finish()));
    }
}
