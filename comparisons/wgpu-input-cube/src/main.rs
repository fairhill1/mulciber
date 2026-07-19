//! Input comparison implementation: the same interactive cube as `examples/input-cube`, written
//! as an ordinary best-practice `wgpu` + `winit` application.
//!
//! It shares the exact WGSL module, scene data, controls, and interaction math with the Mulciber
//! input example. Validation-only flags remain in the separate graphics comparison probe rather
//! than inflating this ordinary interactive application.

mod scene;

use std::error::Error;
use std::sync::Arc;
use std::time::Instant;

use glam::Quat;
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

const SHADER: &str = include_str!("../../../examples/cube/src/cube.wgsl");
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    let mut app = App {
        window: None,
        gpu: None,
        started: Instant::now(),
        failure: None,
        interaction: Interaction::default(),
    };
    event_loop.run_app(&mut app)?;
    if let Some(failure) = app.failure {
        return Err(failure);
    }
    Ok(())
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    started: Instant,
    failure: Option<Box<dyn Error>>,
    interaction: Interaction,
}

struct Interaction {
    animation_seconds: f32,
    last_frame_seconds: Option<f32>,
    paused: bool,
    orientation: Quat,
    distance_offset: f32,
    dragging: bool,
    pointer: Option<LogicalPosition<f64>>,
    capture_intended: bool,
    captured: bool,
}

impl Default for Interaction {
    fn default() -> Self {
        Self {
            animation_seconds: 0.0,
            last_frame_seconds: None,
            paused: true,
            orientation: Quat::IDENTITY,
            distance_offset: 0.0,
            dragging: false,
            pointer: None,
            capture_intended: false,
            captured: false,
        }
    }
}

impl Interaction {
    fn key(&mut self, key: KeyCode, repeat: bool) {
        match key {
            KeyCode::KeyA | KeyCode::ArrowLeft => self.rotate(-0.12, 0.0),
            KeyCode::KeyD | KeyCode::ArrowRight => self.rotate(0.12, 0.0),
            KeyCode::KeyW | KeyCode::ArrowUp => self.rotate(0.0, -0.12),
            KeyCode::KeyS | KeyCode::ArrowDown => self.rotate(0.0, 0.12),
            KeyCode::Space if !repeat => self.paused = !self.paused,
            KeyCode::KeyR if !repeat => self.reset_view(),
            _ => {}
        }
    }

    fn focus(&mut self, focused: bool) {
        if !focused {
            self.dragging = false;
            self.pointer = None;
        }
    }

    fn pointer_moved(&mut self, position: LogicalPosition<f64>) {
        if self.dragging
            && let Some(previous) = self.pointer
        {
            let yaw = (position.x - previous.x) as f32 * 0.008;
            let pitch = (position.y - previous.y) as f32 * 0.008;
            self.rotate(yaw, pitch);
        }
        self.pointer = Some(position);
    }

    fn primary_button(&mut self, state: ElementState) {
        self.dragging = state == ElementState::Pressed;
    }

    fn pointer_delta(&mut self, delta_x: f64, delta_y: f64) {
        self.rotate(delta_x as f32 * 0.008, delta_y as f32 * 0.008);
    }

