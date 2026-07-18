use core::ffi::c_void;
use core::{mem, ptr};
use std::format;
use std::vec::Vec;

use mulciber_platform::{SurfaceTarget, WindowMetrics};

use super::{ClearSurface, MetalFrameToken, objc, required};
use crate::resource::{Arena, DestroyRequest, ResourceId, ResourceKind};
use crate::{
    ClearColor, DeviceRequest, FrameAcquire, FrameDisposition, GraphicsError, SampleCount,
    ShaderArtifact, SurfaceInfo, TexturedInstanceBatch, TexturedSceneDraw, Vertex,
};

use objc::{Object, Origin3, Region3, Size3};

const PIXEL_FORMAT_BGRA8_UNORM_SRGB: usize = 81;
const PIXEL_FORMAT_RGBA8_UNORM_SRGB: usize = 71;
const PIXEL_FORMAT_DEPTH32_FLOAT: usize = 252;
const VERTEX_FORMAT_FLOAT2: usize = 29;
const VERTEX_FORMAT_FLOAT3: usize = 30;
const VERTEX_FORMAT_FLOAT4: usize = 31;
const VERTEX_STEP_FUNCTION_PER_INSTANCE: usize = 2;
const LOAD_ACTION_CLEAR: usize = 2;
const STORE_ACTION_STORE: usize = 1;
const STORE_ACTION_DONT_CARE: usize = 0;
const STORE_ACTION_MULTISAMPLE_RESOLVE: usize = 2;
const PRIMITIVE_TYPE_TRIANGLE: usize = 3;
const INDEX_TYPE_UINT16: usize = 0;
const STORAGE_MODE_PRIVATE: usize = 2;
const STORAGE_MODE_MEMORYLESS: usize = 3;
const TEXTURE_TYPE_2D_MULTISAMPLE: usize = 4;
const TEXTURE_USAGE_SHADER_READ: usize = 1;
const TEXTURE_USAGE_RENDER_TARGET: usize = 4;
const COMPARE_FUNCTION_LESS: usize = 1;
const SAMPLER_FILTER_LINEAR: usize = 1;
const SAMPLER_ADDRESS_REPEAT: usize = 2;
const DRAW_UNIFORM_SIZE: usize = 64;
const DRAW_UNIFORM_STRIDE: usize = 256;
const INSTANCE_TRANSFORM_SIZE: usize = 64;

#[link(name = "System")]
unsafe extern "C" {
    fn dispatch_data_create(
        buffer: *const c_void,
        size: usize,
        queue: Object,
        destructor: Object,
    ) -> Object;
}

struct MeshResource {
    vertices: Object,
    indices: Object,
    indirect: Object,
    index_count: u32,
}

struct TextureResource {
    texture: Object,
    sampler: Object,
}

struct PipelineResource {
    pipeline: Object,
    depth_state: Object,
}

struct PostprocessPipelineResource {
    pipeline: Object,
    sampler: Object,
}

struct TargetResource {
    info: SurfaceInfo,
    multisample_color: Object,
    depth: Object,
}

struct PostprocessTargetResource {
    info: SurfaceInfo,
    scene_color: Object,
    multisample_color: Object,
    depth: Object,
}

#[derive(Clone, Copy)]
struct ResolvedInstanceBatch {
    mesh: usize,
    texture: usize,
    pipeline: usize,
    transform_offset: usize,
    instance_count: usize,
}

#[derive(Clone, Copy)]
enum PreparedScene<'resources> {
    Draws(&'resources [TexturedSceneDraw<'resources>]),
    Instances,
}

pub(crate) struct TexturedFrameToken(MetalFrameToken);

impl TexturedFrameToken {
    pub(crate) const fn info(&self) -> SurfaceInfo {
        self.0.info
    }
}

pub(crate) struct TexturedSession<'window> {
    surface: ClearSurface<'window>,
    sample_count: usize,
    uniform: Object,
    uniform_capacity: usize,
    instance_transforms: Object,
    instance_capacity: usize,
    resolved_instance_batches: Vec<ResolvedInstanceBatch>,
    meshes: Arena<MeshResource>,
    textures: Arena<TextureResource>,
    pipelines: Arena<PipelineResource>,
    instanced_pipelines: Arena<PipelineResource>,
    postprocess_pipelines: Arena<PostprocessPipelineResource>,
    targets: Arena<TargetResource>,
    postprocess_targets: Arena<PostprocessTargetResource>,
}

impl<'window> TexturedSession<'window> {
    pub(crate) fn new(
        target: SurfaceTarget<'window>,
        metrics: WindowMetrics,
        request: DeviceRequest,
    ) -> Result<(Self, SampleCount), GraphicsError> {
        let surface = ClearSurface::new(target, metrics)?;
        let sample_count = if request.preferred_sample_count == SampleCount::Four
            && unsafe { objc::bool_usize(surface.device, c"supportsTextureSampleCount:", 4) }
        {
            4
        } else {
            1
        };
        let uniform = unsafe {
            required(
                objc::object_two_usizes(
                    surface.device,
                    c"newBufferWithLength:options:",
                    DRAW_UNIFORM_STRIDE,
                    0,
                ),
                "Metal cube uniform buffer",
            )?
        };
        let instance_transforms = match unsafe {
            required(
                objc::object_two_usizes(
                    surface.device,
                    c"newBufferWithLength:options:",
                    INSTANCE_TRANSFORM_SIZE,
                    0,
                ),
                "Metal instance transform buffer",
            )
        } {
            Ok(buffer) => buffer,
            Err(failure) => {
                unsafe { objc::void(uniform, c"release") };
                return Err(failure);
            }
        };
        Ok((
            Self {
                surface,
                sample_count,
                uniform,
                uniform_capacity: 1,
                instance_transforms,
                instance_capacity: 1,
                resolved_instance_batches: Vec::new(),
                meshes: Arena::new("mesh"),
                textures: Arena::new("texture"),
                pipelines: Arena::new("textured pipeline"),
                instanced_pipelines: Arena::new("instanced textured pipeline"),
                postprocess_pipelines: Arena::new("postprocess pipeline"),
                targets: Arena::new("render targets"),
                postprocess_targets: Arena::new("postprocess targets"),
            },
            if sample_count == 4 {
                SampleCount::Four
            } else {
                SampleCount::One
            },
        ))
    }

    pub(crate) const fn info(&self) -> SurfaceInfo {
        self.surface.info()
    }

