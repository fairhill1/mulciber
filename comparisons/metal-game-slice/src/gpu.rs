//! Direct Metal resource, pipeline, synchronization, resize, and presentation plumbing.

use std::error::Error;
use std::mem;

use core_graphics_types::geometry::CGSize;
use metal::*;
use objc2_app_kit::NSView;
use objc2_quartz_core::CALayer;

use crate::{game::Game, scene};

const SHADER: &[u8] =
    include_bytes!("../../../examples/instanced-scene/artifacts/instanced.metal.shaderbin");
const SHADER_HEADER_BYTES: usize = 16;
const FORMAT: MTLPixelFormat = MTLPixelFormat::BGRA8Unorm_sRGB;
const DEPTH_FORMAT: MTLPixelFormat = MTLPixelFormat::Depth32Float;
const INSTANCE_BYTES: u64 = 64;
const OBJECT_COUNT: u64 = 26;

struct Targets {
    scene_color: Texture,
    depth: Texture,
    multisample: Option<Texture>,
}

struct Mesh {
    vertices: Buffer,
    indices: Buffer,
    index_count: u64,
}

pub(crate) struct Gpu {
    device: Device,
    queue: CommandQueue,
    layer: MetalLayer,
    width: u32,
    height: u32,
    sample_count: u64,
    instance_buffer: Buffer,
    meshes: [Mesh; 2],
    textures: [Texture; 5],
    scene_sampler: SamplerState,
    post_sampler: SamplerState,
    scene_pipeline: RenderPipelineState,
    post_pipeline: RenderPipelineState,
    depth_state: DepthStencilState,
    targets: Targets,
    previous_submission: Option<CommandBuffer>,
}

impl Gpu {
    pub(crate) fn new(view: &NSView, width: u32, height: u32) -> Result<Self, Box<dyn Error>> {
        let device = Device::system_default().ok_or("Metal device unavailable")?;
        let layer = MetalLayer::new();
        layer.set_device(&device);
        layer.set_pixel_format(FORMAT);
        layer.set_presents_with_transaction(false);
        layer.set_framebuffer_only(true);
        view.setWantsLayer(true);
        // metal-rs and objc2 wrap the same Objective-C CAMetalLayer object through different
        // binding families. The cast is confined to this AppKit attachment boundary.
        let cocoa_layer = unsafe { &*(layer.as_ref() as *const MetalLayerRef).cast::<CALayer>() };
        view.setLayer(Some(cocoa_layer));

        let sample_count = if device.supports_texture_sample_count(4) {
            4
        } else {
            1
        };
        let payload = SHADER
            .get(SHADER_HEADER_BYTES..)
            .filter(|payload| payload.starts_with(b"MTLB"))
            .ok_or("invalid embedded Metal shader artifact")?;
        let library = device
            .new_library_with_data(payload)
            .map_err(|error| -> Box<dyn Error> { error.into() })?;
        let scene_pipeline = scene_pipeline(&device, &library, sample_count)?;
        let post_pipeline = post_pipeline(&device, &library)?;

        let depth_descriptor = DepthStencilDescriptor::new();
        depth_descriptor.set_depth_compare_function(MTLCompareFunction::Less);
        depth_descriptor.set_depth_write_enabled(true);
        let depth_state = device.new_depth_stencil_state(&depth_descriptor);
        let meshes = [
            mesh(&device, "cube", &scene::CUBE_VERTICES, &scene::CUBE_INDICES),
            mesh(
                &device,
                "pyramid",
                &scene::PYRAMID_VERTICES,
                &scene::PYRAMID_INDICES,
            ),
        ];
        let instance_buffer = device.new_buffer(
            INSTANCE_BYTES * OBJECT_COUNT,
            MTLResourceOptions::StorageModeShared,
        );
        instance_buffer.set_label("Forge Run instance transforms");
        let textures = [
            texture(
                &device,
                "ground",
                &scene::checkerboard([30, 42, 58], [55, 72, 82], 1),
            ),
            texture(
                &device,
                "obstacles",
                &scene::checkerboard([100, 105, 120], [42, 48, 65], 2),
            ),
            texture(
                &device,
                "player",
                &scene::checkerboard([35, 230, 190], [20, 90, 145], 2),
            ),
            texture(
                &device,
                "pickups",
                &scene::checkerboard([255, 195, 45], [245, 80, 30], 1),
            ),
            texture(
                &device,
                "hazards",
                &scene::checkerboard([245, 45, 85], [100, 20, 135], 2),
            ),
        ];
        let scene_sampler = sampler(&device, MTLSamplerAddressMode::Repeat);
        let post_sampler = sampler(&device, MTLSamplerAddressMode::ClampToEdge);
        let width = width.max(1);
        let height = height.max(1);
        layer.set_drawable_size(CGSize::new(f64::from(width), f64::from(height)));
        let targets = targets(&device, width, height, sample_count);

        Ok(Self {
            queue: device.new_command_queue(),
            device,
            layer,
            width,
            height,
            sample_count,
            instance_buffer,
            meshes,
            textures,
            scene_sampler,
            post_sampler,
            scene_pipeline,
            post_pipeline,
            depth_state,
            targets,
            previous_submission: None,
        })
    }