    fn scroll(&mut self, delta: MouseScrollDelta, scale_factor: f64) {
        let y = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y) * 0.18,
            MouseScrollDelta::PixelDelta(position) => position.y / scale_factor * 0.015,
        };
        self.distance_offset = (self.distance_offset - y as f32).clamp(-1.5, 8.0);
    }

    fn reset_view(&mut self) {
        self.orientation = Quat::IDENTITY;
        self.distance_offset = 0.0;
    }

    fn rotate(&mut self, yaw: f32, pitch: f32) {
        let screen_rotation = Quat::from_rotation_x(pitch) * Quat::from_rotation_y(yaw);
        self.orientation = (screen_rotation * self.orientation).normalize();
    }

    fn animation_time(&mut self, frame_seconds: f32) -> f32 {
        if let Some(previous) = self.last_frame_seconds
            && !self.paused
        {
            self.animation_seconds += frame_seconds - previous;
        }
        self.last_frame_seconds = Some(frame_seconds);
        self.animation_seconds
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("wgpu — input cube comparison")
            .with_inner_size(LogicalSize::new(960, 540));
        match event_loop.create_window(attributes) {
            Ok(window) => {
                let window = Arc::new(window);
                match pollster::block_on(Gpu::new(window.clone())) {
                    Ok(gpu) => {
                        println!(
                            "backend: wgpu ({:?}), samples: {}",
                            gpu.backend,
                            if gpu.sample_count == 4 { "Four" } else { "One" }
                        );
                        println!(
                            "input: W/A/S/D or arrows rotate, primary-button drag orbits, scroll zooms, Space toggles spin, R resets"
                        );
                        println!(
                            "input: C captures the pointer for relative look, Escape releases it"
                        );
                        self.gpu = Some(gpu);
                        self.window = Some(window);
                    }
                    Err(error) => {
                        self.failure = Some(error);
                        event_loop.exit();
                    }
                }
            }
            Err(error) => {
                self.failure = Some(Box::new(error));
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let (Some(window), Some(gpu)) = (self.window.as_ref(), self.gpu.as_mut()) else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Focused(focused) => {
                self.interaction.focus(focused);
                // Hand-rolled capture suspension across focus changes; the platform offers none.
                if focused {
                    if self.interaction.capture_intended {
                        self.interaction.captured = grab_pointer(window, true);
                    }
                } else if self.interaction.captured {
                    grab_pointer(window, false);
                    self.interaction.captured = false;
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let PhysicalKey::Code(key) = event.physical_key {
                    match key {
                        KeyCode::KeyC if !event.repeat => {
                            if self.interaction.capture_intended {
                                grab_pointer(window, false);
                                self.interaction.capture_intended = false;
                                self.interaction.captured = false;
                                println!("cursor mode: Normal");
                            } else if grab_pointer(window, true) {
                                self.interaction.capture_intended = true;
                                self.interaction.captured = true;
                                println!("cursor mode: Captured");
                            } else {
                                println!("pointer capture: refused by the platform");
                            }
                        }
                        KeyCode::Escape => {
                            if self.interaction.captured {
                                grab_pointer(window, false);
                            }
                            self.interaction.capture_intended = false;
                            self.interaction.captured = false;
                        }
                        _ => self.interaction.key(key, event.repeat),
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => self
                .interaction
                .pointer_moved(position.to_logical(window.scale_factor())),
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => self.interaction.primary_button(state),
            WindowEvent::MouseWheel { delta, .. } => {
                self.interaction.scroll(delta, window.scale_factor())
            }
            WindowEvent::Resized(size) => {
                gpu.resize(size.width, size.height);
            }
            WindowEvent::RedrawRequested => {
                let animation_seconds = self
                    .interaction
                    .animation_time(self.started.elapsed().as_secs_f32());
                match gpu.render(animation_seconds, &self.interaction) {
                    Ok(true) => {}
                    Ok(false) => {}
                    Err(error) => {
                        self.failure = Some(error);
                        event_loop.exit();
                        return;
                    }
                }
                window.request_redraw();
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta: (x, y) } = event
            && self.interaction.captured
        {
            self.interaction.pointer_delta(x, y);
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

/// The `CursorGrabMode::Locked` then `Confined` fallback plus visibility bookkeeping that every
/// surveyed consumer hand-rolls above this stack.
fn grab_pointer(window: &Window, capture: bool) -> bool {
    if capture {
        let grabbed = window.set_cursor_grab(CursorGrabMode::Locked).is_ok()
            || window.set_cursor_grab(CursorGrabMode::Confined).is_ok();
        if grabbed {
            window.set_cursor_visible(false);
        }
        grabbed
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
        false
    }
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    configured: bool,
    backend: wgpu::Backend,
    sample_count: u32,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    depth_view: wgpu::TextureView,
    multisample_view: Option<wgpu::TextureView>,
}

impl Gpu {
    async fn new(window: Arc<Window>) -> Result<Self, Box<dyn Error>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await?;

        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(capabilities.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: capabilities.alpha_modes[0],
            color_space: wgpu::SurfaceColorSpace::Auto,
            view_formats: Vec::new(),
        };
        surface.configure(&device, &config);
        println!("surface configured at {}x{}", config.width, config.height);

        let sample_count = 4;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cube shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube vertices"),
            contents: bytemuck::cast_slice(&scene::CUBE_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube indices"),
            contents: bytemuck::cast_slice(&scene::CUBE_INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cube uniforms"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let texels = scene::checkerboard();
        let texture = device.create_texture_with_data(
            &queue,
            &wgpu::TextureDescriptor {
                label: Some("cube checkerboard"),
                size: wgpu::Extent3d {
                    width: 8,
                    height: 8,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &texels,
        );
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("cube sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cube bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cube bindings"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cube pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cube pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("cube_vertex"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 32,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,
                        1 => Float32x3,
                        2 => Float32x2,
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("cube_fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });

        let (depth_view, multisample_view) = create_targets(&device, &config, sample_count);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            configured: true,
            backend: adapter.get_info().backend,
            sample_count,
            pipeline,
            bind_group,
            uniform_buffer,
            vertex_buffer,
            index_buffer,
            depth_view,
            multisample_view,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            self.configured = false;
            return;
        }
        if self.configured && width == self.config.width && height == self.config.height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.configured = true;
        println!("surface configured at {width}x{height}");
        let (depth_view, multisample_view) =
            create_targets(&self.device, &self.config, self.sample_count);
        self.depth_view = depth_view;
        self.multisample_view = multisample_view;
    }

    fn render(&mut self, seconds: f32, interaction: &Interaction) -> Result<bool, Box<dyn Error>> {
        if !self.configured {
            return Ok(false);
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                let (width, height) = (self.config.width, self.config.height);
                self.configured = false;
                self.resize(width, height);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("surface acquisition failed validation".into());
            }
        };
        let aspect = self.config.width as f32 / self.config.height as f32;
        self.queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::cast_slice(&scene::transform(
                seconds,
                aspect,
                interaction.orientation,
                4.0 + interaction.distance_offset,
            )),
        );

        let frame_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let (attachment_view, resolve_target) = match self.multisample_view.as_ref() {
            Some(multisample_view) => (multisample_view, Some(&frame_view)),
            None => (&frame_view, None),
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("cube frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cube pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: attachment_view,
                    depth_slice: None,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.025,
                            g: 0.035,
                            b: 0.055,
                            a: 1.0,
                        }),
                        store: if resolve_target.is_some() {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..scene::CUBE_INDICES.len() as u32, 0, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        Ok(true)
    }
}

fn create_targets(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
    sample_count: u32,
) -> (wgpu::TextureView, Option<wgpu::TextureView>) {
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cube depth"),
        size: wgpu::Extent3d {
            width: config.width,
            height: config.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let multisample = (sample_count > 1).then(|| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("cube multisample color"),
                size: wgpu::Extent3d {
                    width: config.width,
                    height: config.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: config.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    });
    (
        depth.create_view(&wgpu::TextureViewDescriptor::default()),
        multisample,
    )
}
