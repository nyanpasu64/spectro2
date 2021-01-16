use crate::common::{FftSample, FftSlice, SpectrumFrame};
use crate::Opt;
use anyhow::{Context, Result};
use itertools::izip;
use num_traits::Zero;
use std::{fs::File, io::Read, path::PathBuf, slice};
use wgpu::util::DeviceExt;
use winit::{event::*, window::Window};

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

/// Sent to GPU. Controls FFT layout and options.
#[repr(C)]
#[derive(Copy, Clone)]
struct GpuRenderParameters {
    /// Screen size.
    screen_wx: u32,
    screen_hy: u32,

    /// Samples per second.
    sample_rate: u32,

    /// Number of FFT bins between 0 and Nyquist inclusive.
    /// Equals nsamp/2 + 1.
    fft_out_size: u32,
}

unsafe impl bytemuck::Zeroable for GpuRenderParameters {}
unsafe impl bytemuck::Pod for GpuRenderParameters {}

/// The longest allowed FFT is ???.
/// The real FFT produces ??? complex bins.
fn fft_out_size(fft_input_size: usize) -> usize {
    fft_input_size / 2 + 1
}

// Docs: https://sotrh.github.io/learn-wgpu/beginner/tutorial2-swapchain/
// Code: https://github.com/sotrh/learn-wgpu/blob/master/code/beginner/tutorial2-swapchain/src/main.rs
// - https://github.com/sotrh/learn-wgpu/blob/3a46a215/code/beginner/tutorial2-swapchain/src/main.rs

pub struct State {
    adapter_info: wgpu::AdapterInfo,
    surface: wgpu::Surface,
    device: wgpu::Device,
    queue: wgpu::Queue,
    sc_desc: wgpu::SwapChainDescriptor,
    swap_chain: wgpu::SwapChain,
    size: winit::dpi::PhysicalSize<u32>,
    render_pipeline: wgpu::RenderPipeline,

    render_parameters: GpuRenderParameters,
    fft_vec: PodVec,

    render_param_buffer: wgpu::Buffer,
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
    pub async fn new(window: &Window, opt: &Opt, sample_rate: u32) -> anyhow::Result<State> {
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
            .context("Failed to create adapter")?;

        let adapter_info = adapter.get_info();

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
            .context("Failed to create device")?;

        let sc_desc = wgpu::SwapChainDescriptor {
            usage: wgpu::TextureUsage::OUTPUT_ATTACHMENT,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::Immediate,
        };
        // PresentMode::Fifo adds around 3 frames of latency.
        // And polling the device before/after submitting each frame doesn't help.

        let swap_chain = device.create_swap_chain(&surface, &sc_desc);

        let vs_src = load_from_file("shaders/shader.vert")?;
        let fs_src = load_from_file("shaders/shader.frag")?;
        let mut compiler =
            shaderc::Compiler::new().context("Failed to initialize shader compiler")?;
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
        let render_parameters = GpuRenderParameters {
            screen_wx: size.width,
            screen_hy: size.height,
            fft_out_size: fft_out_size as u32,
            sample_rate,
        };
        let fft_vec: PodVec = vec![PodComplex(FftSample::zero()); fft_out_size];

        let render_param_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("FFT layout (size)"),
            contents: bytemuck::cast_slice(slice::from_ref(&render_parameters)),
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
                    resource: wgpu::BindingResource::Buffer(render_param_buffer.slice(..)),
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
            adapter_info,
            surface,
            device,
            queue,
            sc_desc,
            swap_chain,
            size,
            render_pipeline,
            render_parameters,
            fft_vec,
            render_param_buffer,
            fft_vec_buffer,
            bind_group,
        })
    }

    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.adapter_info
    }

    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        self.size = new_size;
        self.sc_desc.width = new_size.width;
        self.sc_desc.height = new_size.height;
        self.swap_chain = self.device.create_swap_chain(&self.surface, &self.sc_desc);
    }

    pub fn input(&mut self, _event: &WindowEvent) -> bool {
        false
    }

    pub fn update(&mut self, frame: &SpectrumFrame) {
        self.render_parameters = GpuRenderParameters {
            screen_wx: self.size.width,
            screen_hy: self.size.height,
            ..self.render_parameters
        };
        self.queue.write_buffer(
            &self.render_param_buffer,
            0,
            bytemuck::cast_slice(slice::from_ref(&self.render_parameters)),
        );

        const PHASE_DERIVATIVE: bool = true;

        assert_eq!(self.fft_vec.len(), frame.spectrum.len());
        assert_eq!(self.fft_vec.len(), frame.prev_spectrum.len());
        if PHASE_DERIVATIVE {
            for (out, curr, prev) in izip!(&mut self.fft_vec, &frame.spectrum, &frame.prev_spectrum)
            {
                *out = PodComplex(FftSample::from_polar(curr.norm(), curr.arg() - prev.arg()))
            }
        } else {
            self.fft_vec.copy_from_slice(fft_as_pod(&frame.spectrum));
        }

        self.queue
            .write_buffer(&self.fft_vec_buffer, 0, bytemuck::cast_slice(&self.fft_vec));
    }

    pub fn render(&mut self) {
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