    pub(crate) const fn sample_count(&self) -> u64 {
        self.sample_count
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) -> Result<(), Box<dyn Error>> {
        if width == 0 || height == 0 || (width == self.width && height == self.height) {
            return Ok(());
        }
        self.finish_previous()?;
        self.width = width;
        self.height = height;
        self.layer
            .set_drawable_size(CGSize::new(f64::from(width), f64::from(height)));
        self.targets = targets(&self.device, width, height, self.sample_count);
        Ok(())
    }

    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn render(&mut self, game: &Game, interpolation: f64) -> Result<(), Box<dyn Error>> {
        self.finish_previous()?;
        let Some(drawable) = self.layer.next_drawable() else {
            return Ok(());
        };
        let transforms =
            scene::transforms(game, self.width as f32 / self.height as f32, interpolation);
        let batches = transforms.batches();
        let mut ranges = [(0_u64, 0_u64); 5];
        let mut offset = 0_usize;
        for (index, batch) in batches.iter().enumerate() {
            let bytes = bytemuck::cast_slice(batch);
            ranges[index] = (
                u64::try_from(offset).expect("instance offset fits u64"),
                u64::try_from(batch.len()).expect("instance count fits u64"),
            );
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    self.instance_buffer.contents().cast::<u8>().add(offset),
                    bytes.len(),
                );
            }
            offset += bytes.len();
        }

        let command = self.queue.new_command_buffer().to_owned();
        command.set_label("Forge Run frame");
        self.encode_scene(&command, &ranges);
        self.encode_postprocess(&command, drawable.texture());
        command.present_drawable(drawable);
        command.commit();
        self.previous_submission = Some(command);
        Ok(())
    }

    pub(crate) fn shutdown(&mut self) -> Result<(), Box<dyn Error>> {
        self.finish_previous()
    }

    fn finish_previous(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(command) = self.previous_submission.take() {
            command.wait_until_completed();
            if command.status() != MTLCommandBufferStatus::Completed {
                return Err(
                    format!("Metal command failed with status {:?}", command.status()).into(),
                );
            }
        }
        Ok(())
    }

    fn encode_scene(&self, command: &CommandBufferRef, ranges: &[(u64, u64); 5]) {
        let pass = RenderPassDescriptor::new();
        let color = pass.color_attachments().object_at(0).expect("color zero");
        if let Some(multisample) = self.targets.multisample.as_ref() {
            color.set_texture(Some(multisample));
            color.set_resolve_texture(Some(&self.targets.scene_color));
            color.set_store_action(MTLStoreAction::MultisampleResolve);
        } else {
            color.set_texture(Some(&self.targets.scene_color));
            color.set_store_action(MTLStoreAction::Store);
        }
        color.set_load_action(MTLLoadAction::Clear);
        color.set_clear_color(MTLClearColor::new(0.008, 0.014, 0.026, 1.0));
        let depth = pass.depth_attachment().expect("depth attachment");
        depth.set_texture(Some(&self.targets.depth));
        depth.set_load_action(MTLLoadAction::Clear);
        depth.set_store_action(MTLStoreAction::DontCare);
        depth.set_clear_depth(1.0);

        let encoder = command.new_render_command_encoder(pass);
        encoder.set_label("Forge Run scene pass");
        encoder.set_render_pipeline_state(&self.scene_pipeline);
        encoder.set_depth_stencil_state(&self.depth_state);
        for (batch, &(offset, count)) in ranges.iter().enumerate() {
            if count == 0 {
                continue;
            }
            let mesh = &self.meshes[usize::from(batch >= 3)];
            encoder.set_vertex_buffer(1, Some(&mesh.vertices), 0);
            encoder.set_vertex_buffer(2, Some(&self.instance_buffer), offset);
            encoder.set_fragment_texture(1, Some(&self.textures[batch]));
            encoder.set_fragment_sampler_state(2, Some(&self.scene_sampler));
            encoder.draw_indexed_primitives_instanced(
                MTLPrimitiveType::Triangle,
                mesh.index_count,
                MTLIndexType::UInt16,
                &mesh.indices,
                0,
                count,
            );
        }
        encoder.end_encoding();
    }

    fn encode_postprocess(&self, command: &CommandBufferRef, drawable: &TextureRef) {
        let pass = RenderPassDescriptor::new();
        let color = pass.color_attachments().object_at(0).expect("color zero");
        color.set_texture(Some(drawable));
        color.set_load_action(MTLLoadAction::Clear);
        color.set_store_action(MTLStoreAction::Store);
        color.set_clear_color(MTLClearColor::new(0.0, 0.0, 0.0, 1.0));
        let encoder = command.new_render_command_encoder(pass);
        encoder.set_label("Forge Run postprocess pass");
        encoder.set_render_pipeline_state(&self.post_pipeline);
        encoder.set_fragment_texture(1, Some(&self.targets.scene_color));
        encoder.set_fragment_sampler_state(2, Some(&self.post_sampler));
        encoder.draw_primitives(MTLPrimitiveType::Triangle, 0, 3);
        encoder.end_encoding();
    }
}