    pub(crate) fn acquire(
        &mut self,
        metrics: WindowMetrics,
    ) -> Result<FrameAcquire<TexturedFrameToken>, GraphicsError> {
        let acquisition = self.surface.acquire_drawable(metrics)?;
        self.reclaim_stale_targets();
        Ok(acquisition.map_ready(TexturedFrameToken))
    }

    pub(crate) fn create_mesh(
        &mut self,
        vertices: &[Vertex],
        indices: &[u16],
    ) -> Result<ResourceId, GraphicsError> {
        let draw = IndexedIndirectArguments {
            index_count: u32::try_from(indices.len())
                .map_err(|_| GraphicsError::new("mesh index count exceeds u32"))?,
            instance_count: 1,
            index_start: 0,
            base_vertex: 0,
            base_instance: 0,
        };
        unsafe {
            let vertices = required(
                objc::object_bytes(
                    self.surface.device,
                    c"newBufferWithBytes:length:options:",
                    vertices.as_ptr().cast(),
                    mem::size_of_val(vertices),
                    0,
                ),
                "Metal cube vertex buffer",
            )?;
            let indices = required(
                objc::object_bytes(
                    self.surface.device,
                    c"newBufferWithBytes:length:options:",
                    indices.as_ptr().cast(),
                    mem::size_of_val(indices),
                    0,
                ),
                "Metal cube index buffer",
            )?;
            let indirect = required(
                objc::object_bytes(
                    self.surface.device,
                    c"newBufferWithBytes:length:options:",
                    ptr::from_ref(&draw).cast(),
                    mem::size_of_val(&draw),
                    0,
                ),
                "Metal cube indirect buffer",
            )?;
            self.meshes.insert(MeshResource {
                vertices,
                indices,
                indirect,
                index_count: draw.index_count,
            })
        }
    }

    pub(crate) fn create_texture(
        &mut self,
        width: u32,
        height: u32,
        texels: &[u8],
    ) -> Result<ResourceId, GraphicsError> {
        let width = usize::try_from(width)
            .map_err(|_| GraphicsError::new("texture width exceeds usize"))?;
        let height = usize::try_from(height)
            .map_err(|_| GraphicsError::new("texture height exceeds usize"))?;
        unsafe {
            let descriptor = required(
                objc::object_three_usizes_bool(
                    objc::class(c"MTLTextureDescriptor"),
                    c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
                    PIXEL_FORMAT_RGBA8_UNORM_SRGB,
                    width,
                    height,
                    false,
                ),
                "Metal cube texture descriptor",
            )?;
            objc::void_usize(descriptor, c"setUsage:", TEXTURE_USAGE_SHADER_READ);
            let texture = required(
                objc::object_object(
                    self.surface.device,
                    c"newTextureWithDescriptor:",
                    descriptor,
                ),
                "Metal cube texture",
            )?;
            objc::void_region_usize_bytes_usize(
                texture,
                c"replaceRegion:mipmapLevel:withBytes:bytesPerRow:",
                Region3 {
                    origin: Origin3 { x: 0, y: 0, z: 0 },
                    size: Size3 {
                        width,
                        height,
                        depth: 1,
                    },
                },
                0,
                texels.as_ptr().cast(),
                width * 4,
            );
            let sampler_descriptor = required(
                objc::object(objc::class(c"MTLSamplerDescriptor"), c"new"),
                "Metal sampler descriptor",
            )?;
            objc::void_usize(sampler_descriptor, c"setMinFilter:", SAMPLER_FILTER_LINEAR);
            objc::void_usize(sampler_descriptor, c"setMagFilter:", SAMPLER_FILTER_LINEAR);
            objc::void_usize(
                sampler_descriptor,
                c"setSAddressMode:",
                SAMPLER_ADDRESS_REPEAT,
            );
            objc::void_usize(
                sampler_descriptor,
                c"setTAddressMode:",
                SAMPLER_ADDRESS_REPEAT,
            );
            let sampler = required(
                objc::object_object(
                    self.surface.device,
                    c"newSamplerStateWithDescriptor:",
                    sampler_descriptor,
                ),
                "Metal cube sampler",
            )?;
            objc::void(sampler_descriptor, c"release");
            self.textures.insert(TextureResource { texture, sampler })
        }
    }

