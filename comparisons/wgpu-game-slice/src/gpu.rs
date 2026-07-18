//! Ordinary wgpu instance-buffer, resize, depth/MSAA, and postprocess plumbing.

use std::error::Error;
use std::sync::Arc;

use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::game::Game;
use crate::scene;

const SHADER: &str = include_str!("../../../examples/instanced-scene/src/instanced.wgsl");
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const INSTANCE_TRANSFORM_SIZE: u64 = 64;
const OBJECT_COUNT: u64 = 26;

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
    instance_buffer: wgpu::Buffer,
    meshes: [Mesh; 2],
    scene_pipeline: wgpu::RenderPipeline,
    scene_bind_groups: [wgpu::BindGroup; 5],
    postprocess_pipeline: wgpu::RenderPipeline,
    bindings_layout: wgpu::BindGroupLayout,
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
            label: Some("Forge Run shader"),
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
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Forge Run instance transforms"),
            size: INSTANCE_TRANSFORM_SIZE * OBJECT_COUNT,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let texture_views = [
            texture(
                &device,
                &queue,
                "ground",
                &scene::checkerboard([30, 42, 58], [55, 72, 82], 1),
            ),
            texture(
                &device,
                &queue,
                "obstacles",
                &scene::checkerboard([100, 105, 120], [42, 48, 65], 2),
            ),
            texture(
                &device,
                &queue,
                "player",
                &scene::checkerboard([35, 230, 190], [20, 90, 145], 2),
            ),
            texture(
                &device,
                &queue,
                "pickups",
                &scene::checkerboard([255, 195, 45], [245, 80, 30], 1),
            ),
            texture(
                &device,
                &queue,
                "hazards",
                &scene::checkerboard([245, 45, 85], [100, 20, 135], 2),
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
        let bindings_layout = bind_group_layout(&device);
        let scene_bind_groups = [
            bind_group(
                &device,
                "ground bindings",
                &bindings_layout,
                &texture_views[0],
                &scene_sampler,
            ),
            bind_group(
                &device,
                "obstacle bindings",
                &bindings_layout,
                &texture_views[1],
                &scene_sampler,
            ),
            bind_group(
                &device,
                "player bindings",
                &bindings_layout,
                &texture_views[2],
                &scene_sampler,
            ),
            bind_group(
                &device,
                "pickup bindings",
                &bindings_layout,
                &texture_views[3],
                &scene_sampler,
            ),
            bind_group(
                &device,
                "hazard bindings",
                &bindings_layout,
                &texture_views[4],
                &scene_sampler,
            ),
        ];
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Forge Run pipeline layout"),
            bind_group_layouts: &[Some(&bindings_layout)],
            immediate_size: 0,
        });
        let scene_pipeline =
            scene_pipeline(&device, &pipeline_layout, &shader, format, sample_count);
        let postprocess_pipeline = postprocess_pipeline(&device, &pipeline_layout, &shader, format);
        let targets = create_targets(&device, &config, sample_count);
        let postprocess_bind_group = bind_group(
            &device,
            "postprocess bindings",
            &bindings_layout,
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
            instance_buffer,
            meshes,
            scene_pipeline,
            scene_bind_groups,
            postprocess_pipeline,
            bindings_layout,
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
            &self.bindings_layout,
            &self.targets.scene_color,
            &self.postprocess_sampler,
        );
        self.configured = true;
    }

    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn render(&mut self, game: &Game, interpolation: f64) -> Result<(), Box<dyn Error>> {
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
        let transforms = scene::transforms(game, aspect, interpolation);
        let batches = transforms.batches();
        let mut bytes = Vec::with_capacity(usize::try_from(OBJECT_COUNT * 64).expect("fits usize"));
        let mut ranges = [(0_u64, 0_u32); 5];
        for (index, batch) in batches.iter().enumerate() {
            ranges[index] = (
                u64::try_from(bytes.len()).expect("instance offset fits u64"),
                u32::try_from(batch.len()).expect("batch count fits u32"),
            );
            bytes.extend_from_slice(bytemuck::cast_slice(batch));
        }
        self.queue.write_buffer(&self.instance_buffer, 0, &bytes);

        let frame_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Forge Run frame"),
            });
        self.encode_scene(&mut encoder, &ranges);
        self.encode_postprocess(&mut encoder, &frame_view);
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        Ok(())
    }

    fn encode_scene(&self, encoder: &mut wgpu::CommandEncoder, ranges: &[(u64, u32); 5]) {
        let (view, resolve_target, store) = match self.targets.multisample.as_ref() {
            Some(multisample) => (
                multisample,
                Some(&self.targets.scene_color),
                wgpu::StoreOp::Discard,
            ),
            None => (&self.targets.scene_color, None, wgpu::StoreOp::Store),
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Forge Run scene pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.008,
                        g: 0.014,
                        b: 0.026,
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
        for (batch, &(offset, count)) in ranges.iter().enumerate() {
            if count == 0 {
                continue;
            }
            let mesh = &self.meshes[usize::from(batch >= 3)];
            pass.set_bind_group(0, &self.scene_bind_groups[batch], &[]);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_vertex_buffer(1, self.instance_buffer.slice(offset..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..mesh.index_count, 0, 0..count);
        }
    }

    fn encode_postprocess(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        frame_view: &wgpu::TextureView,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Forge Run postprocess pass"),
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
        pass.set_bind_group(0, &self.postprocess_bind_group, &[]);
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
        label: Some("Forge Run bindings"),
        entries: &[
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
    texture: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
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
        label: Some("Forge Run scene pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("instanced_vertex"),
            compilation_options: Default::default(),
            buffers: &[
                Some(wgpu::VertexBufferLayout {
                    array_stride: 32,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,
                        1 => Float32x3,
                        2 => Float32x2,
                    ],
                }),
                Some(wgpu::VertexBufferLayout {
                    array_stride: INSTANCE_TRANSFORM_SIZE,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        3 => Float32x4,
                        4 => Float32x4,
                        5 => Float32x4,
                        6 => Float32x4,
                    ],
                }),
            ],
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
        label: Some("Forge Run postprocess pipeline"),
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