fn mesh(device: &DeviceRef, label: &str, vertices: &[scene::Vertex], indices: &[u16]) -> Mesh {
    let vertices = device.new_buffer_with_data(
        vertices.as_ptr().cast(),
        u64::try_from(mem::size_of_val(vertices)).expect("vertex bytes fit u64"),
        MTLResourceOptions::StorageModeShared,
    );
    vertices.set_label(&format!("{label} vertices"));
    let indices_buffer = device.new_buffer_with_data(
        indices.as_ptr().cast(),
        u64::try_from(mem::size_of_val(indices)).expect("index bytes fit u64"),
        MTLResourceOptions::StorageModeShared,
    );
    indices_buffer.set_label(&format!("{label} indices"));
    Mesh {
        vertices,
        indices: indices_buffer,
        index_count: u64::try_from(indices.len()).expect("index count fits u64"),
    }
}

fn texture(device: &DeviceRef, label: &str, texels: &[u8]) -> Texture {
    let descriptor = TextureDescriptor::new();
    descriptor.set_texture_type(MTLTextureType::D2);
    descriptor.set_pixel_format(MTLPixelFormat::RGBA8Unorm_sRGB);
    descriptor.set_width(8);
    descriptor.set_height(8);
    descriptor.set_storage_mode(MTLStorageMode::Shared);
    descriptor.set_usage(MTLTextureUsage::ShaderRead);
    let texture = device.new_texture(&descriptor);
    texture.set_label(label);
    texture.replace_region(MTLRegion::new_2d(0, 0, 8, 8), 0, texels.as_ptr().cast(), 32);
    texture
}

fn sampler(device: &DeviceRef, address: MTLSamplerAddressMode) -> SamplerState {
    let descriptor = SamplerDescriptor::new();
    descriptor.set_min_filter(MTLSamplerMinMagFilter::Linear);
    descriptor.set_mag_filter(MTLSamplerMinMagFilter::Linear);
    descriptor.set_address_mode_s(address);
    descriptor.set_address_mode_t(address);
    device.new_sampler(&descriptor)
}