    pub(crate) fn create_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        self.pipelines.insert(create_pipeline(
            self.surface.device,
            shader.payload(),
            self.sample_count,
            false,
        )?)
    }

    pub(crate) fn create_instanced_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        self.instanced_pipelines.insert(create_pipeline(
            self.surface.device,
            shader.payload(),
            self.sample_count,
            true,
        )?)
    }

    pub(crate) fn create_postprocess_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        self.postprocess_pipelines
            .insert(create_postprocess_pipeline(
                self.surface.device,
                shader.payload(),
            )?)
    }

    /// Releases the session's references to render targets from superseded surface generations.
    ///
    /// Draws reject targets that do not match the acquired generation, and committed command
    /// buffers retain the textures they reference, so releasing the session's reference cannot
    /// free storage still owned by in-flight GPU work. Reclaimed entries keep their identifiers
    /// with null textures and are rejected if drawn.
    fn reclaim_stale_targets(&mut self) {
        let current = self.surface.info().generation();
        for target in self.targets.iter_mut() {
            if target.info.generation().get() >= current.get() || target.depth.is_null() {
                continue;
            }
            unsafe {
                if !target.multisample_color.is_null() {
                    objc::void(target.multisample_color, c"release");
                }
                objc::void(target.depth, c"release");
            }
            target.multisample_color = ptr::null_mut();
            target.depth = ptr::null_mut();
        }
        for target in self.postprocess_targets.iter_mut() {
            if target.info.generation().get() >= current.get() || target.depth.is_null() {
                continue;
            }
            unsafe {
                if !target.multisample_color.is_null() {
                    objc::void(target.multisample_color, c"release");
                }
                objc::void(target.scene_color, c"release");
                objc::void(target.depth, c"release");
            }
            target.scene_color = ptr::null_mut();
            target.multisample_color = ptr::null_mut();
            target.depth = ptr::null_mut();
        }
    }

    pub(crate) fn create_render_targets(
        &mut self,
        info: SurfaceInfo,
    ) -> Result<ResourceId, GraphicsError> {
        self.reclaim_stale_targets();
        let width = usize::try_from(info.extent().width())
            .map_err(|_| GraphicsError::new("target width exceeds usize"))?;
        let height = usize::try_from(info.extent().height())
            .map_err(|_| GraphicsError::new("target height exceeds usize"))?;
        let depth = create_target_texture(
            self.surface.device,
            PIXEL_FORMAT_DEPTH32_FLOAT,
            width,
            height,
            self.sample_count,
            TEXTURE_USAGE_RENDER_TARGET,
        )?;
        let multisample_color = if self.sample_count == 4 {
            match create_target_texture(
                self.surface.device,
                PIXEL_FORMAT_BGRA8_UNORM_SRGB,
                width,
                height,
                4,
                TEXTURE_USAGE_RENDER_TARGET,
            ) {
                Ok(color) => color,
                Err(failure) => {
                    unsafe { objc::void(depth, c"release") };
                    return Err(failure);
                }
            }
        } else {
            ptr::null_mut()
        };
        self.targets.insert(TargetResource {
            info,
            multisample_color,
            depth,
        })
    }

    pub(crate) fn create_postprocess_targets(
        &mut self,
        info: SurfaceInfo,
    ) -> Result<ResourceId, GraphicsError> {
        self.reclaim_stale_targets();
        let width = usize::try_from(info.extent().width())
            .map_err(|_| GraphicsError::new("target width exceeds usize"))?;
        let height = usize::try_from(info.extent().height())
            .map_err(|_| GraphicsError::new("target height exceeds usize"))?;
        let scene_color = create_target_texture(
            self.surface.device,
            PIXEL_FORMAT_BGRA8_UNORM_SRGB,
            width,
            height,
            1,
            TEXTURE_USAGE_RENDER_TARGET | TEXTURE_USAGE_SHADER_READ,
        )?;
        let depth = match create_target_texture(
            self.surface.device,
            PIXEL_FORMAT_DEPTH32_FLOAT,
            width,
            height,
            self.sample_count,
            TEXTURE_USAGE_RENDER_TARGET,
        ) {
            Ok(depth) => depth,
            Err(failure) => {
                unsafe { objc::void(scene_color, c"release") };
                return Err(failure);
            }
        };
        let multisample_color = if self.sample_count == 4 {
            match create_target_texture(
                self.surface.device,
                PIXEL_FORMAT_BGRA8_UNORM_SRGB,
                width,
                height,
                4,
                TEXTURE_USAGE_RENDER_TARGET,
            ) {
                Ok(color) => color,
                Err(failure) => {
                    unsafe {
                        objc::void(depth, c"release");
                        objc::void(scene_color, c"release");
                    }
                    return Err(failure);
                }
            }
        } else {
            ptr::null_mut()
        };
        self.postprocess_targets.insert(PostprocessTargetResource {
            info,
            scene_color,
            multisample_color,
            depth,
        })
    }

    pub(crate) fn draw_scene_and_present(
        &mut self,
        token: TexturedFrameToken,
        draws: &[TexturedSceneDraw<'_>],
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let target = self.targets.index_of(targets)?;
        if self.targets[target].info != token.info() {
            return Err(GraphicsError::new(
                "render targets do not match acquired Metal generation",
            ));
        }
        if self.targets[target].depth.is_null() {
            return Err(GraphicsError::new(
                "render targets were reclaimed by a newer surface generation",
            ));
        }
        self.prepare_scene(draws)?;
        self.encode_present(token, PreparedScene::Draws(draws), target, clear)
    }

    pub(crate) fn draw_scene_postprocessed_and_present(
        &mut self,
        token: TexturedFrameToken,
        draws: &[TexturedSceneDraw<'_>],
        postprocess_pipeline: ResourceId,
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let postprocess_pipeline = self.postprocess_pipelines.index_of(postprocess_pipeline)?;
        let target = self.postprocess_targets.index_of(targets)?;
        if self.postprocess_targets[target].info != token.info() {
            return Err(GraphicsError::new(
                "postprocess targets do not match acquired Metal generation",
            ));
        }
        if self.postprocess_targets[target].scene_color.is_null() {
            return Err(GraphicsError::new(
                "postprocess targets were reclaimed by a newer surface generation",
            ));
        }
        self.prepare_scene(draws)?;
        self.encode_postprocessed_present(
            token,
            PreparedScene::Draws(draws),
            postprocess_pipeline,
            target,
            clear,
        )
    }

    pub(crate) fn draw_instanced_scene_and_present(
        &mut self,
        token: TexturedFrameToken,
        batches: &[TexturedInstanceBatch<'_>],
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let target = self.targets.index_of(targets)?;
        if self.targets[target].info != token.info() {
            return Err(GraphicsError::new(
                "render targets do not match acquired Metal generation",
            ));
        }
        if self.targets[target].depth.is_null() {
            return Err(GraphicsError::new(
                "render targets were reclaimed by a newer surface generation",
            ));
        }
        self.prepare_instanced_scene(batches)?;
        self.encode_present(token, PreparedScene::Instances, target, clear)
    }

    pub(crate) fn draw_instanced_scene_postprocessed_and_present(
        &mut self,
        token: TexturedFrameToken,
        batches: &[TexturedInstanceBatch<'_>],
        postprocess_pipeline: ResourceId,
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let postprocess_pipeline = self.postprocess_pipelines.index_of(postprocess_pipeline)?;
        let target = self.postprocess_targets.index_of(targets)?;
        if self.postprocess_targets[target].info != token.info() {
            return Err(GraphicsError::new(
                "postprocess targets do not match acquired Metal generation",
            ));
        }
        if self.postprocess_targets[target].scene_color.is_null() {
            return Err(GraphicsError::new(
                "postprocess targets were reclaimed by a newer surface generation",
            ));
        }
        self.prepare_instanced_scene(batches)?;
        self.encode_postprocessed_present(
            token,
            PreparedScene::Instances,
            postprocess_pipeline,
            target,
            clear,
        )
    }

    fn prepare_scene(&mut self, draws: &[TexturedSceneDraw<'_>]) -> Result<(), GraphicsError> {
        for draw in draws {
            self.meshes.get(draw.mesh.id())?;
            self.textures.get(draw.texture.id())?;
            self.pipelines.get(draw.pipeline.id())?;
        }
        if draws.len() > self.uniform_capacity {
            let capacity = draws
                .len()
                .checked_next_power_of_two()
                .ok_or_else(|| GraphicsError::new("Metal scene transform capacity overflow"))?;
            let bytes = capacity
                .checked_mul(DRAW_UNIFORM_STRIDE)
                .ok_or_else(|| GraphicsError::new("Metal scene transform storage is too large"))?;
            let replacement = unsafe {
                required(
                    objc::object_two_usizes(
                        self.surface.device,
                        c"newBufferWithLength:options:",
                        bytes,
                        0,
                    ),
                    "Metal scene transform buffer",
                )?
            };
            unsafe { objc::void(self.uniform, c"release") };
            self.uniform = replacement;
            self.uniform_capacity = capacity;
        }
        unsafe {
            let contents = objc::pointer_value(self.uniform, c"contents");
            if contents.is_null() {
                return Err(GraphicsError::new(
                    "Metal uniform buffer has no CPU address",
                ));
            }
            for (index, draw) in draws.iter().enumerate() {
                ptr::copy_nonoverlapping(
                    ptr::from_ref(&draw.model_view_projection).cast::<u8>(),
                    contents.cast::<u8>().add(index * DRAW_UNIFORM_STRIDE),
                    DRAW_UNIFORM_SIZE,
                );
            }
        }
        Ok(())
    }

    fn prepare_instanced_scene(
        &mut self,
        batches: &[TexturedInstanceBatch<'_>],
    ) -> Result<(), GraphicsError> {
        let instance_count = batches.iter().try_fold(0_usize, |total, batch| {
            total
                .checked_add(batch.model_view_projections.len())
                .ok_or_else(|| GraphicsError::new("Metal instance count exceeds address space"))
        })?;
        if instance_count > self.instance_capacity {
            let capacity = instance_count
                .checked_next_power_of_two()
                .ok_or_else(|| GraphicsError::new("Metal instance capacity overflow"))?;
            let bytes = capacity
                .checked_mul(INSTANCE_TRANSFORM_SIZE)
                .ok_or_else(|| GraphicsError::new("Metal instance storage is too large"))?;
            let replacement = unsafe {
                required(
                    objc::object_two_usizes(
                        self.surface.device,
                        c"newBufferWithLength:options:",
                        bytes,
                        0,
                    ),
                    "Metal instance transform buffer",
                )?
            };
            unsafe { objc::void(self.instance_transforms, c"release") };
            self.instance_transforms = replacement;
            self.instance_capacity = capacity;
        }
        self.resolved_instance_batches.clear();
        let mut transform_offset = 0_usize;
        for batch in batches {
            let mesh = self.meshes.index_of(batch.mesh.id())?;
            let texture = self.textures.index_of(batch.texture.id())?;
            let pipeline = self.instanced_pipelines.index_of(batch.pipeline.id())?;
            let instance_count = batch.model_view_projections.len();
            self.resolved_instance_batches.push(ResolvedInstanceBatch {
                mesh,
                texture,
                pipeline,
                transform_offset,
                instance_count,
            });
            transform_offset = transform_offset
                .checked_add(instance_count * INSTANCE_TRANSFORM_SIZE)
                .ok_or_else(|| GraphicsError::new("Metal instance offsets overflow"))?;
        }
        unsafe {
            let contents = objc::pointer_value(self.instance_transforms, c"contents");
            if contents.is_null() {
                return Err(GraphicsError::new(
                    "Metal instance transform buffer has no CPU address",
                ));
            }
            let mut offset = 0_usize;
            for batch in batches {
                let bytes = mem::size_of_val(batch.model_view_projections);
                ptr::copy_nonoverlapping(
                    batch.model_view_projections.as_ptr().cast::<u8>(),
                    contents.cast::<u8>().add(offset),
                    bytes,
                );
                offset += bytes;
            }
        }
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    pub(crate) fn abandon(
        &mut self,
        token: TexturedFrameToken,
    ) -> Result<FrameDisposition, GraphicsError> {
        let generation = token.info().generation();
        drop(token);
        Ok(FrameDisposition::Abandoned(generation))
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn defer_abandon(&mut self, token: TexturedFrameToken) {
        // Metal abandonment is an inline drawable release at the token's autorelease boundary.
        // The token must NOT be stored for a later flush: its pool would then outlive the
        // enclosing AppKit autorelease scope, which is a fatal pool-nesting violation.
        drop(token);
    }

    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn reclaim_resources(
        &mut self,
        requests: &[DestroyRequest],
    ) -> Result<(), GraphicsError> {
        for &request in requests {
            self.destroy_resource_if_live(request);
        }
        Ok(())
    }

    pub(crate) fn destroy_resource(
        &mut self,
        request: DestroyRequest,
    ) -> Result<(), GraphicsError> {
        match request.kind {
            ResourceKind::Mesh => release_mesh(self.meshes.remove(request.id)?),
            ResourceKind::Texture => release_texture(self.textures.remove(request.id)?),
            ResourceKind::TexturedPipeline => release_pipeline(self.pipelines.remove(request.id)?),
            ResourceKind::InstancedTexturedPipeline => {
                release_pipeline(self.instanced_pipelines.remove(request.id)?);
            }
            ResourceKind::PostprocessPipeline => {
                release_postprocess_pipeline(self.postprocess_pipelines.remove(request.id)?);
            }
            ResourceKind::RenderTargets => release_target(self.targets.remove(request.id)?),
            ResourceKind::PostprocessTargets => {
                release_postprocess_target(self.postprocess_targets.remove(request.id)?);
            }
        }
        Ok(())
    }

    fn destroy_resource_if_live(&mut self, request: DestroyRequest) {
        match request.kind {
            ResourceKind::Mesh => self.meshes.remove_if_live(request.id).map(release_mesh),
            ResourceKind::Texture => self
                .textures
                .remove_if_live(request.id)
                .map(release_texture),
            ResourceKind::TexturedPipeline => self
                .pipelines
                .remove_if_live(request.id)
                .map(release_pipeline),
            ResourceKind::InstancedTexturedPipeline => self
                .instanced_pipelines
                .remove_if_live(request.id)
                .map(release_pipeline),
            ResourceKind::PostprocessPipeline => self
                .postprocess_pipelines
                .remove_if_live(request.id)
                .map(release_postprocess_pipeline),
            ResourceKind::RenderTargets => {
                self.targets.remove_if_live(request.id).map(release_target)
            }
            ResourceKind::PostprocessTargets => self
                .postprocess_targets
                .remove_if_live(request.id)
                .map(release_postprocess_target),
        };
    }

    pub(crate) fn shutdown(mut self) -> Result<(), GraphicsError> {
        let result = self.surface.finish_last_submission();
        self.destroy_resources();
        let surface = unsafe { ptr::read(&raw const self.surface) };
        mem::forget(self);
        result.and(surface.shutdown())
    }

    #[allow(clippy::too_many_lines)]
    fn encode_present(
        &mut self,
        mut token: TexturedFrameToken,
        scene: PreparedScene<'_>,
        target: usize,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        unsafe {
            let drawable = token.0.drawable;
            let drawable_texture = required(
                objc::object(drawable, c"texture"),
                "Metal cube drawable texture",
            )?;
            let pass = required(
                objc::object(
                    objc::class(c"MTLRenderPassDescriptor"),
                    c"renderPassDescriptor",
                ),
                "Metal cube render-pass descriptor",
            )?;
            let attachments =
                required(objc::object(pass, c"colorAttachments"), "color attachments")?;
            let color = required(
                objc::object_usize(attachments, c"objectAtIndexedSubscript:", 0),
                "color attachment zero",
            )?;
            let targets = &self.targets[target];
            if self.sample_count == 4 {
                objc::void_object(color, c"setTexture:", targets.multisample_color);
                objc::void_object(color, c"setResolveTexture:", drawable_texture);
                objc::void_usize(color, c"setStoreAction:", STORE_ACTION_MULTISAMPLE_RESOLVE);
            } else {
                objc::void_object(color, c"setTexture:", drawable_texture);
                objc::void_usize(color, c"setStoreAction:", STORE_ACTION_STORE);
            }
            objc::void_usize(color, c"setLoadAction:", LOAD_ACTION_CLEAR);
            let [red, green, blue, alpha] = clear.components();
            objc::void_clear_color(
                color,
                c"setClearColor:",
                objc::ClearColor {
                    red: f64::from(red),
                    green: f64::from(green),
                    blue: f64::from(blue),
                    alpha: f64::from(alpha),
                },
            );
            let depth = required(objc::object(pass, c"depthAttachment"), "depth attachment")?;
            objc::void_object(depth, c"setTexture:", targets.depth);
            objc::void_usize(depth, c"setLoadAction:", LOAD_ACTION_CLEAR);
            objc::void_usize(depth, c"setStoreAction:", STORE_ACTION_DONT_CARE);
            objc::void_f64(depth, c"setClearDepth:", 1.0);
            let command = required(
                objc::object(self.surface.queue, c"commandBuffer"),
                "Metal cube command buffer",
            )?;
            let encoder = required(
                objc::object_object(command, c"renderCommandEncoderWithDescriptor:", pass),
                "Metal cube render encoder",
            )?;
            self.encode_prepared_scene(encoder, scene)?;
            objc::void(encoder, c"endEncoding");
            objc::void_object(command, c"presentDrawable:", drawable);
            objc::void(command, c"retain");
            objc::void(command, c"commit");
            self.surface.last_command_buffer = command;
            token.0.drawable = ptr::null_mut();
        }
        Ok(FrameDisposition::Presented(token.info().generation()))
    }

    #[allow(clippy::too_many_lines)]
    fn encode_postprocessed_present(
        &mut self,
        mut token: TexturedFrameToken,
        scene: PreparedScene<'_>,
        postprocess_pipeline: usize,
        target: usize,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        unsafe {
            let drawable = token.0.drawable;
            let drawable_texture = required(
                objc::object(drawable, c"texture"),
                "Metal postprocess drawable texture",
            )?;
            let targets = &self.postprocess_targets[target];
            let scene_pass = required(
                objc::object(
                    objc::class(c"MTLRenderPassDescriptor"),
                    c"renderPassDescriptor",
                ),
                "Metal scene render-pass descriptor",
            )?;
            let scene_attachments = required(
                objc::object(scene_pass, c"colorAttachments"),
                "scene color attachments",
            )?;
            let scene_color = required(
                objc::object_usize(scene_attachments, c"objectAtIndexedSubscript:", 0),
                "scene color attachment zero",
            )?;
            if self.sample_count == 4 {
                objc::void_object(scene_color, c"setTexture:", targets.multisample_color);
                objc::void_object(scene_color, c"setResolveTexture:", targets.scene_color);
                objc::void_usize(
                    scene_color,
                    c"setStoreAction:",
                    STORE_ACTION_MULTISAMPLE_RESOLVE,
                );
            } else {
                objc::void_object(scene_color, c"setTexture:", targets.scene_color);
                objc::void_usize(scene_color, c"setStoreAction:", STORE_ACTION_STORE);
            }
            objc::void_usize(scene_color, c"setLoadAction:", LOAD_ACTION_CLEAR);
            let [red, green, blue, alpha] = clear.components();
            objc::void_clear_color(
                scene_color,
                c"setClearColor:",
                objc::ClearColor {
                    red: f64::from(red),
                    green: f64::from(green),
                    blue: f64::from(blue),
                    alpha: f64::from(alpha),
                },
            );
            let scene_depth = required(
                objc::object(scene_pass, c"depthAttachment"),
                "scene depth attachment",
            )?;
            objc::void_object(scene_depth, c"setTexture:", targets.depth);
            objc::void_usize(scene_depth, c"setLoadAction:", LOAD_ACTION_CLEAR);
            objc::void_usize(scene_depth, c"setStoreAction:", STORE_ACTION_DONT_CARE);
            objc::void_f64(scene_depth, c"setClearDepth:", 1.0);

            let post_pass = required(
                objc::object(
                    objc::class(c"MTLRenderPassDescriptor"),
                    c"renderPassDescriptor",
                ),
                "Metal postprocess render-pass descriptor",
            )?;
            let post_attachments = required(
                objc::object(post_pass, c"colorAttachments"),
                "postprocess color attachments",
            )?;
            let post_color = required(
                objc::object_usize(post_attachments, c"objectAtIndexedSubscript:", 0),
                "postprocess color attachment zero",
            )?;
            objc::void_object(post_color, c"setTexture:", drawable_texture);
            objc::void_usize(post_color, c"setLoadAction:", LOAD_ACTION_CLEAR);
            objc::void_usize(post_color, c"setStoreAction:", STORE_ACTION_STORE);
            objc::void_clear_color(
                post_color,
                c"setClearColor:",
                objc::ClearColor {
                    red: 0.0,
                    green: 0.0,
                    blue: 0.0,
                    alpha: 1.0,
                },
            );

            let command = required(
                objc::object(self.surface.queue, c"commandBuffer"),
                "Metal postprocess command buffer",
            )?;
            let scene_encoder = required(
                objc::object_object(command, c"renderCommandEncoderWithDescriptor:", scene_pass),
                "Metal scene render encoder",
            )?;
            self.encode_prepared_scene(scene_encoder, scene)?;
            objc::void(scene_encoder, c"endEncoding");

            let post_encoder = required(
                objc::object_object(command, c"renderCommandEncoderWithDescriptor:", post_pass),
                "Metal postprocess render encoder",
            )?;
            let postprocess = &self.postprocess_pipelines[postprocess_pipeline];
            objc::void_object(
                post_encoder,
                c"setRenderPipelineState:",
                postprocess.pipeline,
            );
            objc::void_object_usize(
                post_encoder,
                c"setFragmentTexture:atIndex:",
                targets.scene_color,
                1,
            );
            objc::void_object_usize(
                post_encoder,
                c"setFragmentSamplerState:atIndex:",
                postprocess.sampler,
                2,
            );
            objc::void_three_usizes(
                post_encoder,
                c"drawPrimitives:vertexStart:vertexCount:",
                PRIMITIVE_TYPE_TRIANGLE,
                0,
                3,
            );
            objc::void(post_encoder, c"endEncoding");
            objc::void_object(command, c"presentDrawable:", drawable);
            objc::void(command, c"retain");
            objc::void(command, c"commit");
            self.surface.last_command_buffer = command;
            token.0.drawable = ptr::null_mut();
        }
        Ok(FrameDisposition::Presented(token.info().generation()))
    }

    unsafe fn encode_prepared_scene(
        &self,
        encoder: Object,
        scene: PreparedScene<'_>,
    ) -> Result<(), GraphicsError> {
        unsafe {
            match scene {
                PreparedScene::Draws(draws) => {
                    for (index, draw) in draws.iter().enumerate() {
                        let pipeline =
                            &self.pipelines[self.pipelines.index_of(draw.pipeline.id())?];
                        let mesh = &self.meshes[self.meshes.index_of(draw.mesh.id())?];
                        let texture = &self.textures[self.textures.index_of(draw.texture.id())?];
                        objc::void_object(encoder, c"setRenderPipelineState:", pipeline.pipeline);
                        objc::void_object(encoder, c"setDepthStencilState:", pipeline.depth_state);
                        objc::void_object_two_usizes(
                            encoder,
                            c"setVertexBuffer:offset:atIndex:",
                            self.uniform,
                            index * DRAW_UNIFORM_STRIDE,
                            0,
                        );
                        objc::void_object_two_usizes(
                            encoder,
                            c"setVertexBuffer:offset:atIndex:",
                            mesh.vertices,
                            0,
                            1,
                        );
                        objc::void_object_usize(
                            encoder,
                            c"setFragmentTexture:atIndex:",
                            texture.texture,
                            1,
                        );
                        objc::void_object_usize(
                            encoder,
                            c"setFragmentSamplerState:atIndex:",
                            texture.sampler,
                            2,
                        );
                        objc::void_two_usizes_object_usize_object_usize(
                            encoder,
                            c"drawIndexedPrimitives:indexType:indexBuffer:indexBufferOffset:indirectBuffer:indirectBufferOffset:",
                            PRIMITIVE_TYPE_TRIANGLE,
                            INDEX_TYPE_UINT16,
                            mesh.indices,
                            0,
                            mesh.indirect,
                            0,
                        );
                    }
                }
                PreparedScene::Instances => {
                    for batch in &self.resolved_instance_batches {
                        let pipeline = &self.instanced_pipelines[batch.pipeline];
                        let mesh = &self.meshes[batch.mesh];
                        let texture = &self.textures[batch.texture];
                        objc::void_object(encoder, c"setRenderPipelineState:", pipeline.pipeline);
                        objc::void_object(encoder, c"setDepthStencilState:", pipeline.depth_state);
                        objc::void_object_two_usizes(
                            encoder,
                            c"setVertexBuffer:offset:atIndex:",
                            mesh.vertices,
                            0,
                            1,
                        );
                        objc::void_object_two_usizes(
                            encoder,
                            c"setVertexBuffer:offset:atIndex:",
                            self.instance_transforms,
                            batch.transform_offset,
                            2,
                        );
                        objc::void_object_usize(
                            encoder,
                            c"setFragmentTexture:atIndex:",
                            texture.texture,
                            1,
                        );
                        objc::void_object_usize(
                            encoder,
                            c"setFragmentSamplerState:atIndex:",
                            texture.sampler,
                            2,
                        );
                        objc::void_three_usizes_object_two_usizes(
                            encoder,
                            c"drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:",
                            PRIMITIVE_TYPE_TRIANGLE,
                            usize::try_from(mesh.index_count).expect("u32 index count fits usize"),
                            INDEX_TYPE_UINT16,
                            mesh.indices,
                            0,
                            batch.instance_count,
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn destroy_resources(&mut self) {
        for pipeline in self.postprocess_pipelines.take_all() {
            release_postprocess_pipeline(pipeline);
        }
        for pipeline in self.pipelines.take_all() {
            release_pipeline(pipeline);
        }
        for pipeline in self.instanced_pipelines.take_all() {
            release_pipeline(pipeline);
        }
        for texture in self.textures.take_all() {
            release_texture(texture);
        }
        for target in self.targets.take_all() {
            release_target(target);
        }
        for target in self.postprocess_targets.take_all() {
            release_postprocess_target(target);
        }
        for mesh in self.meshes.take_all() {
            release_mesh(mesh);
        }
        unsafe {
            if !self.uniform.is_null() {
                objc::void(self.uniform, c"release");
                self.uniform = ptr::null_mut();
            }
            if !self.instance_transforms.is_null() {
                objc::void(self.instance_transforms, c"release");
                self.instance_transforms = ptr::null_mut();
            }
        }

        // `shutdown` moves the surface out and deliberately suppresses this
        // session's destructor. Release the now-empty arenas' allocations here
        // so that path does not retain their backing storage.
        self.pipelines = Arena::new("textured pipeline");
        self.instanced_pipelines = Arena::new("instanced textured pipeline");
        self.postprocess_pipelines = Arena::new("postprocess pipeline");
        self.textures = Arena::new("texture");
        self.targets = Arena::new("render targets");
        self.postprocess_targets = Arena::new("postprocess targets");
        self.meshes = Arena::new("mesh");
    }
}

// These helpers intentionally consume the wrapper so one call owns the native release authority.
// Its fields are raw pointers, so Clippy cannot otherwise observe that ownership transfer.
#[allow(clippy::needless_pass_by_value)]
fn release_mesh(mesh: MeshResource) {
    let MeshResource {
        vertices,
        indices,
        indirect,
        index_count: _,
    } = mesh;
    unsafe {
        objc::void(indirect, c"release");
        objc::void(indices, c"release");
        objc::void(vertices, c"release");
    }
}

#[allow(clippy::needless_pass_by_value)]
fn release_texture(texture: TextureResource) {
    let TextureResource { texture, sampler } = texture;
    unsafe {
        objc::void(sampler, c"release");
        objc::void(texture, c"release");
    }
}

#[allow(clippy::needless_pass_by_value)]
fn release_pipeline(pipeline: PipelineResource) {
    let PipelineResource {
        pipeline,
        depth_state,
    } = pipeline;
    unsafe {
        objc::void(depth_state, c"release");
        objc::void(pipeline, c"release");
    }
}

#[allow(clippy::needless_pass_by_value)]
fn release_postprocess_pipeline(pipeline: PostprocessPipelineResource) {
    let PostprocessPipelineResource { pipeline, sampler } = pipeline;
    unsafe {
        objc::void(sampler, c"release");
        objc::void(pipeline, c"release");
    }
}

#[allow(clippy::needless_pass_by_value)]
fn release_target(target: TargetResource) {
    let TargetResource {
        info: _,
        multisample_color,
        depth,
    } = target;
    unsafe {
        if !multisample_color.is_null() {
            objc::void(multisample_color, c"release");
        }
        if !depth.is_null() {
            objc::void(depth, c"release");
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn release_postprocess_target(target: PostprocessTargetResource) {
    let PostprocessTargetResource {
        info: _,
        scene_color,
        multisample_color,
        depth,
    } = target;
    unsafe {
        if !multisample_color.is_null() {
            objc::void(multisample_color, c"release");
        }
        if !scene_color.is_null() {
            objc::void(scene_color, c"release");
        }
        if !depth.is_null() {
            objc::void(depth, c"release");
        }
    }
}

impl Drop for TexturedSession<'_> {
    fn drop(&mut self) {
        let _ = self.surface.finish_last_submission();
        self.destroy_resources();
    }
}

#[repr(C)]
struct IndexedIndirectArguments {
    index_count: u32,
    instance_count: u32,
    index_start: u32,
    base_vertex: i32,
    base_instance: u32,
}

fn create_pipeline(
    device: Object,
    bytes: &[u8],
    sample_count: usize,
    instanced: bool,
) -> Result<PipelineResource, GraphicsError> {
    unsafe {
        let data = required(
            dispatch_data_create(
                bytes.as_ptr().cast(),
                bytes.len(),
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            "Metal cube library data",
        )?;
        let mut library_error = ptr::null_mut();
        let library = objc::object_object_out(
            device,
            c"newLibraryWithData:error:",
            data,
            &raw mut library_error,
        );
        if library.is_null() {
            return Err(GraphicsError::new(format!(
                "loading cube metallib failed: {}",
                objc::description(library_error)
            )));
        }
        let (vertex_name, vertex_label) = if instanced {
            (c"instanced_vertex", "Metal instanced vertex function")
        } else {
            (c"cube_vertex", "Metal cube vertex function")
        };
        let vertex = required(
            objc::object_object(
                library,
                c"newFunctionWithName:",
                objc::ns_string(vertex_name),
            ),
            vertex_label,
        )?;
        let fragment = required(
            objc::object_object(
                library,
                c"newFunctionWithName:",
                objc::ns_string(c"cube_fragment"),
            ),
            "Metal cube fragment function",
        )?;
        let descriptor = required(
            objc::object(objc::class(c"MTLRenderPipelineDescriptor"), c"new"),
            "Metal cube pipeline descriptor",
        )?;
        configure_scene_pipeline_descriptor(descriptor, vertex, fragment, sample_count, instanced)?;
        let mut pipeline_error = ptr::null_mut();
        let pipeline = objc::object_object_out(
            device,
            c"newRenderPipelineStateWithDescriptor:error:",
            descriptor,
            &raw mut pipeline_error,
        );
        if pipeline.is_null() {
            return Err(GraphicsError::new(format!(
                "creating Metal cube pipeline failed: {}",
                objc::description(pipeline_error)
            )));
        }
        let depth_descriptor = required(
            objc::object(objc::class(c"MTLDepthStencilDescriptor"), c"new"),
            "Metal depth descriptor",
        )?;
        objc::void_usize(
            depth_descriptor,
            c"setDepthCompareFunction:",
            COMPARE_FUNCTION_LESS,
        );
        objc::void_bool(depth_descriptor, c"setDepthWriteEnabled:", true);
        let depth_state = required(
            objc::object_object(
                device,
                c"newDepthStencilStateWithDescriptor:",
                depth_descriptor,
            ),
            "Metal cube depth state",
        )?;
        for object in [depth_descriptor, descriptor, fragment, vertex, library] {
            objc::void(object, c"release");
        }
        Ok(PipelineResource {
            pipeline,
            depth_state,
        })
    }
}

fn configure_scene_pipeline_descriptor(
    descriptor: Object,
    vertex: Object,
    fragment: Object,
    sample_count: usize,
    instanced: bool,
) -> Result<(), GraphicsError> {
    unsafe {
        objc::void_object(descriptor, c"setVertexFunction:", vertex);
        objc::void_object(descriptor, c"setFragmentFunction:", fragment);
        objc::void_usize(descriptor, c"setSampleCount:", sample_count);
        objc::void_usize(
            descriptor,
            c"setDepthAttachmentPixelFormat:",
            PIXEL_FORMAT_DEPTH32_FLOAT,
        );
        let colors = required(
            objc::object(descriptor, c"colorAttachments"),
            "pipeline colors",
        )?;
        let color = required(
            objc::object_usize(colors, c"objectAtIndexedSubscript:", 0),
            "pipeline color zero",
        )?;
        objc::void_usize(color, c"setPixelFormat:", PIXEL_FORMAT_BGRA8_UNORM_SRGB);
        configure_vertex_descriptor(descriptor, instanced)
    }
}

fn create_postprocess_pipeline(
    device: Object,
    bytes: &[u8],
) -> Result<PostprocessPipelineResource, GraphicsError> {
    unsafe {
        let data = required(
            dispatch_data_create(
                bytes.as_ptr().cast(),
                bytes.len(),
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            "Metal postprocess library data",
        )?;
        let mut library_error = ptr::null_mut();
        let library = objc::object_object_out(
            device,
            c"newLibraryWithData:error:",
            data,
            &raw mut library_error,
        );
        if library.is_null() {
            return Err(GraphicsError::new(format!(
                "loading postprocess metallib failed: {}",
                objc::description(library_error)
            )));
        }
        let vertex = required(
            objc::object_object(
                library,
                c"newFunctionWithName:",
                objc::ns_string(c"post_vertex"),
            ),
            "Metal postprocess vertex function",
        )?;
        let fragment = required(
            objc::object_object(
                library,
                c"newFunctionWithName:",
                objc::ns_string(c"post_fragment"),
            ),
            "Metal postprocess fragment function",
        )?;
        let descriptor = required(
            objc::object(objc::class(c"MTLRenderPipelineDescriptor"), c"new"),
            "Metal postprocess pipeline descriptor",
        )?;
        objc::void_object(descriptor, c"setVertexFunction:", vertex);
        objc::void_object(descriptor, c"setFragmentFunction:", fragment);
        objc::void_usize(descriptor, c"setSampleCount:", 1);
        let colors = required(
            objc::object(descriptor, c"colorAttachments"),
            "postprocess pipeline colors",
        )?;
        let color = required(
            objc::object_usize(colors, c"objectAtIndexedSubscript:", 0),
            "postprocess pipeline color zero",
        )?;
        objc::void_usize(color, c"setPixelFormat:", PIXEL_FORMAT_BGRA8_UNORM_SRGB);
        let mut pipeline_error = ptr::null_mut();
        let pipeline = objc::object_object_out(
            device,
            c"newRenderPipelineStateWithDescriptor:error:",
            descriptor,
            &raw mut pipeline_error,
        );
        if pipeline.is_null() {
            return Err(GraphicsError::new(format!(
                "creating Metal postprocess pipeline failed: {}",
                objc::description(pipeline_error)
            )));
        }
        let sampler_descriptor = required(
            objc::object(objc::class(c"MTLSamplerDescriptor"), c"new"),
            "Metal postprocess sampler descriptor",
        )?;
        objc::void_usize(sampler_descriptor, c"setMinFilter:", SAMPLER_FILTER_LINEAR);
        objc::void_usize(sampler_descriptor, c"setMagFilter:", SAMPLER_FILTER_LINEAR);
        let sampler = required(
            objc::object_object(
                device,
                c"newSamplerStateWithDescriptor:",
                sampler_descriptor,
            ),
            "Metal postprocess sampler",
        )?;
        for object in [sampler_descriptor, descriptor, fragment, vertex, library] {
            objc::void(object, c"release");
        }
        Ok(PostprocessPipelineResource { pipeline, sampler })
    }
}

unsafe fn configure_vertex_descriptor(
    descriptor: Object,
    instanced: bool,
) -> Result<(), GraphicsError> {
    unsafe {
        let vertex = required(
            objc::object(objc::class(c"MTLVertexDescriptor"), c"vertexDescriptor"),
            "Metal vertex descriptor",
        )?;
        let attributes = required(objc::object(vertex, c"attributes"), "vertex attributes")?;
        for (index, format, offset) in [
            (0, VERTEX_FORMAT_FLOAT3, 0),
            (1, VERTEX_FORMAT_FLOAT3, 12),
            (2, VERTEX_FORMAT_FLOAT2, 24),
        ] {
            let attribute = required(
                objc::object_usize(attributes, c"objectAtIndexedSubscript:", index),
                "Metal vertex attribute",
            )?;
            objc::void_usize(attribute, c"setFormat:", format);
            objc::void_usize(attribute, c"setOffset:", offset);
            objc::void_usize(attribute, c"setBufferIndex:", 1);
        }
        let layouts = required(objc::object(vertex, c"layouts"), "vertex layouts")?;
        let layout = required(
            objc::object_usize(layouts, c"objectAtIndexedSubscript:", 1),
            "vertex layout one",
        )?;
        objc::void_usize(layout, c"setStride:", mem::size_of::<Vertex>());
        if instanced {
            for (index, offset) in [(3, 0), (4, 16), (5, 32), (6, 48)] {
                let attribute = required(
                    objc::object_usize(attributes, c"objectAtIndexedSubscript:", index),
                    "Metal instance matrix attribute",
                )?;
                objc::void_usize(attribute, c"setFormat:", VERTEX_FORMAT_FLOAT4);
                objc::void_usize(attribute, c"setOffset:", offset);
                objc::void_usize(attribute, c"setBufferIndex:", 2);
            }
            let instance_layout = required(
                objc::object_usize(layouts, c"objectAtIndexedSubscript:", 2),
                "instance vertex layout two",
            )?;
            objc::void_usize(instance_layout, c"setStride:", INSTANCE_TRANSFORM_SIZE);
            objc::void_usize(
                instance_layout,
                c"setStepFunction:",
                VERTEX_STEP_FUNCTION_PER_INSTANCE,
            );
        }
        objc::void_object(descriptor, c"setVertexDescriptor:", vertex);
        Ok(())
    }
}

fn create_target_texture(
    device: Object,
    format: usize,
    width: usize,
    height: usize,
    sample_count: usize,
    usage: usize,
) -> Result<Object, GraphicsError> {
    unsafe {
        let descriptor = required(
            objc::object_three_usizes_bool(
                objc::class(c"MTLTextureDescriptor"),
                c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
                format,
                width,
                height,
                false,
            ),
            "Metal target texture descriptor",
        )?;
        if sample_count == 4 {
            objc::void_usize(descriptor, c"setTextureType:", TEXTURE_TYPE_2D_MULTISAMPLE);
            objc::void_usize(descriptor, c"setSampleCount:", 4);
            objc::void_usize(descriptor, c"setStorageMode:", STORAGE_MODE_MEMORYLESS);
        } else {
            objc::void_usize(descriptor, c"setStorageMode:", STORAGE_MODE_PRIVATE);
        }
        objc::void_usize(descriptor, c"setUsage:", usage);
        required(
            objc::object_object(device, c"newTextureWithDescriptor:", descriptor),
            "Metal render target texture",
        )
    }
}
