//! Ordinary `wgpu` resource, dynamic-uniform, multi-draw, resize, and postprocess plumbing.

use std::error::Error;
use std::num::NonZeroU64;
use std::sync::Arc;

use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::scene;

const SHADER: &str = include_str!("../../../examples/postprocess-cube/src/postprocess.wgsl");
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const OBJECT_COUNT: usize = 100;

struct Targets {
    scene_color: wgpu::TextureView,
    depth: wgpu::TextureView,
    multisample: Option<wgpu::TextureView>,
}

struct Mesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

pub(crate) struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    configured: bool,
    backend: wgpu::Backend,
    sample_count: u32,
    uniform_stride: u64,
    uniform_buffer: wgpu::Buffer,
    meshes: [Mesh; 2],
    scene_pipeline: wgpu::RenderPipeline,
    scene_bind_groups: [wgpu::BindGroup; 2],
    postprocess_pipeline: wgpu::RenderPipeline,
    postprocess_layout: wgpu::BindGroupLayout,
    postprocess_sampler: wgpu::Sampler,
    postprocess_bind_group: wgpu::BindGroup,
    targets: Targets,
}

impl Gpu {
    pub(crate) async fn new(window: Arc<Window>) -> Result<Self, Box<dyn Error>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(Arc::clone(&window))?;
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
        let sample_count = if adapter
            .get_texture_format_features(format)
            .flags
            .contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4)
        {
            4
        } else {
            1
        };
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

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("multi-object scene shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let meshes = [
            mesh(&device, "cube", &scene::CUBE_VERTICES, &scene::CUBE_INDICES),
            mesh(
                &device,
                "pyramid",
                &scene::PYRAMID_VERTICES,
                &scene::PYRAMID_INDICES,
            ),
        ];
        let uniform_stride = u64::from(device.limits().min_uniform_buffer_offset_alignment.max(64));
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene transforms"),
            size: uniform_stride * u64::try_from(OBJECT_COUNT).expect("object count fits u64"),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let texture_views = [
            texture(
                &device,
                &queue,
                "amber checkerboard",
                &scene::checkerboard([245, 165, 40], [35, 90, 210]),
            ),
            texture(
                &device,
                &queue,
                "violet checkerboard",
                &scene::checkerboard([175, 70, 235], [25, 180, 145]),
            ),
        ];
        let scene_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("scene sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let postprocess_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("postprocess sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let layout = bind_group_layout(&device);
        let scene_bind_groups = [
            bind_group(
                &device,
                "amber scene bindings",
                &layout,
                &uniform_buffer,
                &texture_views[0],
                &scene_sampler,
            ),
            bind_group(
                &device,
                "violet scene bindings",
                &layout,
                &uniform_buffer,
                &texture_views[1],
                &scene_sampler,
            ),
        ];
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let scene_pipeline =
            scene_pipeline(&device, &pipeline_layout, &shader, format, sample_count);
        let postprocess_pipeline = postprocess_pipeline(&device, &pipeline_layout, &shader, format);
        let targets = create_targets(&device, &config, sample_count);
        let postprocess_bind_group = bind_group(
            &device,
            "postprocess bindings",
            &layout,
            &uniform_buffer,
            &targets.scene_color,
            &postprocess_sampler,
        );

        Ok(Self {
            surface,
            device,
            queue,
            config,
            configured: true,
            backend: adapter.get_info().backend,
            sample_count,
            uniform_stride,
            uniform_buffer,
            meshes,
            scene_pipeline,
            scene_bind_groups,
            postprocess_pipeline,
            postprocess_layout: layout,
            postprocess_sampler,
            postprocess_bind_group,
            targets,
        })
    }

    pub(crate) const fn backend(&self) -> wgpu::Backend {
        self.backend
    }

    pub(crate) const fn sample_count(&self) -> u32 {
        self.sample_count
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) {
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
        self.targets = create_targets(&self.device, &self.config, self.sample_count);
        self.postprocess_bind_group = bind_group(
            &self.device,
            "postprocess bindings",
            &self.postprocess_layout,
            &self.uniform_buffer,
            &self.targets.scene_color,
            &self.postprocess_sampler,
        );
        self.configured = true;
    }

    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn render(&mut self, seconds: f32) -> Result<(), Box<dyn Error>> {
        if !self.configured {
            return Ok(());
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                let (width, height) = (self.config.width, self.config.height);
                self.configured = false;
                self.resize(width, height);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("surface acquisition failed validation".into());
            }
        };
        let aspect = self.config.width as f32 / self.config.height as f32;
        let transforms = scene::transforms(seconds, aspect);
        let stride = usize::try_from(self.uniform_stride).expect("uniform stride fits usize");
        let mut bytes = vec![0_u8; transforms.len() * stride];
        for (index, transform) in transforms.iter().enumerate() {
            let transform = bytemuck::cast_slice(std::slice::from_ref(transform));
            bytes[index * stride..index * stride + transform.len()].copy_from_slice(transform);
        }
        self.queue.write_buffer(&self.uniform_buffer, 0, &bytes);

        let frame_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("multi-object frame"),
            });
        self.encode_scene(&mut encoder, transforms.len());
        self.encode_postprocess(&mut encoder, &frame_view);
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        Ok(())
    }

    fn encode_scene(&self, encoder: &mut wgpu::CommandEncoder, draw_count: usize) {
        let (view, resolve_target, store) = match self.targets.multisample.as_ref() {
            Some(multisample) => (
                multisample,
                Some(&self.targets.scene_color),
                wgpu::StoreOp::Discard,
            ),
            None => (&self.targets.scene_color, None, wgpu::StoreOp::Store),
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("multi-object scene pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.012,
                        g: 0.018,
                        b: 0.032,
                        a: 1.0,
                    }),
                    store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.targets.depth,
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
        pass.set_pipeline(&self.scene_pipeline);
        for index in 0..draw_count {
            let mesh = &self.meshes[index % 2];
            let texture = usize::from(index % 3 == 0);
            let offset = u32::try_from(index)
                .expect("draw index fits u32")
                .checked_mul(u32::try_from(self.uniform_stride).expect("uniform stride fits u32"))
                .expect("dynamic uniform offset fits u32");
            pass.set_bind_group(0, &self.scene_bind_groups[texture], &[offset]);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        }
    }

    fn encode_postprocess(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        frame_view: &wgpu::TextureView,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("fullscreen postprocess pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: frame_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.postprocess_pipeline);
        pass.set_bind_group(0, &self.postprocess_bind_group, &[0]);
        pass.draw(0..3, 0..1);
    }
}