fn scene_pipeline(
    device: &DeviceRef,
    library: &LibraryRef,
    sample_count: u64,
) -> Result<RenderPipelineState, Box<dyn Error>> {
    let descriptor = RenderPipelineDescriptor::new();
    descriptor.set_label("Forge Run scene pipeline");
    let vertex_function = library.get_function("instanced_vertex", None)?;
    let fragment_function = library.get_function("cube_fragment", None)?;
    descriptor.set_vertex_function(Some(&vertex_function));
    descriptor.set_fragment_function(Some(&fragment_function));
    descriptor.set_sample_count(sample_count);
    descriptor.set_depth_attachment_pixel_format(DEPTH_FORMAT);
    descriptor
        .color_attachments()
        .object_at(0)
        .expect("color zero")
        .set_pixel_format(FORMAT);

    let vertex = VertexDescriptor::new();
    for (index, format, offset) in [
        (0, MTLVertexFormat::Float3, 0),
        (1, MTLVertexFormat::Float3, 12),
        (2, MTLVertexFormat::Float2, 24),
    ] {
        let attribute = vertex
            .attributes()
            .object_at(index)
            .expect("vertex attribute");
        attribute.set_format(format);
        attribute.set_offset(offset);
        attribute.set_buffer_index(1);
    }
    vertex
        .layouts()
        .object_at(1)
        .expect("vertex layout")
        .set_stride(32);
    for (index, offset) in [(3, 0), (4, 16), (5, 32), (6, 48)] {
        let attribute = vertex
            .attributes()
            .object_at(index)
            .expect("instance attribute");
        attribute.set_format(MTLVertexFormat::Float4);
        attribute.set_offset(offset);
        attribute.set_buffer_index(2);
    }
    let instances = vertex.layouts().object_at(2).expect("instance layout");
    instances.set_stride(INSTANCE_BYTES);
    instances.set_step_function(MTLVertexStepFunction::PerInstance);
    descriptor.set_vertex_descriptor(Some(vertex));
    Ok(device.new_render_pipeline_state(&descriptor)?)
}

fn post_pipeline(
    device: &DeviceRef,
    library: &LibraryRef,
) -> Result<RenderPipelineState, Box<dyn Error>> {
    let descriptor = RenderPipelineDescriptor::new();
    descriptor.set_label("Forge Run postprocess pipeline");
    let vertex_function = library.get_function("post_vertex", None)?;
    let fragment_function = library.get_function("post_fragment", None)?;
    descriptor.set_vertex_function(Some(&vertex_function));
    descriptor.set_fragment_function(Some(&fragment_function));
    descriptor
        .color_attachments()
        .object_at(0)
        .expect("color zero")
        .set_pixel_format(FORMAT);
    Ok(device.new_render_pipeline_state(&descriptor)?)
}

fn targets(device: &DeviceRef, width: u32, height: u32, samples: u64) -> Targets {
    let texture = |label, format, sample_count, usage| {
        let descriptor = TextureDescriptor::new();
        descriptor.set_texture_type(if sample_count > 1 {
            MTLTextureType::D2Multisample
        } else {
            MTLTextureType::D2
        });
        descriptor.set_pixel_format(format);
        descriptor.set_width(u64::from(width));
        descriptor.set_height(u64::from(height));
        descriptor.set_sample_count(sample_count);
        descriptor.set_storage_mode(if sample_count > 1 {
            MTLStorageMode::Memoryless
        } else {
            MTLStorageMode::Private
        });
        descriptor.set_usage(usage);
        let texture = device.new_texture(&descriptor);
        texture.set_label(label);
        texture
    };
    Targets {
        scene_color: texture(
            "resolved scene color",
            FORMAT,
            1,
            MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
        ),
        depth: texture(
            "scene depth",
            DEPTH_FORMAT,
            samples,
            MTLTextureUsage::RenderTarget,
        ),
        multisample: (samples > 1).then(|| {
            texture(
                "multisample scene color",
                FORMAT,
                samples,
                MTLTextureUsage::RenderTarget,
            )
        }),
    }
}