fn mesh(device: &wgpu::Device, label: &str, vertices: &[scene::Vertex], indices: &[u16]) -> Mesh {
    Mesh {
        vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(&format!("{label} vertices")),
            contents: bytemuck::cast_slice(vertices),
            usage: wgpu::BufferUsages::VERTEX,
        }),
        indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(&format!("{label} indices")),
            contents: bytemuck::cast_slice(indices),
            usage: wgpu::BufferUsages::INDEX,
        }),
        index_count: u32::try_from(indices.len()).expect("index count fits u32"),
    }
}

fn texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    texels: &[u8],
) -> wgpu::TextureView {
    device
        .create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some(label),
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
            texels,
        )
        .create_view(&wgpu::TextureViewDescriptor::default())
}

fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("scene bindings"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(64),
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
    })
}

fn bind_group(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    texture: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: uniform,
                    offset: 0,
                    size: NonZeroU64::new(64),
                }),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(texture),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

fn scene_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    sample_count: u32,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("multi-object scene pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
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
            module: shader,
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
    })
}

fn postprocess_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("fullscreen postprocess pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("post_vertex"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("post_fragment"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn create_targets(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
    sample_count: u32,
) -> Targets {
    let descriptor = |label, format, samples, usage| wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: config.width,
            height: config.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: samples,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    };
    let scene_color = device.create_texture(&descriptor(
        "resolved scene color",
        config.format,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
    ));
    let depth = device.create_texture(&descriptor(
        "scene depth",
        DEPTH_FORMAT,
        sample_count,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    ));
    let multisample = (sample_count > 1).then(|| {
        device
            .create_texture(&descriptor(
                "multisample scene color",
                config.format,
                sample_count,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            ))
            .create_view(&wgpu::TextureViewDescriptor::default())
    });
    Targets {
        scene_color: scene_color.create_view(&wgpu::TextureViewDescriptor::default()),
        depth: depth.create_view(&wgpu::TextureViewDescriptor::default()),
        multisample,
    }
}
