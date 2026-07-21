use core::ffi::c_void;
use core::{mem, ptr, slice};
use std::collections::VecDeque;
use std::ffi::CString;
use std::time::Duration;
use std::{format, vec::Vec};

use mulciber_platform::{SurfaceTarget, WindowMetrics};

use super::{ClearSurface, check, color_subresource_range, error, vk};
use crate::graphics::{
    BlendMode, DepthMode, MaterialPipelineConfig, MeshIndices, SamplerAddress, SamplerFilter,
    ShadowPipelineConfig, mip_extent,
};
use crate::resource::{Arena, DestroyRequest, ResourceId, ResourceKind};
use crate::{
    ClearColor, DeviceRequest, FrameAcquire, FrameDisposition, GeometrySource, GpuFrameTiming,
    GpuScopeTiming, GpuTimingFeedback, GpuTimingScope, GpuTimingSupport, GraphicsError,
    MaterialRecord, PresentFeedback, SampleCount, ShaderArtifact, ShadowPrepass, ShadowSource,
    SurfaceInfo, TexturedInstanceBatch, TexturedSceneDraw, Vertex, VertexFormat,
};

const DEPTH_FORMAT: vk::VkFormat = vk::VK_FORMAT_D32_SFLOAT;
/// Far-plane depth clear for conventional less-compare scenes; reversed-Z material scenes
/// clear to 0.0 instead, selected per submission by the validated `depth_clear` argument.
const DEPTH_CLEAR_FAR: f32 = 1.0;
/// The specification's `VK_LOD_CLAMP_NONE`, absent from the generated bindings.
const LOD_CLAMP_NONE: f32 = 1000.0;
const DRAW_UNIFORM_SIZE: usize = 64;
const DRAW_UNIFORM_STRIDE: usize = 256;

/// Alignment for per-record offsets into the frame's read-only storage region: the
/// specification's cap on `minStorageBufferOffsetAlignment`, valid on every implementation.
const STORAGE_OFFSET_ALIGNMENT: usize = 256;
const INSTANCE_TRANSFORM_SIZE: usize = 64;
const GPU_QUERY_COUNT: u32 = 8;
const FRAME_QUERY_START: u32 = 0;
const SHADOW_QUERY_START: u32 = 2;
const SCENE_QUERY_START: u32 = 4;
const POSTPROCESS_QUERY_START: u32 = 6;
const GPU_TIMING_FEEDBACK_CAP: usize = 1024;

#[derive(Clone, Copy)]
struct PendingGpuTiming {
    frame_index: u64,
    has_shadow: bool,
    has_postprocess: bool,
}

struct GpuTimingState {
    enabled: bool,
    query_pool: vk::VkQueryPool,
    pending: Option<PendingGpuTiming>,
    completed: VecDeque<GpuFrameTiming>,
}

#[derive(Clone, Copy, Default)]
struct Buffer {
    handle: vk::VkBuffer,
    memory: vk::VkDeviceMemory,
    size: vk::VkDeviceSize,
}

#[derive(Clone, Copy, Default)]
struct Image {
    handle: vk::VkImage,
    memory: vk::VkDeviceMemory,
    view: vk::VkImageView,
}

struct MeshResource {
    vertices: Buffer,
    indices: Buffer,
    indirect: Buffer,
    index_count: u32,
    index_type: vk::VkIndexType,
}

struct TextureResource {
    image: Image,
    sampler: vk::VkSampler,
}

struct PipelineResource {
    set_layout: vk::VkDescriptorSetLayout,
    layout: vk::VkPipelineLayout,
    pipeline: vk::VkPipeline,
    descriptor_pool: vk::VkDescriptorPool,
    sampler: vk::VkSampler,
    bindings: Vec<(ResourceId, vk::VkDescriptorSet)>,
}

struct MaterialPipelineResource {
    set_layout: vk::VkDescriptorSetLayout,
    layout: vk::VkPipelineLayout,
    pipeline: vk::VkPipeline,
    /// Single-sample no-depth variant for the presentable overlay pass; null unless the
    /// pipeline declares [`DepthMode::Off`].
    overlay_pipeline: vk::VkPipeline,
    descriptor_pool: vk::VkDescriptorPool,
    /// One pipeline-owned sampler per declared slot as (binding, sampler).
    samplers: Vec<(u32, vk::VkSampler)>,
    /// Declared uniform slot as (binding, size).
    uniform: Option<(u32, u32)>,
    /// Declared read-only storage slot as (binding, size).
    storage: Option<(u32, u32)>,
    /// Declared texture binding numbers in ascending order.
    texture_bindings: Vec<u32>,
    /// Declared depth-texture slot fed from a shadow map per record.
    depth_texture_binding: Option<u32>,
    /// Declared depth-texture-array slot fed from a shadow map array per record.
    depth_texture_array_binding: Option<u32>,
    /// Pipeline-owned fixed-recipe comparison sampler as (binding, sampler).
    comparison_sampler: Option<(u32, vk::VkSampler)>,
    /// Descriptor sets cached per sampled-identity tuple (textures, then the shadow map or
    /// array).
    bindings: Vec<(Vec<ResourceId>, vk::VkDescriptorSet)>,
}

struct ShadowMapResource {
    image: Image,
    size: u32,
    /// Whether any shadow pass has rendered into this map; sampling before that is rejected.
    rendered: bool,
}

struct ShadowMapArrayResource {
    /// Layered depth image backing every cascade.
    image: vk::VkImage,
    memory: vk::VkDeviceMemory,
    /// One single-layer rendering view per cascade in layer order.
    layer_views: Vec<vk::VkImageView>,
    /// Whole-array view the scene pass samples.
    array_view: vk::VkImageView,
    size: u32,
    layers: u32,
    /// Whether any cascaded shadow pass has rendered into this array; sampling before that is
    /// rejected.
    rendered: bool,
}

struct ShadowPipelineResource {
    set_layout: vk::VkDescriptorSetLayout,
    layout: vk::VkPipelineLayout,
    pipeline: vk::VkPipeline,
    descriptor_pool: vk::VkDescriptorPool,
    /// Declared uniform slot as (binding, size).
    uniform: Option<(u32, u32)>,
    /// Declared read-only storage slot as (binding, size).
    storage: Option<(u32, u32)>,
    /// Buffer-only descriptor set cached until a pool reset.
    descriptor: Option<vk::VkDescriptorSet>,
}

struct TargetResource {
    info: SurfaceInfo,
    multisample_color: Option<Image>,
    depth: Option<Image>,
}

struct PostprocessTargetResource {
    info: SurfaceInfo,
    /// Offscreen scene extent — the render-scale-adjusted extent the scene pass renders at.
    scene_extent: vk::VkExtent2D,
    scene_color: Option<Image>,
    multisample_color: Option<Image>,
    depth: Option<Image>,
}

#[derive(Clone, Copy)]
struct ResolvedDraw {
    mesh: usize,
    pipeline: usize,
    descriptor: vk::VkDescriptorSet,
    dynamic_offset: u32,
}

#[derive(Clone, Copy)]
struct ResolvedInstanceBatch {
    mesh: usize,
    pipeline: usize,
    descriptor: vk::VkDescriptorSet,
    transform_offset: u64,
    instance_count: u32,
}

/// Geometry for one material record resolved at preparation: an uploaded mesh's arena index, or
/// offsets into the frame's transient-geometry region recomputed in staging order.
#[derive(Clone, Copy)]
enum ResolvedGeometry {
    Mesh(usize),
    Transient {
        vertex_offset: u64,
        index_offset: u64,
        index_count: u32,
        index_type: vk::VkIndexType,
    },
}

#[derive(Clone, Copy)]
struct ResolvedMaterialDraw {
    geometry: ResolvedGeometry,
    pipeline: usize,
    descriptor: vk::VkDescriptorSet,
    /// Uniform and storage dynamic offsets in ascending binding-number order, matching how
    /// Vulkan consumes dynamic offsets across the set layout's dynamic descriptors.
    dynamic_offsets: [u32; 2],
    /// One dynamic offset per declared uniform and storage slot.
    dynamic_offset_count: u32,
}

#[derive(Clone, Copy)]
struct ResolvedShadowDraw {
    mesh: usize,
    pipeline: usize,
    descriptor: vk::VkDescriptorSet,
    /// Uniform and storage dynamic offsets in ascending binding-number order, matching how
    /// Vulkan consumes dynamic offsets across the set layout's dynamic descriptors.
    dynamic_offsets: [u32; 2],
    /// One dynamic offset per declared uniform and storage slot.
    dynamic_offset_count: u32,
}

/// Destination of the prepared frame's depth-only pre-pass.
#[derive(Clone, Copy)]
enum PendingShadowTarget {
    /// Shadow map arena index for a single pass.
    Map(usize),
    /// Shadow map array arena index for a cascaded pass.
    Array(usize),
}

/// The rendered depth view a material record samples, routed to the pipeline's plain or array
/// depth-texture slot.
#[derive(Clone, Copy)]
enum ShadowView {
    Map(vk::VkImageView),
    Array(vk::VkImageView),
}

#[derive(Clone, Copy)]
enum PreparedScene {
    Draws,
    Instances,
    Materials,
}

pub(crate) struct TexturedSession<'window> {
    surface: ClearSurface<'window>,
    sample_count: vk::VkSampleCountFlagBits,
    gpu_timing: GpuTimingState,
    uniform: Buffer,
    uniform_capacity: usize,
    /// Frame-transient read-only storage region for material and shadow records, in bytes.
    storage: Buffer,
    storage_capacity: usize,
    /// Frame-transient indexed-geometry region for material records, in bytes.
    transient_geometry: Buffer,
    transient_capacity: usize,
    resolved_draws: Vec<ResolvedDraw>,
    instance_transforms: Buffer,
    instance_capacity: usize,
    resolved_instance_batches: Vec<ResolvedInstanceBatch>,
    resolved_material_draws: Vec<ResolvedMaterialDraw>,
    /// Overlay records resolved by the last material preparation, recorded into the
    /// presentable pass after the postprocess draw.
    resolved_overlay_draws: Vec<ResolvedMaterialDraw>,
    resolved_shadow_draws: Vec<ResolvedShadowDraw>,
    /// Per-cascade draw counts splitting `resolved_shadow_draws` in layer order; empty unless
    /// the prepared pre-pass targets a shadow map array.
    resolved_shadow_cascades: Vec<usize>,
    /// Target of the prepared frame's depth-only pre-pass.
    pending_shadow_target: Option<PendingShadowTarget>,
    recorded_has_shadow: bool,
    meshes: Arena<MeshResource>,
    textures: Arena<TextureResource>,
    pipelines: Arena<PipelineResource>,
    instanced_pipelines: Arena<PipelineResource>,
    material_pipelines: Arena<MaterialPipelineResource>,
    shadow_maps: Arena<ShadowMapResource>,
    shadow_map_arrays: Arena<ShadowMapArrayResource>,
    shadow_pipelines: Arena<ShadowPipelineResource>,
    targets: Arena<TargetResource>,
    postprocess_pipelines: Arena<PipelineResource>,
    postprocess_targets: Arena<PostprocessTargetResource>,
    deferred_token: Option<TexturedFrameToken>,
}

#[derive(Clone, Copy)]
pub(crate) struct TexturedFrameToken {
    image_index: u32,
    info: SurfaceInfo,
}

impl TexturedFrameToken {
    pub(crate) const fn info(&self) -> SurfaceInfo {
        self.info
    }
}

impl<'window> TexturedSession<'window> {
    pub(crate) fn new(
        target: SurfaceTarget<'window>,
        metrics: WindowMetrics,
        request: DeviceRequest,
    ) -> Result<(Self, SampleCount), GraphicsError> {
        let surface = ClearSurface::new(target, metrics)?;
        let sample_count = if request.preferred_sample_count == SampleCount::Four
            && surface.device().adapter.sample_count == vk::VK_SAMPLE_COUNT_4_BIT
        {
            vk::VK_SAMPLE_COUNT_4_BIT
        } else {
            vk::VK_SAMPLE_COUNT_1_BIT
        };
        let uniform = create_buffer(
            &surface,
            DRAW_UNIFORM_STRIDE,
            vk::VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT as u32,
            &[],
        )?;
        let storage = match create_buffer(
            &surface,
            STORAGE_OFFSET_ALIGNMENT,
            vk::VK_BUFFER_USAGE_STORAGE_BUFFER_BIT as u32,
            &[],
        ) {
            Ok(buffer) => buffer,
            Err(failure) => {
                destroy_buffer(&surface, uniform);
                return Err(failure);
            }
        };
        let transient_geometry = match create_buffer(
            &surface,
            STORAGE_OFFSET_ALIGNMENT,
            (vk::VK_BUFFER_USAGE_VERTEX_BUFFER_BIT | vk::VK_BUFFER_USAGE_INDEX_BUFFER_BIT) as u32,
            &[],
        ) {
            Ok(buffer) => buffer,
            Err(failure) => {
                destroy_buffer(&surface, storage);
                destroy_buffer(&surface, uniform);
                return Err(failure);
            }
        };
        let instance_transforms = match create_buffer(
            &surface,
            INSTANCE_TRANSFORM_SIZE,
            vk::VK_BUFFER_USAGE_VERTEX_BUFFER_BIT as u32,
            &[],
        ) {
            Ok(buffer) => buffer,
            Err(failure) => {
                destroy_buffer(&surface, transient_geometry);
                destroy_buffer(&surface, storage);
                destroy_buffer(&surface, uniform);
                return Err(failure);
            }
        };
        Ok((
            Self {
                surface,
                sample_count,
                gpu_timing: GpuTimingState {
                    enabled: false,
                    query_pool: ptr::null_mut(),
                    pending: None,
                    completed: VecDeque::new(),
                },
                uniform,
                uniform_capacity: 1,
                storage,
                storage_capacity: STORAGE_OFFSET_ALIGNMENT,
                transient_geometry,
                transient_capacity: STORAGE_OFFSET_ALIGNMENT,
                resolved_draws: Vec::new(),
                instance_transforms,
                instance_capacity: 1,
                resolved_instance_batches: Vec::new(),
                resolved_material_draws: Vec::new(),
                resolved_overlay_draws: Vec::new(),
                resolved_shadow_draws: Vec::new(),
                resolved_shadow_cascades: Vec::new(),
                pending_shadow_target: None,
                recorded_has_shadow: false,
                meshes: Arena::new("mesh"),
                textures: Arena::new("texture"),
                pipelines: Arena::new("textured pipeline"),
                instanced_pipelines: Arena::new("instanced textured pipeline"),
                material_pipelines: Arena::new("material pipeline"),
                shadow_maps: Arena::new("shadow map"),
                shadow_map_arrays: Arena::new("shadow map array"),
                shadow_pipelines: Arena::new("shadow pipeline"),
                targets: Arena::new("render targets"),
                postprocess_pipelines: Arena::new("postprocess pipeline"),
                postprocess_targets: Arena::new("postprocess targets"),
                deferred_token: None,
            },
            if sample_count == vk::VK_SAMPLE_COUNT_4_BIT {
                SampleCount::Four
            } else {
                SampleCount::One
            },
        ))
    }

    pub(crate) const fn info(&self) -> SurfaceInfo {
        self.surface.info()
    }

    pub(crate) fn gpu_timing_support(&self) -> GpuTimingSupport {
        if self.surface.device().adapter.timestamp_valid_bits == 0 {
            GpuTimingSupport::Unsupported
        } else {
            GpuTimingSupport::Regions
        }
    }

    pub(crate) fn take_gpu_timings(&mut self) -> GpuTimingFeedback {
        if !self.gpu_timing.enabled {
            return GpuTimingFeedback::Disabled;
        }
        if self.gpu_timing.query_pool.is_null() {
            return GpuTimingFeedback::Unsupported;
        }
        GpuTimingFeedback::Reported(self.gpu_timing.completed.drain(..).collect())
    }

    pub(crate) fn set_gpu_timing_enabled(&mut self, enabled: bool) -> Result<(), GraphicsError> {
        if enabled
            && self.gpu_timing.query_pool.is_null()
            && self.surface.device().adapter.timestamp_valid_bits != 0
        {
            self.gpu_timing.query_pool = create_gpu_query_pool(&self.surface)?;
        }
        self.gpu_timing.enabled = enabled;
        if !enabled {
            self.gpu_timing.completed.clear();
        }
        Ok(())
    }

    pub(crate) fn acquire(
        &mut self,
        metrics: WindowMetrics,
    ) -> Result<FrameAcquire<TexturedFrameToken>, GraphicsError> {
        self.flush_deferred_abandon()?;
        let acquisition = self.surface.acquire_image(metrics)?;
        self.reclaim_stale_targets()?;
        let info = self.surface.info();
        Ok(acquisition.map_ready(|image_index| TexturedFrameToken { image_index, info }))
    }

    pub(crate) fn take_present_feedback(&mut self) -> PresentFeedback {
        self.surface.take_present_feedback()
    }

    pub(crate) fn create_mesh(
        &mut self,
        vertices: &[Vertex],
        indices: &[u16],
    ) -> Result<ResourceId, GraphicsError> {
        self.create_mesh_from_bytes(bytes_of_slice(vertices), MeshIndices::U16(indices))
    }

    pub(crate) fn create_mesh_from_bytes(
        &mut self,
        vertex_bytes: &[u8],
        indices: MeshIndices<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        let (index_bytes, index_type) = match indices {
            MeshIndices::U16(indices) => (bytes_of_slice(indices), vk::VK_INDEX_TYPE_UINT16),
            MeshIndices::U32(indices) => (bytes_of_slice(indices), vk::VK_INDEX_TYPE_UINT32),
        };
        let draw = vk::VkDrawIndexedIndirectCommand {
            indexCount: u32::try_from(indices.len())
                .map_err(|_| error("mesh index count exceeds u32"))?,
            instanceCount: 1,
            firstIndex: 0,
            vertexOffset: 0,
            firstInstance: 0,
        };
        let vertices = create_buffer(
            &self.surface,
            vertex_bytes.len(),
            vk::VK_BUFFER_USAGE_VERTEX_BUFFER_BIT as u32,
            vertex_bytes,
        )?;
        let indices = match create_buffer(
            &self.surface,
            index_bytes.len(),
            vk::VK_BUFFER_USAGE_INDEX_BUFFER_BIT as u32,
            index_bytes,
        ) {
            Ok(buffer) => buffer,
            Err(failure) => {
                destroy_buffer(&self.surface, vertices);
                return Err(failure);
            }
        };
        let indirect = match create_buffer(
            &self.surface,
            mem::size_of_val(&draw),
            vk::VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT as u32,
            bytes_of(&draw),
        ) {
            Ok(buffer) => buffer,
            Err(failure) => {
                destroy_buffer(&self.surface, vertices);
                destroy_buffer(&self.surface, indices);
                return Err(failure);
            }
        };
        self.meshes.insert(MeshResource {
            vertices,
            indices,
            indirect,
            index_count: draw.indexCount,
            index_type,
        })
    }

    pub(crate) fn create_texture(
        &mut self,
        width: u32,
        height: u32,
        levels: &[&[u8]],
    ) -> Result<ResourceId, GraphicsError> {
        let mip_levels =
            u32::try_from(levels.len()).map_err(|_| error("mip chain length exceeds u32"))?;
        let mut packed = Vec::with_capacity(levels.iter().map(|texels| texels.len()).sum());
        for texels in levels {
            packed.extend_from_slice(texels);
        }
        let staging = create_buffer(
            &self.surface,
            packed.len(),
            vk::VK_BUFFER_USAGE_TRANSFER_SRC_BIT as u32,
            &packed,
        )?;
        let image = match create_image(
            &self.surface,
            width,
            height,
            vk::VK_FORMAT_R8G8B8A8_SRGB,
            (vk::VK_IMAGE_USAGE_TRANSFER_DST_BIT | vk::VK_IMAGE_USAGE_SAMPLED_BIT) as u32,
            vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
            vk::VK_SAMPLE_COUNT_1_BIT,
            mip_levels,
        ) {
            Ok(image) => image,
            Err(failure) => {
                destroy_buffer(&self.surface, staging);
                return Err(failure);
            }
        };
        let upload = self.upload_texture(&staging, &image, width, height, levels);
        destroy_buffer(&self.surface, staging);
        if let Err(failure) = upload {
            destroy_image(&self.surface, image);
            return Err(failure);
        }
        let sampler_info = vk::VkSamplerCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,
            magFilter: vk::VK_FILTER_LINEAR,
            minFilter: vk::VK_FILTER_LINEAR,
            mipmapMode: vk::VK_SAMPLER_MIPMAP_MODE_NEAREST,
            addressModeU: vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            addressModeV: vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            addressModeW: vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            maxAnisotropy: 1.0,
            maxLod: 0.0,
            ..Default::default()
        };
        let mut sampler = ptr::null_mut();
        if let Err(failure) = check(
            unsafe {
                self.surface
                    .device()
                    .functions
                    .create_sampler
                    .expect("loaded function")(
                    self.surface.device().handle,
                    &raw const sampler_info,
                    ptr::null(),
                    &raw mut sampler,
                )
            },
            "vkCreateSampler for textured slice",
        ) {
            destroy_image(&self.surface, image);
            return Err(failure);
        }
        self.textures.insert(TextureResource { image, sampler })
    }

    pub(crate) fn create_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        let resource = create_pipeline(&self.surface, shader.payload(), self.sample_count, false)?;
        self.pipelines.insert(resource)
    }

    pub(crate) fn create_instanced_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        let resource = create_pipeline(&self.surface, shader.payload(), self.sample_count, true)?;
        self.instanced_pipelines.insert(resource)
    }

    pub(crate) fn create_postprocess_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        let resource = create_postprocess_pipeline(&self.surface, shader.payload())?;
        self.postprocess_pipelines.insert(resource)
    }

    pub(crate) fn create_material_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
        config: &MaterialPipelineConfig<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        let resource =
            create_material_pipeline(&self.surface, shader.payload(), config, self.sample_count)?;
        self.material_pipelines.insert(resource)
    }

    pub(crate) fn create_shadow_map(&mut self, size: u32) -> Result<ResourceId, GraphicsError> {
        let mut properties = vk::VkFormatProperties::default();
        unsafe {
            self.surface
                .device()
                .instance
                .functions
                .get_physical_device_format_properties
                .expect("loaded function")(
                self.surface.device().adapter.handle,
                DEPTH_FORMAT,
                &raw mut properties,
            );
        }
        let required = (vk::VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT
            | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT
            | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR_BIT) as u32;
        if properties.optimalTilingFeatures & required != required {
            return Err(error(
                "adapter does not support rendering and comparison-sampling D32_FLOAT shadow maps",
            ));
        }
        let image = create_image(
            &self.surface,
            size,
            size,
            DEPTH_FORMAT,
            (vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT | vk::VK_IMAGE_USAGE_SAMPLED_BIT)
                as u32,
            vk::VK_IMAGE_ASPECT_DEPTH_BIT as u32,
            vk::VK_SAMPLE_COUNT_1_BIT,
            1,
        )?;
        self.shadow_maps.insert(ShadowMapResource {
            image,
            size,
            rendered: false,
        })
    }

    pub(crate) fn create_shadow_map_array(
        &mut self,
        size: u32,
        layers: u32,
    ) -> Result<ResourceId, GraphicsError> {
        let mut properties = vk::VkFormatProperties::default();
        unsafe {
            self.surface
                .device()
                .instance
                .functions
                .get_physical_device_format_properties
                .expect("loaded function")(
                self.surface.device().adapter.handle,
                DEPTH_FORMAT,
                &raw mut properties,
            );
        }
        let required = (vk::VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT
            | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT
            | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR_BIT) as u32;
        if properties.optimalTilingFeatures & required != required {
            return Err(error(
                "adapter does not support rendering and comparison-sampling D32_FLOAT shadow maps",
            ));
        }
        let array = create_shadow_map_array_storage(&self.surface, size, layers)?;
        self.shadow_map_arrays.insert(array)
    }

    pub(crate) fn create_shadow_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
        config: &ShadowPipelineConfig<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        let resource = create_shadow_pipeline(&self.surface, shader.payload(), config)?;
        self.shadow_pipelines.insert(resource)
    }

    /// Destroys image storage for render targets from superseded surface generations.
    ///
    /// The surface generation advances only inside swapchain recreation, which first waits on the
    /// in-flight frame fence, and draws reject targets that do not match the acquired generation.
    /// A target older than the current generation therefore cannot be referenced by submitted GPU
    /// work, so its storage is reclaimed instead of growing until shutdown across live resizes.
    fn reclaim_stale_targets(&mut self) -> Result<(), GraphicsError> {
        let current = self.surface.info().generation();
        let surface = &self.surface;
        for target in self.targets.iter_mut() {
            if target.info.generation().get() >= current.get() {
                continue;
            }
            if let Some(color) = target.multisample_color.take() {
                destroy_image(surface, color);
            }
            if let Some(depth) = target.depth.take() {
                destroy_image(surface, depth);
            }
        }
        let mut reclaimed_postprocess_target = false;
        for target in self.postprocess_targets.iter_mut() {
            if target.info.generation().get() >= current.get() {
                continue;
            }
            reclaimed_postprocess_target = true;
            if let Some(color) = target.multisample_color.take() {
                destroy_image(surface, color);
            }
            if let Some(color) = target.scene_color.take() {
                destroy_image(surface, color);
            }
            if let Some(depth) = target.depth.take() {
                destroy_image(surface, depth);
            }
        }
        if reclaimed_postprocess_target {
            let device = self.surface.device();
            for pipeline in self.postprocess_pipelines.iter_mut() {
                let replacement = create_postprocess_descriptor_pool(device)?;
                unsafe {
                    device
                        .functions
                        .destroy_descriptor_pool
                        .expect("loaded function")(
                        device.handle,
                        pipeline.descriptor_pool,
                        ptr::null(),
                    );
                }
                pipeline.descriptor_pool = replacement;
                pipeline.bindings.clear();
            }
        }
        Ok(())
    }

    pub(crate) fn create_render_targets(
        &mut self,
        info: SurfaceInfo,
    ) -> Result<ResourceId, GraphicsError> {
        self.reclaim_stale_targets()?;
        let mut properties = vk::VkFormatProperties::default();
        unsafe {
            self.surface
                .device()
                .instance
                .functions
                .get_physical_device_format_properties
                .expect("loaded function")(
                self.surface.device().adapter.handle,
                DEPTH_FORMAT,
                &raw mut properties,
            );
        }
        if properties.optimalTilingFeatures
            & vk::VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT as u32
            == 0
        {
            return Err(error(
                "adapter does not support D32_FLOAT depth attachments",
            ));
        }
        let extent = info.extent();
        let depth = create_image(
            &self.surface,
            extent.width(),
            extent.height(),
            DEPTH_FORMAT,
            vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT as u32,
            vk::VK_IMAGE_ASPECT_DEPTH_BIT as u32,
            self.sample_count,
            1,
        )?;
        let multisample_color = if self.sample_count == vk::VK_SAMPLE_COUNT_4_BIT {
            match create_image(
                &self.surface,
                extent.width(),
                extent.height(),
                self.surface.swapchain.format,
                vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT as u32,
                vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
                self.sample_count,
                1,
            ) {
                Ok(image) => Some(image),
                Err(failure) => {
                    destroy_image(&self.surface, depth);
                    return Err(failure);
                }
            }
        } else {
            None
        };
        self.targets.insert(TargetResource {
            info,
            multisample_color,
            depth: Some(depth),
        })
    }

    /// Creates the two-pass targets: offscreen scene storage at `scene_extent` — the
    /// render-scale-adjusted extent — while presentation stays at the surface extent carried
    /// by `info`.
    pub(crate) fn create_postprocess_targets(
        &mut self,
        info: SurfaceInfo,
        scene_extent: crate::SurfaceExtent,
    ) -> Result<ResourceId, GraphicsError> {
        self.reclaim_stale_targets()?;
        let scene_color = create_image(
            &self.surface,
            scene_extent.width(),
            scene_extent.height(),
            self.surface.swapchain.format,
            (vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | vk::VK_IMAGE_USAGE_SAMPLED_BIT) as u32,
            vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
            vk::VK_SAMPLE_COUNT_1_BIT,
            1,
        )?;
        let depth = match create_image(
            &self.surface,
            scene_extent.width(),
            scene_extent.height(),
            DEPTH_FORMAT,
            vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT as u32,
            vk::VK_IMAGE_ASPECT_DEPTH_BIT as u32,
            self.sample_count,
            1,
        ) {
            Ok(depth) => depth,
            Err(failure) => {
                destroy_image(&self.surface, scene_color);
                return Err(failure);
            }
        };
        let multisample_color = if self.sample_count == vk::VK_SAMPLE_COUNT_4_BIT {
            match create_image(
                &self.surface,
                scene_extent.width(),
                scene_extent.height(),
                self.surface.swapchain.format,
                vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT as u32,
                vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
                self.sample_count,
                1,
            ) {
                Ok(image) => Some(image),
                Err(failure) => {
                    destroy_image(&self.surface, depth);
                    destroy_image(&self.surface, scene_color);
                    return Err(failure);
                }
            }
        } else {
            None
        };
        self.postprocess_targets.insert(PostprocessTargetResource {
            info,
            scene_extent: vk::VkExtent2D {
                width: scene_extent.width(),
                height: scene_extent.height(),
            },
            scene_color: Some(scene_color),
            multisample_color,
            depth: Some(depth),
        })
    }

    pub(crate) fn draw_scene_and_present(
        &mut self,
        token: TexturedFrameToken,
        draws: &[TexturedSceneDraw<'_>],
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let target_index = self.targets.index_of(targets)?;
        if self.targets[target_index].info != token.info {
            return Err(error(
                "render targets do not match acquired Vulkan generation",
            ));
        }
        self.prepare_scene(draws)?;
        self.record_draw(
            token.image_index,
            target_index,
            PreparedScene::Draws,
            clear,
            DEPTH_CLEAR_FAR,
        )?;
        self.submit_recorded(token.image_index, false)
    }

    pub(crate) fn draw_scene_postprocessed_and_present(
        &mut self,
        token: TexturedFrameToken,
        draws: &[TexturedSceneDraw<'_>],
        postprocess_pipeline: ResourceId,
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let postprocess_pipeline_index =
            self.postprocess_pipelines.index_of(postprocess_pipeline)?;
        let target_index = self.postprocess_targets.index_of(targets)?;
        if self.postprocess_targets[target_index].info != token.info {
            return Err(error(
                "postprocess targets do not match acquired Vulkan generation",
            ));
        }
        self.prepare_scene(draws)?;
        let postprocess_descriptor =
            self.postprocess_descriptor_set(postprocess_pipeline_index, target_index, targets)?;
        self.record_postprocessed_draw(
            token.image_index,
            postprocess_pipeline_index,
            target_index,
            postprocess_descriptor,
            PreparedScene::Draws,
            clear,
            DEPTH_CLEAR_FAR,
        )?;
        self.submit_recorded(token.image_index, true)
    }

    pub(crate) fn draw_instanced_scene_and_present(
        &mut self,
        token: TexturedFrameToken,
        batches: &[TexturedInstanceBatch<'_>],
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let target_index = self.targets.index_of(targets)?;
        if self.targets[target_index].info != token.info {
            return Err(error(
                "render targets do not match acquired Vulkan generation",
            ));
        }
        self.prepare_instanced_scene(batches)?;
        self.record_draw(
            token.image_index,
            target_index,
            PreparedScene::Instances,
            clear,
            DEPTH_CLEAR_FAR,
        )?;
        self.submit_recorded(token.image_index, false)
    }

    pub(crate) fn draw_instanced_scene_postprocessed_and_present(
        &mut self,
        token: TexturedFrameToken,
        batches: &[TexturedInstanceBatch<'_>],
        postprocess_pipeline: ResourceId,
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let postprocess_pipeline_index =
            self.postprocess_pipelines.index_of(postprocess_pipeline)?;
        let target_index = self.postprocess_targets.index_of(targets)?;
        if self.postprocess_targets[target_index].info != token.info {
            return Err(error(
                "postprocess targets do not match acquired Vulkan generation",
            ));
        }
        self.prepare_instanced_scene(batches)?;
        let postprocess_descriptor =
            self.postprocess_descriptor_set(postprocess_pipeline_index, target_index, targets)?;
        self.record_postprocessed_draw(
            token.image_index,
            postprocess_pipeline_index,
            target_index,
            postprocess_descriptor,
            PreparedScene::Instances,
            clear,
            DEPTH_CLEAR_FAR,
        )?;
        self.submit_recorded(token.image_index, true)
    }

    /// Rejects sampling a shadow map that neither an earlier frame nor this submission's shadow
    /// pass has rendered. This runs before the frame token is consumed so the rejection cannot
    /// strand an acquired image.
    pub(crate) fn validate_shadow_sampling(
        &self,
        records: &[MaterialRecord<'_>],
        shadow: Option<&ShadowPrepass<'_>>,
    ) -> Result<(), GraphicsError> {
        let pending_map = match shadow {
            Some(ShadowPrepass::Single(pass)) => Some(pass.map.id()),
            _ => None,
        };
        let pending_array = match shadow {
            Some(ShadowPrepass::Cascaded(pass)) => Some(pass.map.id()),
            _ => None,
        };
        for record in records {
            match record.shadow_map {
                Some(ShadowSource::Map(map)) => {
                    let index = self.shadow_maps.index_of(map.id())?;
                    if !self.shadow_maps[index].rendered && pending_map != Some(map.id()) {
                        return Err(GraphicsError::invalid_request(
                            "material record samples a shadow map that no shadow pass has \
                             rendered",
                        ));
                    }
                }
                Some(ShadowSource::Array(array)) => {
                    let index = self.shadow_map_arrays.index_of(array.id())?;
                    if !self.shadow_map_arrays[index].rendered && pending_array != Some(array.id())
                    {
                        return Err(GraphicsError::invalid_request(
                            "material record samples a shadow map array that no cascaded shadow \
                             pass has rendered",
                        ));
                    }
                }
                None => {}
            }
        }
        Ok(())
    }

    pub(crate) fn draw_material_scene_and_present(
        &mut self,
        token: TexturedFrameToken,
        records: &[MaterialRecord<'_>],
        shadow: Option<&ShadowPrepass<'_>>,
        targets: ResourceId,
        clear: ClearColor,
        depth_clear: f32,
    ) -> Result<FrameDisposition, GraphicsError> {
        let target_index = self.targets.index_of(targets)?;
        if self.targets[target_index].info != token.info {
            return Err(error(
                "render targets do not match acquired Vulkan generation",
            ));
        }
        self.prepare_material_scene(records, &[], shadow)?;
        self.record_draw(
            token.image_index,
            target_index,
            PreparedScene::Materials,
            clear,
            depth_clear,
        )?;
        self.submit_recorded(token.image_index, false)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_material_scene_postprocessed_and_present(
        &mut self,
        token: TexturedFrameToken,
        records: &[MaterialRecord<'_>],
        shadow: Option<&ShadowPrepass<'_>>,
        overlay: &[MaterialRecord<'_>],
        postprocess_pipeline: ResourceId,
        targets: ResourceId,
        clear: ClearColor,
        depth_clear: f32,
    ) -> Result<FrameDisposition, GraphicsError> {
        let postprocess_pipeline_index =
            self.postprocess_pipelines.index_of(postprocess_pipeline)?;
        let target_index = self.postprocess_targets.index_of(targets)?;
        if self.postprocess_targets[target_index].info != token.info {
            return Err(error(
                "postprocess targets do not match acquired Vulkan generation",
            ));
        }
        self.prepare_material_scene(records, overlay, shadow)?;
        let postprocess_descriptor =
            self.postprocess_descriptor_set(postprocess_pipeline_index, target_index, targets)?;
        self.record_postprocessed_draw(
            token.image_index,
            postprocess_pipeline_index,
            target_index,
            postprocess_descriptor,
            PreparedScene::Materials,
            clear,
            depth_clear,
        )?;
        self.submit_recorded(token.image_index, true)
    }

    /// Checks handles and stages per-record data for the scene records, any overlay records,
    /// and any shadow records in that fixed order, so recording consumes the same uniform
    /// slots and storage offsets.
    #[allow(clippy::too_many_lines)]
    fn prepare_material_scene(
        &mut self,
        records: &[MaterialRecord<'_>],
        overlay: &[MaterialRecord<'_>],
        shadow: Option<&ShadowPrepass<'_>>,
    ) -> Result<(), GraphicsError> {
        let shadow_records = shadow.map_or(0, |shadow| shadow.records().count());
        let uniform_slots = records
            .len()
            .checked_add(overlay.len())
            .and_then(|slots| slots.checked_add(shadow_records))
            .ok_or_else(|| error("Vulkan material uniform offsets overflow"))?;
        let last_offset = uniform_slots
            .saturating_sub(1)
            .checked_mul(DRAW_UNIFORM_STRIDE)
            .ok_or_else(|| error("Vulkan material uniform offsets overflow"))?;
        u32::try_from(last_offset)
            .map_err(|_| error("Vulkan material uniform offsets exceed u32"))?;
        self.ensure_uniform_capacity(uniform_slots)?;
        write_material_uniforms(&self.surface, &self.uniform, records, overlay, shadow)?;
        let (storage_offsets, storage_bytes) = material_storage_offsets(records, overlay, shadow)?;
        self.ensure_storage_capacity(storage_bytes)?;
        write_material_storage(
            &self.surface,
            &self.storage,
            records,
            overlay,
            shadow,
            &storage_offsets,
        )?;
        self.stage_transient_geometry(records, overlay)?;
        self.resolved_shadow_draws.clear();
        self.resolved_shadow_cascades.clear();
        self.pending_shadow_target = None;
        if let Some(shadow) = shadow {
            let target = match shadow {
                ShadowPrepass::Single(pass) => {
                    PendingShadowTarget::Map(self.shadow_maps.index_of(pass.map.id())?)
                }
                ShadowPrepass::Cascaded(pass) => {
                    let array_index = self.shadow_map_arrays.index_of(pass.map.id())?;
                    self.resolved_shadow_cascades
                        .extend(pass.cascades.iter().map(|records| records.len()));
                    PendingShadowTarget::Array(array_index)
                }
            };
            for (index, record) in shadow.records().enumerate() {
                let mesh = self.meshes.index_of(record.mesh.id())?;
                let pipeline = self.shadow_pipelines.index_of(record.pipeline.id())?;
                let descriptor = self.shadow_descriptor_set(pipeline)?;
                let slot = records.len() + overlay.len() + index;
                let uniform_offset =
                    u32::try_from(slot * DRAW_UNIFORM_STRIDE).expect("shadow offset was validated");
                let (dynamic_offsets, dynamic_offset_count) = dynamic_offsets_in_binding_order(
                    self.shadow_pipelines[pipeline].uniform,
                    self.shadow_pipelines[pipeline].storage,
                    uniform_offset,
                    storage_offsets[slot],
                );
                self.resolved_shadow_draws.push(ResolvedShadowDraw {
                    mesh,
                    pipeline,
                    descriptor,
                    dynamic_offsets,
                    dynamic_offset_count,
                });
            }
            self.pending_shadow_target = Some(target);
        }
        self.resolved_material_draws.clear();
        self.resolved_overlay_draws.clear();
        let mut sampled_ids = Vec::new();
        let mut texture_indices = Vec::new();
        let mut transient_offset = 0_usize;
        for (index, record) in records.iter().chain(overlay).enumerate() {
            let geometry = match record.geometry {
                GeometrySource::Mesh(mesh) => {
                    ResolvedGeometry::Mesh(self.meshes.index_of(mesh.id())?)
                }
                GeometrySource::Transient(supply) => {
                    let vertex_offset = transient_offset;
                    let index_offset = vertex_offset
                        + supply
                            .vertices
                            .len()
                            .next_multiple_of(STORAGE_OFFSET_ALIGNMENT);
                    transient_offset = index_offset
                        + supply
                            .indices
                            .byte_len()
                            .next_multiple_of(STORAGE_OFFSET_ALIGNMENT);
                    ResolvedGeometry::Transient {
                        vertex_offset: u64::try_from(vertex_offset)
                            .map_err(|_| error("Vulkan transient geometry offset exceeds u64"))?,
                        index_offset: u64::try_from(index_offset)
                            .map_err(|_| error("Vulkan transient geometry offset exceeds u64"))?,
                        index_count: u32::try_from(supply.indices.len()).map_err(|_| {
                            error("Vulkan transient geometry index count exceeds u32")
                        })?,
                        index_type: match supply.indices {
                            MeshIndices::U16(_) => vk::VK_INDEX_TYPE_UINT16,
                            MeshIndices::U32(_) => vk::VK_INDEX_TYPE_UINT32,
                        },
                    }
                }
            };
            let pipeline = self.material_pipelines.index_of(record.pipeline.id())?;
            if index >= records.len()
                && self.material_pipelines[pipeline].overlay_pipeline.is_null()
            {
                return Err(error(
                    "Vulkan material pipeline lacks the overlay variant its record needs",
                ));
            }
            sampled_ids.clear();
            texture_indices.clear();
            for texture in record.textures {
                sampled_ids.push(texture.id());
                texture_indices.push(self.textures.index_of(texture.id())?);
            }
            let shadow_view = match record.shadow_map {
                Some(ShadowSource::Map(map)) => {
                    let map_index = self.shadow_maps.index_of(map.id())?;
                    sampled_ids.push(map.id());
                    Some(ShadowView::Map(self.shadow_maps[map_index].image.view))
                }
                Some(ShadowSource::Array(array)) => {
                    let array_index = self.shadow_map_arrays.index_of(array.id())?;
                    sampled_ids.push(array.id());
                    Some(ShadowView::Array(
                        self.shadow_map_arrays[array_index].array_view,
                    ))
                }
                None => None,
            };
            let descriptor = self.material_descriptor_set(
                pipeline,
                &sampled_ids,
                &texture_indices,
                shadow_view,
            )?;
            let uniform_offset =
                u32::try_from(index * DRAW_UNIFORM_STRIDE).expect("material offset was validated");
            let (dynamic_offsets, dynamic_offset_count) = dynamic_offsets_in_binding_order(
                self.material_pipelines[pipeline].uniform,
                self.material_pipelines[pipeline].storage,
                uniform_offset,
                storage_offsets[index],
            );
            let resolved = ResolvedMaterialDraw {
                geometry,
                pipeline,
                descriptor,
                dynamic_offsets,
                dynamic_offset_count,
            };
            if index < records.len() {
                self.resolved_material_draws.push(resolved);
            } else {
                self.resolved_overlay_draws.push(resolved);
            }
        }
        Ok(())
    }

    fn ensure_storage_capacity(&mut self, required: usize) -> Result<(), GraphicsError> {
        if required <= self.storage_capacity {
            return Ok(());
        }
        let capacity = required
            .checked_next_power_of_two()
            .ok_or_else(|| error("Vulkan record storage capacity overflow"))?;
        self.surface.wait_for_frame()?;
        let replacement = create_buffer(
            &self.surface,
            capacity,
            vk::VK_BUFFER_USAGE_STORAGE_BUFFER_BIT as u32,
            &[],
        )?;
        if let Err(failure) = self
            .reset_descriptor_pools(false)
            .and_then(|()| self.reset_descriptor_pools(true))
        {
            destroy_buffer(&self.surface, replacement);
            return Err(failure);
        }
        let previous = mem::replace(&mut self.storage, replacement);
        destroy_buffer(&self.surface, previous);
        self.storage_capacity = capacity;
        Ok(())
    }

    /// Copies every transient-geometry record supply into the frame's shared geometry region:
    /// per record, aligned vertex bytes followed by aligned index bytes, scene records then
    /// overlay records in record order, so resolution recomputes the same offsets.
    fn stage_transient_geometry(
        &mut self,
        records: &[MaterialRecord<'_>],
        overlay: &[MaterialRecord<'_>],
    ) -> Result<(), GraphicsError> {
        let geometry_bytes = records
            .iter()
            .chain(overlay)
            .filter_map(|record| match record.geometry {
                GeometrySource::Transient(geometry) => Some(geometry),
                GeometrySource::Mesh(_) => None,
            })
            .try_fold(0_usize, |total, geometry| {
                geometry
                    .vertices
                    .len()
                    .checked_next_multiple_of(STORAGE_OFFSET_ALIGNMENT)
                    .and_then(|aligned| total.checked_add(aligned))
                    .and_then(|total| {
                        geometry
                            .indices
                            .byte_len()
                            .checked_next_multiple_of(STORAGE_OFFSET_ALIGNMENT)
                            .and_then(|aligned| total.checked_add(aligned))
                    })
                    .ok_or_else(|| error("Vulkan transient geometry offsets overflow"))
            })?;
        self.ensure_transient_capacity(geometry_bytes)?;
        write_transient_geometry(&self.surface, &self.transient_geometry, records, overlay)
    }

    fn ensure_transient_capacity(&mut self, required: usize) -> Result<(), GraphicsError> {
        if required <= self.transient_capacity {
            return Ok(());
        }
        let capacity = required
            .checked_next_power_of_two()
            .ok_or_else(|| error("Vulkan transient geometry capacity overflow"))?;
        self.surface.wait_for_frame()?;
        let replacement = create_buffer(
            &self.surface,
            capacity,
            (vk::VK_BUFFER_USAGE_VERTEX_BUFFER_BIT | vk::VK_BUFFER_USAGE_INDEX_BUFFER_BIT) as u32,
            &[],
        )?;
        let previous = mem::replace(&mut self.transient_geometry, replacement);
        destroy_buffer(&self.surface, previous);
        self.transient_capacity = capacity;
        Ok(())
    }

    /// Allocates or reuses the uniform-only descriptor set for one shadow pipeline.
    fn shadow_descriptor_set(
        &mut self,
        pipeline_index: usize,
    ) -> Result<vk::VkDescriptorSet, GraphicsError> {
        if let Some(set) = self.shadow_pipelines[pipeline_index].descriptor {
            return Ok(set);
        }
        let pipeline = &self.shadow_pipelines[pipeline_index];
        let (set_layout, descriptor_pool, pipeline_uniform, pipeline_storage) = (
            pipeline.set_layout,
            pipeline.descriptor_pool,
            pipeline.uniform,
            pipeline.storage,
        );
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const set_layout,
            ..Default::default()
        };
        let mut set = ptr::null_mut();
        check(
            unsafe {
                self.surface
                    .device()
                    .functions
                    .allocate_descriptor_sets
                    .expect("loaded function")(
                    self.surface.device().handle,
                    &raw const allocate,
                    &raw mut set,
                )
            },
            "vkAllocateDescriptorSets for shadow record",
        )?;
        let uniform_buffer = pipeline_uniform.map(|(binding, size)| {
            (
                binding,
                vk::VkDescriptorBufferInfo {
                    buffer: self.uniform.handle,
                    offset: 0,
                    range: u64::from(size),
                },
            )
        });
        let storage_buffer = pipeline_storage.map(|(binding, size)| {
            (
                binding,
                vk::VkDescriptorBufferInfo {
                    buffer: self.storage.handle,
                    offset: 0,
                    range: u64::from(size),
                },
            )
        });
        let mut writes = Vec::with_capacity(2);
        if let Some((binding, ref buffer)) = uniform_buffer {
            writes.push(descriptor_write(
                set,
                binding,
                vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
                ptr::from_ref(buffer).cast(),
            ));
        }
        if let Some((binding, ref buffer)) = storage_buffer {
            writes.push(descriptor_write(
                set,
                binding,
                vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC,
                ptr::from_ref(buffer).cast(),
            ));
        }
        if !writes.is_empty() {
            unsafe {
                self.surface
                    .device()
                    .functions
                    .update_descriptor_sets
                    .expect("loaded function")(
                    self.surface.device().handle,
                    u32::try_from(writes.len()).expect("descriptor write count fits u32"),
                    writes.as_ptr(),
                    0,
                    ptr::null(),
                );
            };
        }
        self.shadow_pipelines[pipeline_index].descriptor = Some(set);
        Ok(set)
    }

    fn prepare_scene(&mut self, draws: &[TexturedSceneDraw<'_>]) -> Result<(), GraphicsError> {
        self.pending_shadow_target = None;
        let last_offset = draws
            .len()
            .saturating_sub(1)
            .checked_mul(DRAW_UNIFORM_STRIDE)
            .ok_or_else(|| error("Vulkan scene transform offsets overflow"))?;
        u32::try_from(last_offset)
            .map_err(|_| error("Vulkan scene transform offsets exceed u32"))?;
        self.ensure_uniform_capacity(draws.len())?;
        write_scene_transforms(&self.surface, &self.uniform, draws)?;
        self.resolved_draws.clear();
        for (index, draw) in draws.iter().enumerate() {
            let mesh = self.meshes.index_of(draw.mesh.id())?;
            let texture = self.textures.index_of(draw.texture.id())?;
            let pipeline = self.pipelines.index_of(draw.pipeline.id())?;
            let descriptor = self.descriptor_set(pipeline, texture, draw.texture.id(), false)?;
            self.resolved_draws.push(ResolvedDraw {
                mesh,
                pipeline,
                descriptor,
                dynamic_offset: u32::try_from(index * DRAW_UNIFORM_STRIDE)
                    .expect("scene offset was validated"),
            });
        }
        Ok(())
    }

    fn prepare_instanced_scene(
        &mut self,
        batches: &[TexturedInstanceBatch<'_>],
    ) -> Result<(), GraphicsError> {
        self.pending_shadow_target = None;
        let instance_count = batches.iter().try_fold(0_usize, |total, batch| {
            total
                .checked_add(batch.model_view_projections.len())
                .ok_or_else(|| error("Vulkan instance count exceeds address space"))
        })?;
        self.ensure_instance_capacity(instance_count)?;
        write_instance_transforms(&self.surface, &self.instance_transforms, batches)?;
        self.resolved_instance_batches.clear();
        let mut transform_offset = 0_usize;
        for batch in batches {
            let mesh = self.meshes.index_of(batch.mesh.id())?;
            let texture = self.textures.index_of(batch.texture.id())?;
            let pipeline = self.instanced_pipelines.index_of(batch.pipeline.id())?;
            let descriptor = self.descriptor_set(pipeline, texture, batch.texture.id(), true)?;
            self.resolved_instance_batches.push(ResolvedInstanceBatch {
                mesh,
                pipeline,
                descriptor,
                transform_offset: u64::try_from(transform_offset)
                    .map_err(|_| error("Vulkan instance offset exceeds u64"))?,
                instance_count: u32::try_from(batch.model_view_projections.len())
                    .map_err(|_| error("Vulkan batch instance count exceeds u32"))?,
            });
            transform_offset = transform_offset
                .checked_add(
                    batch
                        .model_view_projections
                        .len()
                        .checked_mul(INSTANCE_TRANSFORM_SIZE)
                        .ok_or_else(|| error("Vulkan instance offset overflow"))?,
                )
                .ok_or_else(|| error("Vulkan instance offset overflow"))?;
        }
        Ok(())
    }

    fn ensure_uniform_capacity(&mut self, required: usize) -> Result<(), GraphicsError> {
        if required <= self.uniform_capacity {
            return Ok(());
        }
        let capacity = required
            .checked_next_power_of_two()
            .ok_or_else(|| error("Vulkan scene transform capacity overflow"))?;
        let bytes = capacity
            .checked_mul(DRAW_UNIFORM_STRIDE)
            .ok_or_else(|| error("Vulkan scene transform storage is too large"))?;
        self.surface.wait_for_frame()?;
        let replacement = create_buffer(
            &self.surface,
            bytes,
            vk::VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT as u32,
            &[],
        )?;
        if let Err(failure) = self
            .reset_descriptor_pools(false)
            .and_then(|()| self.reset_descriptor_pools(true))
        {
            destroy_buffer(&self.surface, replacement);
            return Err(failure);
        }
        let previous = mem::replace(&mut self.uniform, replacement);
        destroy_buffer(&self.surface, previous);
        self.uniform_capacity = capacity;
        Ok(())
    }

    fn ensure_instance_capacity(&mut self, required: usize) -> Result<(), GraphicsError> {
        if required <= self.instance_capacity {
            return Ok(());
        }
        let capacity = required
            .checked_next_power_of_two()
            .ok_or_else(|| error("Vulkan instance transform capacity overflow"))?;
        let bytes = capacity
            .checked_mul(INSTANCE_TRANSFORM_SIZE)
            .ok_or_else(|| error("Vulkan instance transform storage is too large"))?;
        self.surface.wait_for_frame()?;
        let replacement = create_buffer(
            &self.surface,
            bytes,
            vk::VK_BUFFER_USAGE_VERTEX_BUFFER_BIT as u32,
            &[],
        )?;
        let previous = mem::replace(&mut self.instance_transforms, replacement);
        destroy_buffer(&self.surface, previous);
        self.instance_capacity = capacity;
        Ok(())
    }

    pub(crate) fn abandon(
        &mut self,
        _token: TexturedFrameToken,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.surface.abandon()
    }

    pub(crate) fn defer_abandon(&mut self, token: TexturedFrameToken) {
        self.deferred_token = Some(token);
    }

    fn flush_deferred_abandon(&mut self) -> Result<(), GraphicsError> {
        if let Some(token) = self.deferred_token.take() {
            self.abandon(token)?;
        }
        Ok(())
    }

    pub(crate) fn reclaim_resources(
        &mut self,
        requests: &[DestroyRequest],
    ) -> Result<(), GraphicsError> {
        if requests.is_empty() {
            return Ok(());
        }
        self.surface.wait_for_frame()?;
        let reset_scene_descriptors = requests.iter().any(|request| {
            (request.kind == ResourceKind::Texture && self.textures.get(request.id).is_ok())
                || (request.kind == ResourceKind::ShadowMap
                    && self.shadow_maps.get(request.id).is_ok())
                || (request.kind == ResourceKind::ShadowMapArray
                    && self.shadow_map_arrays.get(request.id).is_ok())
        });
        let reset_postprocess_descriptors = requests.iter().any(|request| {
            request.kind == ResourceKind::PostprocessTargets
                && self.postprocess_targets.get(request.id).is_ok()
        });
        if reset_scene_descriptors {
            self.reset_descriptor_pools(false)?;
        }
        if reset_postprocess_descriptors {
            self.reset_descriptor_pools(true)?;
        }
        for &request in requests {
            self.destroy_resource_if_live(request);
        }
        Ok(())
    }

    pub(crate) fn destroy_resource(
        &mut self,
        request: DestroyRequest,
    ) -> Result<(), GraphicsError> {
        self.surface.wait_for_frame()?;
        if request.kind == ResourceKind::Texture {
            self.textures.get(request.id)?;
            self.reset_descriptor_pools(false)?;
        } else if request.kind == ResourceKind::ShadowMap {
            self.shadow_maps.get(request.id)?;
            self.reset_descriptor_pools(false)?;
        } else if request.kind == ResourceKind::ShadowMapArray {
            self.shadow_map_arrays.get(request.id)?;
            self.reset_descriptor_pools(false)?;
        } else if request.kind == ResourceKind::PostprocessTargets {
            self.postprocess_targets.get(request.id)?;
            self.reset_descriptor_pools(true)?;
        }
        let device = self.surface.device();
        match request.kind {
            ResourceKind::Mesh => destroy_mesh_device(device, self.meshes.remove(request.id)?),
            ResourceKind::Texture => {
                destroy_texture_device(device, self.textures.remove(request.id)?);
            }
            ResourceKind::TexturedPipeline => {
                destroy_pipeline_device(device, self.pipelines.remove(request.id)?);
            }
            ResourceKind::InstancedTexturedPipeline => {
                destroy_pipeline_device(device, self.instanced_pipelines.remove(request.id)?);
            }
            ResourceKind::PostprocessPipeline => {
                destroy_pipeline_device(device, self.postprocess_pipelines.remove(request.id)?);
            }
            ResourceKind::MaterialPipeline => {
                destroy_material_pipeline_device(
                    device,
                    self.material_pipelines.remove(request.id)?,
                );
            }
            ResourceKind::ShadowMap => {
                let map = self.shadow_maps.remove(request.id)?;
                unsafe { destroy_image_device(device, map.image) };
            }
            ResourceKind::ShadowMapArray => {
                destroy_shadow_map_array_device(device, self.shadow_map_arrays.remove(request.id)?);
            }
            ResourceKind::ShadowPipeline => {
                destroy_shadow_pipeline_device(device, self.shadow_pipelines.remove(request.id)?);
            }
            ResourceKind::RenderTargets => {
                destroy_target_device(device, self.targets.remove(request.id)?);
            }
            ResourceKind::PostprocessTargets => destroy_postprocess_target_device(
                device,
                self.postprocess_targets.remove(request.id)?,
            ),
        }
        Ok(())
    }

    fn destroy_resource_if_live(&mut self, request: DestroyRequest) {
        let device = self.surface.device();
        match request.kind {
            ResourceKind::Mesh => self
                .meshes
                .remove_if_live(request.id)
                .map(|resource| destroy_mesh_device(device, resource)),
            ResourceKind::Texture => self
                .textures
                .remove_if_live(request.id)
                .map(|resource| destroy_texture_device(device, resource)),
            ResourceKind::TexturedPipeline => self
                .pipelines
                .remove_if_live(request.id)
                .map(|resource| destroy_pipeline_device(device, resource)),
            ResourceKind::InstancedTexturedPipeline => self
                .instanced_pipelines
                .remove_if_live(request.id)
                .map(|resource| destroy_pipeline_device(device, resource)),
            ResourceKind::PostprocessPipeline => self
                .postprocess_pipelines
                .remove_if_live(request.id)
                .map(|resource| destroy_pipeline_device(device, resource)),
            ResourceKind::MaterialPipeline => self
                .material_pipelines
                .remove_if_live(request.id)
                .map(|resource| destroy_material_pipeline_device(device, resource)),
            ResourceKind::ShadowMap => self
                .shadow_maps
                .remove_if_live(request.id)
                .map(|resource| unsafe { destroy_image_device(device, resource.image) }),
            ResourceKind::ShadowMapArray => self
                .shadow_map_arrays
                .remove_if_live(request.id)
                .map(|resource| destroy_shadow_map_array_device(device, resource)),
            ResourceKind::ShadowPipeline => self
                .shadow_pipelines
                .remove_if_live(request.id)
                .map(|resource| destroy_shadow_pipeline_device(device, resource)),
            ResourceKind::RenderTargets => self
                .targets
                .remove_if_live(request.id)
                .map(|resource| destroy_target_device(device, resource)),
            ResourceKind::PostprocessTargets => self
                .postprocess_targets
                .remove_if_live(request.id)
                .map(|resource| destroy_postprocess_target_device(device, resource)),
        };
    }

    fn reset_descriptor_pools(&mut self, postprocess: bool) -> Result<(), GraphicsError> {
        let device = self.surface.device();
        if postprocess {
            for pipeline in self.postprocess_pipelines.iter_mut() {
                let replacement = create_postprocess_descriptor_pool(device)?;
                unsafe {
                    device
                        .functions
                        .destroy_descriptor_pool
                        .expect("loaded function")(
                        device.handle,
                        pipeline.descriptor_pool,
                        ptr::null(),
                    );
                }
                pipeline.descriptor_pool = replacement;
                pipeline.bindings.clear();
            }
            return Ok(());
        }
        for pipeline in self
            .pipelines
            .iter_mut()
            .chain(self.instanced_pipelines.iter_mut())
        {
            let replacement = create_descriptor_pool(device)?;
            unsafe {
                device
                    .functions
                    .destroy_descriptor_pool
                    .expect("loaded function")(
                    device.handle,
                    pipeline.descriptor_pool,
                    ptr::null(),
                );
            }
            pipeline.descriptor_pool = replacement;
            pipeline.bindings.clear();
        }
        for pipeline in self.material_pipelines.iter_mut() {
            let replacement = create_material_descriptor_pool(device)?;
            unsafe {
                device
                    .functions
                    .destroy_descriptor_pool
                    .expect("loaded function")(
                    device.handle,
                    pipeline.descriptor_pool,
                    ptr::null(),
                );
            }
            pipeline.descriptor_pool = replacement;
            pipeline.bindings.clear();
        }
        for pipeline in self.shadow_pipelines.iter_mut() {
            let replacement = create_material_descriptor_pool(device)?;
            unsafe {
                device
                    .functions
                    .destroy_descriptor_pool
                    .expect("loaded function")(
                    device.handle,
                    pipeline.descriptor_pool,
                    ptr::null(),
                );
            }
            pipeline.descriptor_pool = replacement;
            pipeline.descriptor = None;
        }
        Ok(())
    }

    pub(crate) fn shutdown(mut self) -> Result<(), GraphicsError> {
        self.flush_deferred_abandon()?;
        let result = self.surface.finish();
        self.destroy_resources();
        let surface = unsafe { ptr::read(&raw const self.surface) };
        mem::forget(self);
        result.and(surface.shutdown())
    }

    fn upload_texture(
        &mut self,
        staging: &Buffer,
        image: &Image,
        width: u32,
        height: u32,
        levels: &[&[u8]],
    ) -> Result<(), GraphicsError> {
        let mip_levels =
            u32::try_from(levels.len()).map_err(|_| error("mip chain length exceeds u32"))?;
        let mut regions = Vec::with_capacity(levels.len());
        let mut buffer_offset = 0_u64;
        for (level, texels) in levels.iter().enumerate() {
            let level = u32::try_from(level).map_err(|_| error("mip chain length exceeds u32"))?;
            regions.push(vk::VkBufferImageCopy2 {
                sType: vk::VK_STRUCTURE_TYPE_BUFFER_IMAGE_COPY_2,
                bufferOffset: buffer_offset,
                imageSubresource: vk::VkImageSubresourceLayers {
                    aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
                    mipLevel: level,
                    layerCount: 1,
                    ..Default::default()
                },
                imageExtent: vk::VkExtent3D {
                    width: mip_extent(width, level),
                    height: mip_extent(height, level),
                    depth: 1,
                },
                ..Default::default()
            });
            buffer_offset += u64::try_from(texels.len())
                .map_err(|_| error("mip level bytes exceed Vulkan address space"))?;
        }
        self.begin_upload()?;
        let to_transfer = image_barrier(
            image.handle,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
            color_subresource_levels(mip_levels),
        );
        pipeline_barrier(&self.surface, &to_transfer);
        let copy = vk::VkCopyBufferToImageInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_COPY_BUFFER_TO_IMAGE_INFO_2,
            srcBuffer: staging.handle,
            dstImage: image.handle,
            dstImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            regionCount: u32::try_from(regions.len()).expect("mip chain length fits u32"),
            pRegions: regions.as_ptr(),
            ..Default::default()
        };
        unsafe {
            self.surface
                .device()
                .functions
                .cmd_copy_buffer_to_image2
                .expect("loaded function")(self.surface.command_buffer, &raw const copy);
        }
        let to_sampled = image_barrier(
            image.handle,
            vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
            vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
            vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
            color_subresource_levels(mip_levels),
        );
        pipeline_barrier(&self.surface, &to_sampled);
        self.end_upload()
    }

    fn begin_upload(&mut self) -> Result<(), GraphicsError> {
        self.surface.wait_for_frame()?;
        let device = self.surface.device();
        check(
            unsafe {
                device.functions.reset_fences.expect("loaded function")(
                    device.handle,
                    1,
                    &raw const self.surface.frame_fence,
                )
            },
            "vkResetFences for resource upload",
        )?;
        check(
            unsafe {
                device
                    .functions
                    .reset_command_buffer
                    .expect("loaded function")(self.surface.command_buffer, 0)
            },
            "vkResetCommandBuffer for resource upload",
        )?;
        let begin = vk::VkCommandBufferBeginInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            flags: vk::VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT as u32,
            ..Default::default()
        };
        check(
            unsafe {
                device
                    .functions
                    .begin_command_buffer
                    .expect("loaded function")(
                    self.surface.command_buffer, &raw const begin
                )
            },
            "vkBeginCommandBuffer for resource upload",
        )
    }

    fn end_upload(&mut self) -> Result<(), GraphicsError> {
        let device = self.surface.device();
        check(
            unsafe {
                device
                    .functions
                    .end_command_buffer
                    .expect("loaded function")(self.surface.command_buffer)
            },
            "vkEndCommandBuffer for resource upload",
        )?;
        let command = vk::VkCommandBufferSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO,
            commandBuffer: self.surface.command_buffer,
            ..Default::default()
        };
        let submit = vk::VkSubmitInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_SUBMIT_INFO_2,
            commandBufferInfoCount: 1,
            pCommandBufferInfos: &raw const command,
            ..Default::default()
        };
        check(
            unsafe {
                device.functions.queue_submit2.expect("loaded function")(
                    device.queue,
                    1,
                    &raw const submit,
                    self.surface.frame_fence,
                )
            },
            "vkQueueSubmit2 for resource upload",
        )?;
        self.surface.frame_pending = true;
        self.surface.wait_for_frame()
    }

    fn descriptor_set(
        &mut self,
        pipeline_index: usize,
        texture_index: usize,
        texture_id: ResourceId,
        instanced: bool,
    ) -> Result<vk::VkDescriptorSet, GraphicsError> {
        let pipelines = if instanced {
            &mut self.instanced_pipelines
        } else {
            &mut self.pipelines
        };
        if let Some((_, set)) = pipelines[pipeline_index]
            .bindings
            .iter()
            .find(|(id, _)| *id == texture_id)
        {
            return Ok(*set);
        }
        let pipeline = &mut pipelines[pipeline_index];
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: pipeline.descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const pipeline.set_layout,
            ..Default::default()
        };
        let mut set = ptr::null_mut();
        check(
            unsafe {
                self.surface
                    .device()
                    .functions
                    .allocate_descriptor_sets
                    .expect("loaded function")(
                    self.surface.device().handle,
                    &raw const allocate,
                    &raw mut set,
                )
            },
            "vkAllocateDescriptorSets for texture",
        )?;
        let buffer = vk::VkDescriptorBufferInfo {
            buffer: self.uniform.handle,
            offset: 0,
            range: DRAW_UNIFORM_SIZE as u64,
        };
        let image = vk::VkDescriptorImageInfo {
            sampler: ptr::null_mut(),
            imageView: self.textures[texture_index].image.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        };
        let sampler = vk::VkDescriptorImageInfo {
            sampler: self.textures[texture_index].sampler,
            ..Default::default()
        };
        let writes = [
            descriptor_write(
                set,
                0,
                vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
                (&raw const buffer).cast(),
            ),
            descriptor_write(
                set,
                1,
                vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
                (&raw const image).cast(),
            ),
            descriptor_write(
                set,
                2,
                vk::VK_DESCRIPTOR_TYPE_SAMPLER,
                (&raw const sampler).cast(),
            ),
        ];
        unsafe {
            self.surface
                .device()
                .functions
                .update_descriptor_sets
                .expect("loaded function")(
                self.surface.device().handle,
                3,
                writes.as_ptr(),
                0,
                ptr::null(),
            );
        };
        pipeline.bindings.push((texture_id, set));
        Ok(set)
    }

    #[allow(clippy::too_many_lines)]
    fn material_descriptor_set(
        &mut self,
        pipeline_index: usize,
        sampled_ids: &[ResourceId],
        texture_indices: &[usize],
        shadow_view: Option<ShadowView>,
    ) -> Result<vk::VkDescriptorSet, GraphicsError> {
        if let Some((_, set)) = self.material_pipelines[pipeline_index]
            .bindings
            .iter()
            .find(|(ids, _)| ids.as_slice() == sampled_ids)
        {
            return Ok(*set);
        }
        let pipeline = &self.material_pipelines[pipeline_index];
        let (set_layout, descriptor_pool) = (pipeline.set_layout, pipeline.descriptor_pool);
        let pipeline_uniform = pipeline.uniform;
        let pipeline_storage = pipeline.storage;
        let texture_bindings = pipeline.texture_bindings.clone();
        let samplers = pipeline.samplers.clone();
        let depth_texture_binding = pipeline.depth_texture_binding;
        let depth_texture_array_binding = pipeline.depth_texture_array_binding;
        let comparison_sampler = pipeline.comparison_sampler;
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const set_layout,
            ..Default::default()
        };
        let mut set = ptr::null_mut();
        check(
            unsafe {
                self.surface
                    .device()
                    .functions
                    .allocate_descriptor_sets
                    .expect("loaded function")(
                    self.surface.device().handle,
                    &raw const allocate,
                    &raw mut set,
                )
            },
            "vkAllocateDescriptorSets for material record",
        )?;
        let buffer = vk::VkDescriptorBufferInfo {
            buffer: self.uniform.handle,
            offset: 0,
            range: pipeline_uniform.map_or(1, |(_, size)| u64::from(size)),
        };
        let storage_buffer = vk::VkDescriptorBufferInfo {
            buffer: self.storage.handle,
            offset: 0,
            range: pipeline_storage.map_or(1, |(_, size)| u64::from(size)),
        };
        let images: Vec<vk::VkDescriptorImageInfo> = texture_indices
            .iter()
            .map(|&index| vk::VkDescriptorImageInfo {
                sampler: ptr::null_mut(),
                imageView: self.textures[index].image.view,
                imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            })
            .collect();
        let sampler_infos: Vec<vk::VkDescriptorImageInfo> = samplers
            .iter()
            .map(|&(_, sampler)| vk::VkDescriptorImageInfo {
                sampler,
                ..Default::default()
            })
            .collect();
        let shadow_image = match shadow_view {
            Some(ShadowView::Map(view)) => depth_texture_binding.map(|binding| (binding, view)),
            Some(ShadowView::Array(view)) => {
                depth_texture_array_binding.map(|binding| (binding, view))
            }
            None => None,
        }
        .map(|(binding, view)| {
            (
                binding,
                vk::VkDescriptorImageInfo {
                    sampler: ptr::null_mut(),
                    imageView: view,
                    imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                },
            )
        });
        let comparison_info = comparison_sampler.map(|(_, sampler)| vk::VkDescriptorImageInfo {
            sampler,
            ..Default::default()
        });
        let mut writes = Vec::with_capacity(3 + images.len() + sampler_infos.len());
        if let Some((binding, _)) = pipeline_uniform {
            writes.push(descriptor_write(
                set,
                binding,
                vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
                (&raw const buffer).cast(),
            ));
        }
        if let Some((binding, _)) = pipeline_storage {
            writes.push(descriptor_write(
                set,
                binding,
                vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC,
                (&raw const storage_buffer).cast(),
            ));
        }
        for (image, &binding) in images.iter().zip(texture_bindings.iter()) {
            writes.push(descriptor_write(
                set,
                binding,
                vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
                ptr::from_ref(image).cast(),
            ));
        }
        for (info, &(binding, _)) in sampler_infos.iter().zip(samplers.iter()) {
            writes.push(descriptor_write(
                set,
                binding,
                vk::VK_DESCRIPTOR_TYPE_SAMPLER,
                ptr::from_ref(info).cast(),
            ));
        }
        if let Some((binding, ref image)) = shadow_image {
            writes.push(descriptor_write(
                set,
                binding,
                vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
                ptr::from_ref(image).cast(),
            ));
        }
        if let (Some(info), Some((binding, _))) = (comparison_info.as_ref(), comparison_sampler) {
            writes.push(descriptor_write(
                set,
                binding,
                vk::VK_DESCRIPTOR_TYPE_SAMPLER,
                ptr::from_ref(info).cast(),
            ));
        }
        unsafe {
            self.surface
                .device()
                .functions
                .update_descriptor_sets
                .expect("loaded function")(
                self.surface.device().handle,
                u32::try_from(writes.len()).expect("descriptor write count fits u32"),
                writes.as_ptr(),
                0,
                ptr::null(),
            );
        };
        self.material_pipelines[pipeline_index]
            .bindings
            .push((sampled_ids.to_vec(), set));
        Ok(set)
    }

    fn postprocess_descriptor_set(
        &mut self,
        pipeline_index: usize,
        target_index: usize,
        target_id: ResourceId,
    ) -> Result<vk::VkDescriptorSet, GraphicsError> {
        if let Some((_, set)) = self.postprocess_pipelines[pipeline_index]
            .bindings
            .iter()
            .find(|(id, _)| *id == target_id)
        {
            return Ok(*set);
        }
        let scene_color = self.postprocess_targets[target_index]
            .scene_color
            .ok_or_else(|| error("postprocess targets were reclaimed by a newer generation"))?;
        let pipeline = &mut self.postprocess_pipelines[pipeline_index];
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: pipeline.descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const pipeline.set_layout,
            ..Default::default()
        };
        let mut set = ptr::null_mut();
        check(
            unsafe {
                self.surface
                    .device()
                    .functions
                    .allocate_descriptor_sets
                    .expect("loaded function")(
                    self.surface.device().handle,
                    &raw const allocate,
                    &raw mut set,
                )
            },
            "vkAllocateDescriptorSets for postprocess target",
        )?;
        let buffer = vk::VkDescriptorBufferInfo {
            buffer: self.uniform.handle,
            offset: 0,
            range: 64,
        };
        let image = vk::VkDescriptorImageInfo {
            sampler: ptr::null_mut(),
            imageView: scene_color.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        };
        let sampler = vk::VkDescriptorImageInfo {
            sampler: pipeline.sampler,
            ..Default::default()
        };
        let writes = [
            descriptor_write(
                set,
                0,
                vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
                (&raw const buffer).cast(),
            ),
            descriptor_write(
                set,
                1,
                vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
                (&raw const image).cast(),
            ),
            descriptor_write(
                set,
                2,
                vk::VK_DESCRIPTOR_TYPE_SAMPLER,
                (&raw const sampler).cast(),
            ),
        ];
        unsafe {
            self.surface
                .device()
                .functions
                .update_descriptor_sets
                .expect("loaded function")(
                self.surface.device().handle,
                3,
                writes.as_ptr(),
                0,
                ptr::null(),
            );
        }
        pipeline.bindings.push((target_id, set));
        Ok(set)
    }

    fn collect_gpu_timing(&mut self) -> Result<(), GraphicsError> {
        let Some(pending) = self.gpu_timing.pending.take() else {
            return Ok(());
        };
        let mut values = [0_u64; GPU_QUERY_COUNT as usize];
        let device = self.surface.device();
        check(
            unsafe {
                device
                    .functions
                    .get_query_pool_results
                    .expect("loaded function")(
                    device.handle,
                    self.gpu_timing.query_pool,
                    0,
                    GPU_QUERY_COUNT,
                    mem::size_of_val(&values),
                    values.as_mut_ptr().cast(),
                    u64::try_from(mem::size_of::<u64>()).expect("u64 size fits VkDeviceSize"),
                    vk::VK_QUERY_RESULT_64_BIT.cast_unsigned(),
                )
            },
            "vkGetQueryPoolResults for GPU frame diagnostics",
        )?;
        if !self.gpu_timing.enabled {
            return Ok(());
        }
        let mut scopes = Vec::with_capacity(4);
        scopes.push(self.gpu_scope_timing(
            GpuTimingScope::Frame,
            values[FRAME_QUERY_START as usize],
            values[FRAME_QUERY_START as usize + 1],
        ));
        if pending.has_shadow {
            scopes.push(self.gpu_scope_timing(
                GpuTimingScope::Shadow,
                values[SHADOW_QUERY_START as usize],
                values[SHADOW_QUERY_START as usize + 1],
            ));
        }
        scopes.push(self.gpu_scope_timing(
            GpuTimingScope::Scene,
            values[SCENE_QUERY_START as usize],
            values[SCENE_QUERY_START as usize + 1],
        ));
        if pending.has_postprocess {
            scopes.push(self.gpu_scope_timing(
                GpuTimingScope::Postprocess,
                values[POSTPROCESS_QUERY_START as usize],
                values[POSTPROCESS_QUERY_START as usize + 1],
            ));
        }
        if self.gpu_timing.completed.len() >= GPU_TIMING_FEEDBACK_CAP {
            self.gpu_timing.completed.pop_front();
        }
        self.gpu_timing
            .completed
            .push_back(GpuFrameTiming::new(pending.frame_index, scopes));
        Ok(())
    }

    #[allow(clippy::cast_precision_loss)]
    fn gpu_scope_timing(&self, scope: GpuTimingScope, start: u64, end: u64) -> GpuScopeTiming {
        let ticks = timestamp_tick_delta(
            start,
            end,
            self.surface.device().adapter.timestamp_valid_bits,
        );
        let seconds = ticks as f64 * f64::from(self.surface.device().adapter.timestamp_period)
            / 1_000_000_000.0;
        GpuScopeTiming::new(scope, Duration::from_secs_f64(seconds))
    }

    fn begin_gpu_region(&self, name: &core::ffi::CStr, color: [f32; 4], start_query: u32) {
        if !self.gpu_timing.enabled || self.gpu_timing.query_pool.is_null() {
            return;
        }
        let label = vk::VkDebugUtilsLabelEXT {
            sType: vk::VK_STRUCTURE_TYPE_DEBUG_UTILS_LABEL_EXT,
            pLabelName: name.as_ptr(),
            color,
            ..Default::default()
        };
        unsafe {
            let functions = &self.surface.device().functions;
            functions
                .cmd_begin_debug_utils_label
                .expect("loaded function")(
                self.surface.command_buffer, &raw const label
            );
            functions.cmd_write_timestamp2.expect("loaded function")(
                self.surface.command_buffer,
                vk::VK_PIPELINE_STAGE_2_TOP_OF_PIPE_BIT,
                self.gpu_timing.query_pool,
                start_query,
            );
        }
    }

    fn end_gpu_region(&self, end_query: u32) {
        if !self.gpu_timing.enabled || self.gpu_timing.query_pool.is_null() {
            return;
        }
        unsafe {
            let functions = &self.surface.device().functions;
            functions.cmd_write_timestamp2.expect("loaded function")(
                self.surface.command_buffer,
                vk::VK_PIPELINE_STAGE_2_BOTTOM_OF_PIPE_BIT,
                self.gpu_timing.query_pool,
                end_query,
            );
            functions
                .cmd_end_debug_utils_label
                .expect("loaded function")(self.surface.command_buffer);
        }
    }

    fn begin_gpu_frame(&self) {
        if !self.gpu_timing.enabled || self.gpu_timing.query_pool.is_null() {
            return;
        }
        unsafe {
            self.surface
                .device()
                .functions
                .cmd_reset_query_pool
                .expect("loaded function")(
                self.surface.command_buffer,
                self.gpu_timing.query_pool,
                0,
                GPU_QUERY_COUNT,
            );
        }
        self.begin_gpu_region(c"frame", [0.2, 0.55, 1.0, 1.0], FRAME_QUERY_START);
    }

    fn write_empty_gpu_region(&self, start_query: u32) {
        if !self.gpu_timing.enabled || self.gpu_timing.query_pool.is_null() {
            return;
        }
        unsafe {
            let write = self
                .surface
                .device()
                .functions
                .cmd_write_timestamp2
                .expect("loaded function");
            write(
                self.surface.command_buffer,
                vk::VK_PIPELINE_STAGE_2_TOP_OF_PIPE_BIT,
                self.gpu_timing.query_pool,
                start_query,
            );
            write(
                self.surface.command_buffer,
                vk::VK_PIPELINE_STAGE_2_BOTTOM_OF_PIPE_BIT,
                self.gpu_timing.query_pool,
                start_query + 1,
            );
        }
    }

    fn submit_recorded(
        &mut self,
        image_index: u32,
        has_postprocess: bool,
    ) -> Result<FrameDisposition, GraphicsError> {
        let frame_index = self.surface.presented_count;
        let disposition = self.surface.submit_recorded(image_index)?;
        if self.gpu_timing.enabled && !self.gpu_timing.query_pool.is_null() {
            self.gpu_timing.pending = Some(PendingGpuTiming {
                frame_index,
                has_shadow: self.recorded_has_shadow,
                has_postprocess,
            });
        }
        Ok(disposition)
    }

    /// Encodes the prepared depth-only shadow work — one pass for a single map, or one pass per
    /// cascade layer — into the open command buffer, transitioning the target from its prior
    /// state into depth rendering and out to fragment-sampled reading.
    fn record_shadow_pass_if_pending(&mut self) {
        let Some(target) = self.pending_shadow_target.take() else {
            return;
        };
        let (handle, layers, rendered_before) = match target {
            PendingShadowTarget::Map(index) => {
                let rendered_before = self.shadow_maps[index].rendered;
                self.shadow_maps[index].rendered = true;
                (self.shadow_maps[index].image.handle, 1, rendered_before)
            }
            PendingShadowTarget::Array(index) => {
                let rendered_before = self.shadow_map_arrays[index].rendered;
                self.shadow_map_arrays[index].rendered = true;
                (
                    self.shadow_map_arrays[index].image,
                    self.shadow_map_arrays[index].layers,
                    rendered_before,
                )
            }
        };
        let (old_layout, src_stage, src_access) = if rendered_before {
            (
                vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
                vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
            )
        } else {
            (
                vk::VK_IMAGE_LAYOUT_UNDEFINED,
                vk::VK_PIPELINE_STAGE_2_NONE,
                vk::VK_ACCESS_2_NONE,
            )
        };
        let to_attachment = image_barrier(
            handle,
            old_layout,
            vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            src_stage,
            vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
            src_access,
            vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
            depth_subresource_layers(layers),
        );
        pipeline_barrier(&self.surface, &to_attachment);
        match target {
            PendingShadowTarget::Map(index) => {
                let map = &self.shadow_maps[index];
                self.record_shadow_layer(map.image.view, map.size, &self.resolved_shadow_draws);
            }
            PendingShadowTarget::Array(index) => {
                let array = &self.shadow_map_arrays[index];
                let mut start = 0_usize;
                for (&view, &count) in array.layer_views.iter().zip(&self.resolved_shadow_cascades)
                {
                    self.record_shadow_layer(
                        view,
                        array.size,
                        &self.resolved_shadow_draws[start..start + count],
                    );
                    start += count;
                }
            }
        }
        let to_sampled = image_barrier(
            handle,
            vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
            vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
            vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
            vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
            depth_subresource_layers(layers),
        );
        pipeline_barrier(&self.surface, &to_sampled);
    }

    /// Encodes one depth-only pass into a shadow target view — the whole map, or one array
    /// layer — clearing it to the far plane before its records.
    #[allow(clippy::cast_precision_loss)]
    fn record_shadow_layer(&self, view: vk::VkImageView, size: u32, draws: &[ResolvedShadowDraw]) {
        let depth_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: view,
            imageLayout: vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: vk::VkClearValue {
                depthStencil: vk::VkClearDepthStencilValue {
                    depth: 1.0,
                    stencil: 0,
                },
            },
            ..Default::default()
        };
        let area = vk::VkRect2D {
            offset: vk::VkOffset2D { x: 0, y: 0 },
            extent: vk::VkExtent2D {
                width: size,
                height: size,
            },
        };
        let rendering = vk::VkRenderingInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
            renderArea: area,
            layerCount: 1,
            pDepthAttachment: &raw const depth_attachment,
            ..Default::default()
        };
        let viewport = vk::VkViewport {
            x: 0.0,
            y: 0.0,
            width: size as f32,
            height: size as f32,
            minDepth: 0.0,
            maxDepth: 1.0,
        };
        let device = self.surface.device();
        unsafe {
            let functions = &device.functions;
            functions.cmd_begin_rendering.expect("loaded function")(
                self.surface.command_buffer,
                &raw const rendering,
            );
            functions.cmd_set_viewport.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const viewport,
            );
            functions.cmd_set_scissor.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const area,
            );
            let offset = 0_u64;
            for draw in draws {
                let mesh = &self.meshes[draw.mesh];
                let pipeline = &self.shadow_pipelines[draw.pipeline];
                functions.cmd_bind_pipeline.expect("loaded function")(
                    self.surface.command_buffer,
                    vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                    pipeline.pipeline,
                );
                functions.cmd_bind_descriptor_sets.expect("loaded function")(
                    self.surface.command_buffer,
                    vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                    pipeline.layout,
                    0,
                    1,
                    &raw const draw.descriptor,
                    draw.dynamic_offset_count,
                    draw.dynamic_offsets.as_ptr(),
                );
                functions.cmd_bind_vertex_buffers.expect("loaded function")(
                    self.surface.command_buffer,
                    0,
                    1,
                    &raw const mesh.vertices.handle,
                    &raw const offset,
                );
                functions.cmd_bind_index_buffer.expect("loaded function")(
                    self.surface.command_buffer,
                    mesh.indices.handle,
                    0,
                    mesh.index_type,
                );
                functions
                    .cmd_draw_indexed_indirect
                    .expect("loaded function")(
                    self.surface.command_buffer,
                    mesh.indirect.handle,
                    0,
                    1,
                    u32::try_from(mem::size_of::<vk::VkDrawIndexedIndirectCommand>())
                        .expect("indirect command size fits u32"),
                );
            }
            functions.cmd_end_rendering.expect("loaded function")(self.surface.command_buffer);
        }
    }

    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    fn record_draw(
        &mut self,
        image_index: u32,
        target_index: usize,
        scene: PreparedScene,
        clear: ClearColor,
        depth_clear: f32,
    ) -> Result<(), GraphicsError> {
        let slot = usize::try_from(image_index).map_err(|_| error("invalid image index"))?;
        let target_depth = self.targets[target_index]
            .depth
            .ok_or_else(|| error("render targets were reclaimed by a newer surface generation"))?;
        let multisample_color = self.targets[target_index].multisample_color;
        let image = self.surface.swapchain.images[slot];
        let view = self.surface.swapchain.views[slot];
        let old_layout = if self.surface.swapchain.initialized[slot] {
            vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR
        } else {
            vk::VK_IMAGE_LAYOUT_UNDEFINED
        };
        self.surface.wait_for_frame()?;
        self.collect_gpu_timing()?;
        let device = self.surface.device();
        check(
            unsafe {
                device.functions.reset_fences.expect("loaded function")(
                    device.handle,
                    1,
                    &raw const self.surface.frame_fence,
                )
            },
            "vkResetFences for textured frame",
        )?;
        check(
            unsafe {
                device
                    .functions
                    .reset_command_buffer
                    .expect("loaded function")(self.surface.command_buffer, 0)
            },
            "vkResetCommandBuffer for textured frame",
        )?;
        let begin = vk::VkCommandBufferBeginInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            flags: vk::VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT as u32,
            ..Default::default()
        };
        check(
            unsafe {
                device
                    .functions
                    .begin_command_buffer
                    .expect("loaded function")(
                    self.surface.command_buffer, &raw const begin
                )
            },
            "vkBeginCommandBuffer for textured frame",
        )?;
        self.begin_gpu_frame();
        self.recorded_has_shadow = self.pending_shadow_target.is_some();
        if self.recorded_has_shadow {
            self.begin_gpu_region(c"shadow", [0.55, 0.25, 0.8, 1.0], SHADOW_QUERY_START);
            self.record_shadow_pass_if_pending();
            self.end_gpu_region(SHADOW_QUERY_START + 1);
        } else {
            self.write_empty_gpu_region(SHADOW_QUERY_START);
        }
        let device = self.surface.device();
        let color_barrier = image_barrier(
            image,
            old_layout,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            if old_layout == vk::VK_IMAGE_LAYOUT_UNDEFINED {
                vk::VK_PIPELINE_STAGE_2_NONE
            } else {
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT
            },
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            color_subresource_range(),
        );
        let depth_barrier = image_barrier(
            target_depth.handle,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
            depth_subresource_range(),
        );
        let multisample_barrier = multisample_color.map(|color| {
            image_barrier(
                color.handle,
                vk::VK_IMAGE_LAYOUT_UNDEFINED,
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_NONE,
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                vk::VK_ACCESS_2_NONE,
                vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                color_subresource_range(),
            )
        });
        if let Some(multisample_barrier) = multisample_barrier {
            pipeline_barriers(
                &self.surface,
                &[color_barrier, depth_barrier, multisample_barrier],
            );
        } else {
            pipeline_barriers(&self.surface, &[color_barrier, depth_barrier]);
        }
        let color_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: multisample_color.map_or(view, |color| color.view),
            imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            resolveMode: if multisample_color.is_some() {
                vk::VK_RESOLVE_MODE_AVERAGE_BIT
            } else {
                vk::VK_RESOLVE_MODE_NONE
            },
            resolveImageView: multisample_color.map_or(ptr::null_mut(), |_| view),
            resolveImageLayout: if multisample_color.is_some() {
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL
            } else {
                vk::VK_IMAGE_LAYOUT_UNDEFINED
            },
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: vk::VkClearValue {
                color: vk::VkClearColorValue {
                    float32: clear.components(),
                },
            },
            ..Default::default()
        };
        let depth_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: target_depth.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_DONT_CARE,
            clearValue: vk::VkClearValue {
                depthStencil: vk::VkClearDepthStencilValue {
                    depth: depth_clear,
                    stencil: 0,
                },
            },
            ..Default::default()
        };
        let area = vk::VkRect2D {
            offset: vk::VkOffset2D { x: 0, y: 0 },
            extent: self.surface.swapchain.extent,
        };
        let rendering = vk::VkRenderingInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
            renderArea: area,
            layerCount: 1,
            colorAttachmentCount: 1,
            pColorAttachments: &raw const color_attachment,
            pDepthAttachment: &raw const depth_attachment,
            ..Default::default()
        };
        let viewport = vk::VkViewport {
            x: 0.0,
            y: 0.0,
            width: area.extent.width as f32,
            height: area.extent.height as f32,
            minDepth: 0.0,
            maxDepth: 1.0,
        };
        self.begin_gpu_region(c"scene", [0.15, 0.75, 0.35, 1.0], SCENE_QUERY_START);
        unsafe {
            let functions = &device.functions;
            functions.cmd_begin_rendering.expect("loaded function")(
                self.surface.command_buffer,
                &raw const rendering,
            );
            functions.cmd_set_viewport.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const viewport,
            );
            functions.cmd_set_scissor.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const area,
            );
            self.record_prepared_scene(scene);
            functions.cmd_end_rendering.expect("loaded function")(self.surface.command_buffer);
        }
        self.end_gpu_region(SCENE_QUERY_START + 1);
        self.write_empty_gpu_region(POSTPROCESS_QUERY_START);
        let present = image_barrier(
            image,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_ACCESS_2_NONE,
            color_subresource_range(),
        );
        pipeline_barrier(&self.surface, &present);
        self.end_gpu_region(FRAME_QUERY_START + 1);
        check(
            unsafe {
                device
                    .functions
                    .end_command_buffer
                    .expect("loaded function")(self.surface.command_buffer)
            },
            "vkEndCommandBuffer for textured frame",
        )
    }

    #[allow(
        clippy::cast_precision_loss,
        clippy::too_many_lines,
        clippy::too_many_arguments
    )]
    fn record_postprocessed_draw(
        &mut self,
        image_index: u32,
        postprocess_pipeline_index: usize,
        target_index: usize,
        postprocess_descriptor: vk::VkDescriptorSet,
        scene: PreparedScene,
        clear: ClearColor,
        depth_clear: f32,
    ) -> Result<(), GraphicsError> {
        let slot = usize::try_from(image_index).map_err(|_| error("invalid image index"))?;
        let target = &self.postprocess_targets[target_index];
        let scene_color = target
            .scene_color
            .ok_or_else(|| error("postprocess targets were reclaimed by a newer generation"))?;
        let target_depth = target
            .depth
            .ok_or_else(|| error("postprocess targets were reclaimed by a newer generation"))?;
        let multisample_color = target.multisample_color;
        let scene_extent = target.scene_extent;
        let image = self.surface.swapchain.images[slot];
        let view = self.surface.swapchain.views[slot];
        let old_layout = if self.surface.swapchain.initialized[slot] {
            vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR
        } else {
            vk::VK_IMAGE_LAYOUT_UNDEFINED
        };
        self.surface.wait_for_frame()?;
        self.collect_gpu_timing()?;
        let device = self.surface.device();
        check(
            unsafe {
                device.functions.reset_fences.expect("loaded function")(
                    device.handle,
                    1,
                    &raw const self.surface.frame_fence,
                )
            },
            "vkResetFences for postprocessed frame",
        )?;
        check(
            unsafe {
                device
                    .functions
                    .reset_command_buffer
                    .expect("loaded function")(self.surface.command_buffer, 0)
            },
            "vkResetCommandBuffer for postprocessed frame",
        )?;
        let begin = vk::VkCommandBufferBeginInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            flags: vk::VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT as u32,
            ..Default::default()
        };
        check(
            unsafe {
                device
                    .functions
                    .begin_command_buffer
                    .expect("loaded function")(
                    self.surface.command_buffer, &raw const begin
                )
            },
            "vkBeginCommandBuffer for postprocessed frame",
        )?;
        self.begin_gpu_frame();
        self.recorded_has_shadow = self.pending_shadow_target.is_some();
        if self.recorded_has_shadow {
            self.begin_gpu_region(c"shadow", [0.55, 0.25, 0.8, 1.0], SHADOW_QUERY_START);
            self.record_shadow_pass_if_pending();
            self.end_gpu_region(SHADOW_QUERY_START + 1);
        } else {
            self.write_empty_gpu_region(SHADOW_QUERY_START);
        }
        let device = self.surface.device();
        let swapchain_barrier = image_barrier(
            image,
            old_layout,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            if old_layout == vk::VK_IMAGE_LAYOUT_UNDEFINED {
                vk::VK_PIPELINE_STAGE_2_NONE
            } else {
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT
            },
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            color_subresource_range(),
        );
        let scene_barrier = image_barrier(
            scene_color.handle,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            color_subresource_range(),
        );
        let depth_barrier = image_barrier(
            target_depth.handle,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
            depth_subresource_range(),
        );
        let multisample_barrier = multisample_color.map(|color| {
            image_barrier(
                color.handle,
                vk::VK_IMAGE_LAYOUT_UNDEFINED,
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_NONE,
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                vk::VK_ACCESS_2_NONE,
                vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                color_subresource_range(),
            )
        });
        if let Some(multisample_barrier) = multisample_barrier {
            pipeline_barriers(
                &self.surface,
                &[
                    swapchain_barrier,
                    scene_barrier,
                    depth_barrier,
                    multisample_barrier,
                ],
            );
        } else {
            pipeline_barriers(
                &self.surface,
                &[swapchain_barrier, scene_barrier, depth_barrier],
            );
        }

        let scene_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: multisample_color.map_or(scene_color.view, |color| color.view),
            imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            resolveMode: if multisample_color.is_some() {
                vk::VK_RESOLVE_MODE_AVERAGE_BIT
            } else {
                vk::VK_RESOLVE_MODE_NONE
            },
            resolveImageView: multisample_color.map_or(ptr::null_mut(), |_| scene_color.view),
            resolveImageLayout: if multisample_color.is_some() {
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL
            } else {
                vk::VK_IMAGE_LAYOUT_UNDEFINED
            },
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: vk::VkClearValue {
                color: vk::VkClearColorValue {
                    float32: clear.components(),
                },
            },
            ..Default::default()
        };
        let depth_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: target_depth.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_DONT_CARE,
            clearValue: vk::VkClearValue {
                depthStencil: vk::VkClearDepthStencilValue {
                    depth: depth_clear,
                    stencil: 0,
                },
            },
            ..Default::default()
        };
        let area = vk::VkRect2D {
            offset: vk::VkOffset2D { x: 0, y: 0 },
            extent: self.surface.swapchain.extent,
        };
        let scene_area = vk::VkRect2D {
            offset: vk::VkOffset2D { x: 0, y: 0 },
            extent: scene_extent,
        };
        let scene_rendering = vk::VkRenderingInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
            renderArea: scene_area,
            layerCount: 1,
            colorAttachmentCount: 1,
            pColorAttachments: &raw const scene_attachment,
            pDepthAttachment: &raw const depth_attachment,
            ..Default::default()
        };
        let scene_viewport = vk::VkViewport {
            x: 0.0,
            y: 0.0,
            width: scene_area.extent.width as f32,
            height: scene_area.extent.height as f32,
            minDepth: 0.0,
            maxDepth: 1.0,
        };
        let viewport = vk::VkViewport {
            x: 0.0,
            y: 0.0,
            width: area.extent.width as f32,
            height: area.extent.height as f32,
            minDepth: 0.0,
            maxDepth: 1.0,
        };
        self.begin_gpu_region(c"scene", [0.15, 0.75, 0.35, 1.0], SCENE_QUERY_START);
        unsafe {
            let functions = &device.functions;
            functions.cmd_begin_rendering.expect("loaded function")(
                self.surface.command_buffer,
                &raw const scene_rendering,
            );
            functions.cmd_set_viewport.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const scene_viewport,
            );
            functions.cmd_set_scissor.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const scene_area,
            );
            self.record_prepared_scene(scene);
            functions.cmd_end_rendering.expect("loaded function")(self.surface.command_buffer);
        }
        self.end_gpu_region(SCENE_QUERY_START + 1);

        let sample_scene = image_barrier(
            scene_color.handle,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
            color_subresource_range(),
        );
        pipeline_barrier(&self.surface, &sample_scene);
        let post_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: view,
            imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: vk::VkClearValue {
                color: vk::VkClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                },
            },
            ..Default::default()
        };
        let post_rendering = vk::VkRenderingInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
            renderArea: area,
            layerCount: 1,
            colorAttachmentCount: 1,
            pColorAttachments: &raw const post_attachment,
            ..Default::default()
        };
        let post_pipeline = &self.postprocess_pipelines[postprocess_pipeline_index];
        self.begin_gpu_region(
            c"postprocess",
            [0.95, 0.55, 0.15, 1.0],
            POSTPROCESS_QUERY_START,
        );
        unsafe {
            let functions = &device.functions;
            functions.cmd_begin_rendering.expect("loaded function")(
                self.surface.command_buffer,
                &raw const post_rendering,
            );
            functions.cmd_bind_pipeline.expect("loaded function")(
                self.surface.command_buffer,
                vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                post_pipeline.pipeline,
            );
            functions.cmd_bind_descriptor_sets.expect("loaded function")(
                self.surface.command_buffer,
                vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                post_pipeline.layout,
                0,
                1,
                &raw const postprocess_descriptor,
                0,
                ptr::null(),
            );
            functions.cmd_set_viewport.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const viewport,
            );
            functions.cmd_set_scissor.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const area,
            );
            functions.cmd_draw.expect("loaded function")(self.surface.command_buffer, 3, 1, 0, 0);
            if matches!(scene, PreparedScene::Materials) && !self.resolved_overlay_draws.is_empty()
            {
                self.record_material_draws(&self.resolved_overlay_draws, true);
            }
            functions.cmd_end_rendering.expect("loaded function")(self.surface.command_buffer);
        }
        self.end_gpu_region(POSTPROCESS_QUERY_START + 1);
        let present = image_barrier(
            image,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_ACCESS_2_NONE,
            color_subresource_range(),
        );
        pipeline_barrier(&self.surface, &present);
        self.end_gpu_region(FRAME_QUERY_START + 1);
        check(
            unsafe {
                device
                    .functions
                    .end_command_buffer
                    .expect("loaded function")(self.surface.command_buffer)
            },
            "vkEndCommandBuffer for postprocessed frame",
        )
    }

    /// Records one resolved material draw list on the active rendering. Overlay recording
    /// binds each pipeline's single-sample no-depth variant, whose absence was rejected at
    /// preparation.
    unsafe fn record_material_draws(&self, draws: &[ResolvedMaterialDraw], overlay: bool) {
        unsafe {
            let functions = &self.surface.device().functions;
            let offset = 0_u64;
            for draw in draws {
                let pipeline = &self.material_pipelines[draw.pipeline];
                functions.cmd_bind_pipeline.expect("loaded function")(
                    self.surface.command_buffer,
                    vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                    if overlay {
                        pipeline.overlay_pipeline
                    } else {
                        pipeline.pipeline
                    },
                );
                functions.cmd_bind_descriptor_sets.expect("loaded function")(
                    self.surface.command_buffer,
                    vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                    pipeline.layout,
                    0,
                    1,
                    &raw const draw.descriptor,
                    draw.dynamic_offset_count,
                    draw.dynamic_offsets.as_ptr(),
                );
                match draw.geometry {
                    ResolvedGeometry::Mesh(mesh) => {
                        let mesh = &self.meshes[mesh];
                        functions.cmd_bind_vertex_buffers.expect("loaded function")(
                            self.surface.command_buffer,
                            0,
                            1,
                            &raw const mesh.vertices.handle,
                            &raw const offset,
                        );
                        functions.cmd_bind_index_buffer.expect("loaded function")(
                            self.surface.command_buffer,
                            mesh.indices.handle,
                            0,
                            mesh.index_type,
                        );
                        functions
                            .cmd_draw_indexed_indirect
                            .expect("loaded function")(
                            self.surface.command_buffer,
                            mesh.indirect.handle,
                            0,
                            1,
                            u32::try_from(mem::size_of::<vk::VkDrawIndexedIndirectCommand>())
                                .expect("indirect command size fits u32"),
                        );
                    }
                    ResolvedGeometry::Transient {
                        vertex_offset,
                        index_offset,
                        index_count,
                        index_type,
                    } => {
                        functions.cmd_bind_vertex_buffers.expect("loaded function")(
                            self.surface.command_buffer,
                            0,
                            1,
                            &raw const self.transient_geometry.handle,
                            &raw const vertex_offset,
                        );
                        functions.cmd_bind_index_buffer.expect("loaded function")(
                            self.surface.command_buffer,
                            self.transient_geometry.handle,
                            index_offset,
                            index_type,
                        );
                        functions.cmd_draw_indexed.expect("loaded function")(
                            self.surface.command_buffer,
                            index_count,
                            1,
                            0,
                            0,
                            0,
                        );
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    unsafe fn record_prepared_scene(&self, scene: PreparedScene) {
        unsafe {
            let functions = &self.surface.device().functions;
            match scene {
                PreparedScene::Draws => {
                    let offset = 0_u64;
                    for draw in &self.resolved_draws {
                        let mesh = &self.meshes[draw.mesh];
                        let pipeline = &self.pipelines[draw.pipeline];
                        functions.cmd_bind_pipeline.expect("loaded function")(
                            self.surface.command_buffer,
                            vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                            pipeline.pipeline,
                        );
                        functions.cmd_bind_descriptor_sets.expect("loaded function")(
                            self.surface.command_buffer,
                            vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                            pipeline.layout,
                            0,
                            1,
                            &raw const draw.descriptor,
                            1,
                            &raw const draw.dynamic_offset,
                        );
                        functions.cmd_bind_vertex_buffers.expect("loaded function")(
                            self.surface.command_buffer,
                            0,
                            1,
                            &raw const mesh.vertices.handle,
                            &raw const offset,
                        );
                        functions.cmd_bind_index_buffer.expect("loaded function")(
                            self.surface.command_buffer,
                            mesh.indices.handle,
                            0,
                            mesh.index_type,
                        );
                        functions
                            .cmd_draw_indexed_indirect
                            .expect("loaded function")(
                            self.surface.command_buffer,
                            mesh.indirect.handle,
                            0,
                            1,
                            u32::try_from(mem::size_of::<vk::VkDrawIndexedIndirectCommand>())
                                .expect("indirect command size fits u32"),
                        );
                    }
                }
                PreparedScene::Materials => {
                    self.record_material_draws(&self.resolved_material_draws, false);
                }
                PreparedScene::Instances => {
                    let dynamic_offset = 0_u32;
                    for batch in &self.resolved_instance_batches {
                        let mesh = &self.meshes[batch.mesh];
                        let pipeline = &self.instanced_pipelines[batch.pipeline];
                        functions.cmd_bind_pipeline.expect("loaded function")(
                            self.surface.command_buffer,
                            vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                            pipeline.pipeline,
                        );
                        functions.cmd_bind_descriptor_sets.expect("loaded function")(
                            self.surface.command_buffer,
                            vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                            pipeline.layout,
                            0,
                            1,
                            &raw const batch.descriptor,
                            1,
                            &raw const dynamic_offset,
                        );
                        let buffers = [mesh.vertices.handle, self.instance_transforms.handle];
                        let offsets = [0_u64, batch.transform_offset];
                        functions.cmd_bind_vertex_buffers.expect("loaded function")(
                            self.surface.command_buffer,
                            0,
                            2,
                            buffers.as_ptr(),
                            offsets.as_ptr(),
                        );
                        functions.cmd_bind_index_buffer.expect("loaded function")(
                            self.surface.command_buffer,
                            mesh.indices.handle,
                            0,
                            mesh.index_type,
                        );
                        functions.cmd_draw_indexed.expect("loaded function")(
                            self.surface.command_buffer,
                            mesh.index_count,
                            batch.instance_count,
                            0,
                            0,
                            0,
                        );
                    }
                }
            }
        }
    }

    fn destroy_resources(&mut self) {
        let device = self.surface.device();
        if !self.gpu_timing.query_pool.is_null() {
            unsafe {
                device
                    .functions
                    .destroy_query_pool
                    .expect("loaded function")(
                    device.handle,
                    self.gpu_timing.query_pool,
                    ptr::null(),
                );
            }
            self.gpu_timing.query_pool = ptr::null_mut();
            self.gpu_timing.pending = None;
        }
        for pipeline in self
            .pipelines
            .take_all()
            .into_iter()
            .chain(self.instanced_pipelines.take_all())
            .chain(self.postprocess_pipelines.take_all())
        {
            destroy_pipeline_device(device, pipeline);
        }
        for pipeline in self.material_pipelines.take_all() {
            destroy_material_pipeline_device(device, pipeline);
        }
        for pipeline in self.shadow_pipelines.take_all() {
            destroy_shadow_pipeline_device(device, pipeline);
        }
        for map in self.shadow_maps.take_all() {
            unsafe { destroy_image_device(device, map.image) };
        }
        for array in self.shadow_map_arrays.take_all() {
            destroy_shadow_map_array_device(device, array);
        }
        for texture in self.textures.take_all() {
            destroy_texture_device(device, texture);
        }
        for target in self.targets.take_all() {
            destroy_target_device(device, target);
        }
        for target in self.postprocess_targets.take_all() {
            destroy_postprocess_target_device(device, target);
        }
        for mesh in self.meshes.take_all() {
            destroy_mesh_device(device, mesh);
        }
        unsafe {
            destroy_buffer_device(device, mem::take(&mut self.uniform));
            destroy_buffer_device(device, mem::take(&mut self.storage));
            destroy_buffer_device(device, mem::take(&mut self.transient_geometry));
            destroy_buffer_device(device, mem::take(&mut self.instance_transforms));
        }

        // `shutdown` moves the surface out and deliberately suppresses this
        // session's destructor. Release the now-empty arenas' allocations here
        // so that path does not retain their backing storage.
        self.pipelines = Arena::new("textured pipeline");
        self.instanced_pipelines = Arena::new("instanced textured pipeline");
        self.material_pipelines = Arena::new("material pipeline");
        self.postprocess_pipelines = Arena::new("postprocess pipeline");
        self.textures = Arena::new("texture");
        self.targets = Arena::new("render targets");
        self.postprocess_targets = Arena::new("postprocess targets");
        self.meshes = Arena::new("mesh");
    }
}

impl Drop for TexturedSession<'_> {
    fn drop(&mut self) {
        let _ = self.surface.finish();
        self.destroy_resources();
    }
}

// These helpers intentionally consume the wrapper so one call owns the native destruction
// authority. Most fields are raw handles, so Clippy cannot otherwise observe that ownership.
#[allow(clippy::needless_pass_by_value)]
fn destroy_mesh_device(device: &super::Device, mesh: MeshResource) {
    unsafe {
        destroy_buffer_device(device, mesh.vertices);
        destroy_buffer_device(device, mesh.indices);
        destroy_buffer_device(device, mesh.indirect);
    }
}

#[allow(clippy::needless_pass_by_value)]
fn destroy_texture_device(device: &super::Device, texture: TextureResource) {
    unsafe {
        device.functions.destroy_sampler.expect("loaded function")(
            device.handle,
            texture.sampler,
            ptr::null(),
        );
        destroy_image_device(device, texture.image);
    }
}

#[allow(clippy::needless_pass_by_value)]
fn destroy_pipeline_device(device: &super::Device, pipeline: PipelineResource) {
    unsafe {
        device.functions.destroy_pipeline.expect("loaded function")(
            device.handle,
            pipeline.pipeline,
            ptr::null(),
        );
        device
            .functions
            .destroy_pipeline_layout
            .expect("loaded function")(device.handle, pipeline.layout, ptr::null());
        device
            .functions
            .destroy_descriptor_pool
            .expect("loaded function")(device.handle, pipeline.descriptor_pool, ptr::null());
        device
            .functions
            .destroy_descriptor_set_layout
            .expect("loaded function")(device.handle, pipeline.set_layout, ptr::null());
        if !pipeline.sampler.is_null() {
            device.functions.destroy_sampler.expect("loaded function")(
                device.handle,
                pipeline.sampler,
                ptr::null(),
            );
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn destroy_material_pipeline_device(device: &super::Device, pipeline: MaterialPipelineResource) {
    for &(_, sampler) in pipeline
        .samplers
        .iter()
        .chain(pipeline.comparison_sampler.iter())
    {
        unsafe {
            device.functions.destroy_sampler.expect("loaded function")(
                device.handle,
                sampler,
                ptr::null(),
            );
        }
    }
    if !pipeline.overlay_pipeline.is_null() {
        unsafe {
            device.functions.destroy_pipeline.expect("loaded function")(
                device.handle,
                pipeline.overlay_pipeline,
                ptr::null(),
            );
        }
    }
    destroy_pipeline_device(
        device,
        PipelineResource {
            set_layout: pipeline.set_layout,
            layout: pipeline.layout,
            pipeline: pipeline.pipeline,
            descriptor_pool: pipeline.descriptor_pool,
            sampler: ptr::null_mut(),
            bindings: Vec::new(),
        },
    );
}

// Views fall before the image, and the image before its memory, so the validation layers
// observe no dangling child handles.
#[allow(clippy::needless_pass_by_value)]
fn destroy_shadow_map_array_device(device: &super::Device, array: ShadowMapArrayResource) {
    unsafe {
        for &view in array
            .layer_views
            .iter()
            .chain(core::iter::once(&array.array_view))
        {
            if !view.is_null() {
                device
                    .functions
                    .destroy_image_view
                    .expect("loaded function")(device.handle, view, ptr::null());
            }
        }
        if !array.image.is_null() {
            device.functions.destroy_image.expect("loaded function")(
                device.handle,
                array.image,
                ptr::null(),
            );
        }
        if !array.memory.is_null() {
            device.functions.free_memory.expect("loaded function")(
                device.handle,
                array.memory,
                ptr::null(),
            );
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn destroy_shadow_pipeline_device(device: &super::Device, pipeline: ShadowPipelineResource) {
    destroy_pipeline_device(
        device,
        PipelineResource {
            set_layout: pipeline.set_layout,
            layout: pipeline.layout,
            pipeline: pipeline.pipeline,
            descriptor_pool: pipeline.descriptor_pool,
            sampler: ptr::null_mut(),
            bindings: Vec::new(),
        },
    );
}

#[allow(clippy::needless_pass_by_value)]
fn destroy_target_device(device: &super::Device, target: TargetResource) {
    unsafe {
        if let Some(color) = target.multisample_color {
            destroy_image_device(device, color);
        }
        if let Some(depth) = target.depth {
            destroy_image_device(device, depth);
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn destroy_postprocess_target_device(device: &super::Device, target: PostprocessTargetResource) {
    unsafe {
        if let Some(color) = target.multisample_color {
            destroy_image_device(device, color);
        }
        if let Some(color) = target.scene_color {
            destroy_image_device(device, color);
        }
        if let Some(depth) = target.depth {
            destroy_image_device(device, depth);
        }
    }
}

impl ClearSurface<'_> {
    fn submit_recorded(&mut self, image_index: u32) -> Result<FrameDisposition, GraphicsError> {
        let slot = usize::try_from(image_index).map_err(|_| error("invalid image index"))?;
        let render_finished = self.swapchain.render_finished[slot];
        let wait = vk::VkSemaphoreSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_SEMAPHORE_SUBMIT_INFO,
            semaphore: self.image_available,
            stageMask: vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            ..Default::default()
        };
        let command = vk::VkCommandBufferSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO,
            commandBuffer: self.command_buffer,
            ..Default::default()
        };
        let signal = vk::VkSemaphoreSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_SEMAPHORE_SUBMIT_INFO,
            semaphore: render_finished,
            stageMask: vk::VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
            ..Default::default()
        };
        let submit = vk::VkSubmitInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_SUBMIT_INFO_2,
            waitSemaphoreInfoCount: 1,
            pWaitSemaphoreInfos: &raw const wait,
            commandBufferInfoCount: 1,
            pCommandBufferInfos: &raw const command,
            signalSemaphoreInfoCount: 1,
            pSignalSemaphoreInfos: &raw const signal,
            ..Default::default()
        };
        check(
            unsafe {
                self.device()
                    .functions
                    .queue_submit2
                    .expect("loaded function")(
                    self.device().queue,
                    1,
                    &raw const submit,
                    self.frame_fence,
                )
            },
            "vkQueueSubmit2 for textured frame",
        )?;
        self.frame_pending = true;
        self.swapchain.initialized[slot] = true;
        self.queue_present_with_feedback(
            image_index,
            render_finished,
            "vkQueuePresentKHR for textured frame",
        )?;
        Ok(FrameDisposition::Presented(self.info.generation()))
    }
}

fn create_buffer(
    surface: &ClearSurface<'_>,
    size: usize,
    usage: u32,
    bytes: &[u8],
) -> Result<Buffer, GraphicsError> {
    let size =
        u64::try_from(size).map_err(|_| error("buffer size exceeds Vulkan address space"))?;
    let info = vk::VkBufferCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        size,
        usage,
        sharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
        ..Default::default()
    };
    let device = surface.device();
    let mut buffer = Buffer {
        size,
        ..Default::default()
    };
    check(
        unsafe {
            device.functions.create_buffer.expect("loaded function")(
                device.handle,
                &raw const info,
                ptr::null(),
                &raw mut buffer.handle,
            )
        },
        "vkCreateBuffer for textured slice",
    )?;
    if let Err(failure) = complete_buffer_storage(surface, &mut buffer, bytes) {
        destroy_buffer(surface, buffer);
        return Err(failure);
    }
    Ok(buffer)
}

fn create_gpu_query_pool(surface: &ClearSurface<'_>) -> Result<vk::VkQueryPool, GraphicsError> {
    let info = vk::VkQueryPoolCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO,
        queryType: vk::VK_QUERY_TYPE_TIMESTAMP,
        queryCount: GPU_QUERY_COUNT,
        ..Default::default()
    };
    let mut pool = ptr::null_mut();
    check(
        unsafe {
            surface
                .device()
                .functions
                .create_query_pool
                .expect("loaded function")(
                surface.device().handle,
                &raw const info,
                ptr::null(),
                &raw mut pool,
            )
        },
        "vkCreateQueryPool for GPU frame diagnostics",
    )?;
    Ok(pool)
}

fn timestamp_tick_delta(start: u64, end: u64, valid_bits: u32) -> u64 {
    if valid_bits >= 64 {
        end.wrapping_sub(start)
    } else {
        let mask = (1_u64 << valid_bits) - 1;
        end.wrapping_sub(start) & mask
    }
}

fn complete_buffer_storage(
    surface: &ClearSurface<'_>,
    buffer: &mut Buffer,
    bytes: &[u8],
) -> Result<(), GraphicsError> {
    let device = surface.device();
    let mut requirements = vk::VkMemoryRequirements::default();
    unsafe {
        device
            .functions
            .get_buffer_memory_requirements
            .expect("loaded function")(device.handle, buffer.handle, &raw mut requirements);
    };
    let memory_type = find_memory_type(
        device,
        requirements.memoryTypeBits,
        (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) as u32,
    )
    .ok_or_else(|| error("no host-visible coherent Vulkan memory type"))?;
    let allocate = vk::VkMemoryAllocateInfo {
        sType: vk::VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        allocationSize: requirements.size,
        memoryTypeIndex: memory_type,
        ..Default::default()
    };
    check(
        unsafe {
            device.functions.allocate_memory.expect("loaded function")(
                device.handle,
                &raw const allocate,
                ptr::null(),
                &raw mut buffer.memory,
            )
        },
        "vkAllocateMemory for buffer",
    )?;
    check(
        unsafe {
            device
                .functions
                .bind_buffer_memory
                .expect("loaded function")(
                device.handle, buffer.handle, buffer.memory, 0
            )
        },
        "vkBindBufferMemory",
    )?;
    if !bytes.is_empty() {
        write_buffer(surface, buffer, bytes)?;
    }
    Ok(())
}

fn write_buffer(
    surface: &ClearSurface<'_>,
    buffer: &Buffer,
    bytes: &[u8],
) -> Result<(), GraphicsError> {
    if u64::try_from(bytes.len()).map_err(|_| error("buffer write exceeds u64"))? > buffer.size {
        return Err(error("buffer write exceeds allocation"));
    }
    let device = surface.device();
    let mut mapped = ptr::null_mut();
    check(
        unsafe {
            device.functions.map_memory.expect("loaded function")(
                device.handle,
                buffer.memory,
                0,
                buffer.size,
                0,
                &raw mut mapped,
            )
        },
        "vkMapMemory",
    )?;
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast(), bytes.len());
        device.functions.unmap_memory.expect("loaded function")(device.handle, buffer.memory);
    }
    Ok(())
}

fn write_scene_transforms(
    surface: &ClearSurface<'_>,
    buffer: &Buffer,
    draws: &[TexturedSceneDraw<'_>],
) -> Result<(), GraphicsError> {
    let required = draws
        .len()
        .saturating_sub(1)
        .checked_mul(DRAW_UNIFORM_STRIDE)
        .and_then(|offset| offset.checked_add(DRAW_UNIFORM_SIZE))
        .ok_or_else(|| error("scene transform write exceeds address space"))?;
    if u64::try_from(required).map_err(|_| error("scene transform write exceeds u64"))?
        > buffer.size
    {
        return Err(error("scene transform write exceeds allocation"));
    }
    let device = surface.device();
    let mut mapped = ptr::null_mut();
    check(
        unsafe {
            device.functions.map_memory.expect("loaded function")(
                device.handle,
                buffer.memory,
                0,
                buffer.size,
                0,
                &raw mut mapped,
            )
        },
        "vkMapMemory for scene transforms",
    )?;
    unsafe {
        for (index, draw) in draws.iter().enumerate() {
            ptr::copy_nonoverlapping(
                ptr::from_ref(&draw.model_view_projection).cast::<u8>(),
                mapped.cast::<u8>().add(index * DRAW_UNIFORM_STRIDE),
                DRAW_UNIFORM_SIZE,
            );
        }
        device.functions.unmap_memory.expect("loaded function")(device.handle, buffer.memory);
    }
    Ok(())
}

fn write_material_uniforms(
    surface: &ClearSurface<'_>,
    buffer: &Buffer,
    records: &[MaterialRecord<'_>],
    overlay: &[MaterialRecord<'_>],
    shadow: Option<&ShadowPrepass<'_>>,
) -> Result<(), GraphicsError> {
    let shadow_records = shadow.map_or(0, |shadow| shadow.records().count());
    let required = records
        .len()
        .checked_add(overlay.len())
        .and_then(|slots| slots.checked_add(shadow_records))
        .and_then(|slots| slots.saturating_sub(1).checked_mul(DRAW_UNIFORM_STRIDE))
        .and_then(|offset| offset.checked_add(DRAW_UNIFORM_STRIDE))
        .ok_or_else(|| error("material uniform write exceeds address space"))?;
    if u64::try_from(required).map_err(|_| error("material uniform write exceeds u64"))?
        > buffer.size
    {
        return Err(error("material uniform write exceeds allocation"));
    }
    debug_assert!(
        records
            .iter()
            .chain(overlay)
            .map(|record| record.uniform)
            .chain(
                shadow
                    .into_iter()
                    .flat_map(|shadow| shadow.records().map(|record| record.uniform))
            )
            .all(|uniform| uniform.len() <= DRAW_UNIFORM_STRIDE),
        "uniform sizes were validated at pipeline creation"
    );
    let device = surface.device();
    let mut mapped = ptr::null_mut();
    check(
        unsafe {
            device.functions.map_memory.expect("loaded function")(
                device.handle,
                buffer.memory,
                0,
                buffer.size,
                0,
                &raw mut mapped,
            )
        },
        "vkMapMemory for material uniforms",
    )?;
    unsafe {
        let uniforms = records
            .iter()
            .chain(overlay)
            .map(|record| record.uniform)
            .chain(
                shadow
                    .into_iter()
                    .flat_map(|shadow| shadow.records().map(|record| record.uniform)),
            );
        for (index, uniform) in uniforms.enumerate() {
            ptr::copy_nonoverlapping(
                uniform.as_ptr(),
                mapped.cast::<u8>().add(index * DRAW_UNIFORM_STRIDE),
                uniform.len(),
            );
        }
        device.functions.unmap_memory.expect("loaded function")(device.handle, buffer.memory);
    }
    Ok(())
}

/// Per-record byte offsets into the frame's read-only storage region — scene records first,
/// then overlay records, then shadow records, each aligned to [`STORAGE_OFFSET_ALIGNMENT`] —
/// plus the total bytes the region needs. Records without a storage slot occupy no bytes and
/// keep offset zero.
fn material_storage_offsets(
    records: &[MaterialRecord<'_>],
    overlay: &[MaterialRecord<'_>],
    shadow: Option<&ShadowPrepass<'_>>,
) -> Result<(Vec<u32>, usize), GraphicsError> {
    let lengths = records
        .iter()
        .chain(overlay)
        .map(|record| record.storage.len())
        .chain(
            shadow
                .into_iter()
                .flat_map(|shadow| shadow.records().map(|record| record.storage.len())),
        );
    let mut offsets = Vec::with_capacity(
        records.len() + overlay.len() + shadow.map_or(0, |shadow| shadow.records().count()),
    );
    let mut running = 0_usize;
    for length in lengths {
        offsets
            .push(u32::try_from(running).map_err(|_| error("Vulkan storage offsets exceed u32"))?);
        let aligned = length
            .checked_next_multiple_of(STORAGE_OFFSET_ALIGNMENT)
            .ok_or_else(|| error("Vulkan storage offsets overflow"))?;
        running = running
            .checked_add(aligned)
            .ok_or_else(|| error("Vulkan storage offsets overflow"))?;
    }
    Ok((offsets, running))
}

/// Uniform and storage dynamic offsets rearranged into ascending binding-number order, the order
/// Vulkan consumes dynamic offsets across a set layout's dynamic descriptors.
const fn dynamic_offsets_in_binding_order(
    uniform: Option<(u32, u32)>,
    storage: Option<(u32, u32)>,
    uniform_offset: u32,
    storage_offset: u32,
) -> ([u32; 2], u32) {
    match (uniform, storage) {
        (None, None) => ([0, 0], 0),
        (Some(_), None) => ([uniform_offset, 0], 1),
        (None, Some(_)) => ([storage_offset, 0], 1),
        (Some((uniform_binding, _)), Some((storage_binding, _))) => {
            if uniform_binding < storage_binding {
                ([uniform_offset, storage_offset], 2)
            } else {
                ([storage_offset, uniform_offset], 2)
            }
        }
    }
}

fn write_material_storage(
    surface: &ClearSurface<'_>,
    buffer: &Buffer,
    records: &[MaterialRecord<'_>],
    overlay: &[MaterialRecord<'_>],
    shadow: Option<&ShadowPrepass<'_>>,
    offsets: &[u32],
) -> Result<(), GraphicsError> {
    let contents = records
        .iter()
        .chain(overlay)
        .map(|record| record.storage)
        .chain(
            shadow
                .into_iter()
                .flat_map(|shadow| shadow.records().map(|record| record.storage)),
        );
    if records
        .iter()
        .chain(overlay)
        .all(|record| record.storage.is_empty())
        && shadow.is_none_or(|shadow| shadow.records().all(|record| record.storage.is_empty()))
    {
        return Ok(());
    }
    let device = surface.device();
    let mut mapped = ptr::null_mut();
    check(
        unsafe {
            device.functions.map_memory.expect("loaded function")(
                device.handle,
                buffer.memory,
                0,
                buffer.size,
                0,
                &raw mut mapped,
            )
        },
        "vkMapMemory for record storage",
    )?;
    unsafe {
        for (bytes, &offset) in contents.zip(offsets) {
            debug_assert!(
                offset as usize + bytes.len() <= usize::try_from(buffer.size).expect("size fits"),
                "storage capacity was ensured before writing"
            );
            ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                mapped.cast::<u8>().add(offset as usize),
                bytes.len(),
            );
        }
        device.functions.unmap_memory.expect("loaded function")(device.handle, buffer.memory);
    }
    Ok(())
}

fn write_transient_geometry(
    surface: &ClearSurface<'_>,
    buffer: &Buffer,
    records: &[MaterialRecord<'_>],
    overlay: &[MaterialRecord<'_>],
) -> Result<(), GraphicsError> {
    if records
        .iter()
        .chain(overlay)
        .all(|record| matches!(record.geometry, GeometrySource::Mesh(_)))
    {
        return Ok(());
    }
    let device = surface.device();
    let mut mapped = ptr::null_mut();
    check(
        unsafe {
            device.functions.map_memory.expect("loaded function")(
                device.handle,
                buffer.memory,
                0,
                buffer.size,
                0,
                &raw mut mapped,
            )
        },
        "vkMapMemory for transient geometry",
    )?;
    unsafe {
        let mut offset = 0_usize;
        for record in records.iter().chain(overlay) {
            let GeometrySource::Transient(geometry) = record.geometry else {
                continue;
            };
            debug_assert!(
                offset + geometry.vertices.len() + geometry.indices.byte_len()
                    <= usize::try_from(buffer.size).expect("size fits"),
                "transient capacity was ensured before writing"
            );
            ptr::copy_nonoverlapping(
                geometry.vertices.as_ptr(),
                mapped.cast::<u8>().add(offset),
                geometry.vertices.len(),
            );
            offset += geometry
                .vertices
                .len()
                .next_multiple_of(STORAGE_OFFSET_ALIGNMENT);
            let index_pointer: *const u8 = match geometry.indices {
                MeshIndices::U16(indices) => indices.as_ptr().cast(),
                MeshIndices::U32(indices) => indices.as_ptr().cast(),
            };
            ptr::copy_nonoverlapping(
                index_pointer,
                mapped.cast::<u8>().add(offset),
                geometry.indices.byte_len(),
            );
            offset += geometry
                .indices
                .byte_len()
                .next_multiple_of(STORAGE_OFFSET_ALIGNMENT);
        }
        device.functions.unmap_memory.expect("loaded function")(device.handle, buffer.memory);
    }
    Ok(())
}

fn write_instance_transforms(
    surface: &ClearSurface<'_>,
    buffer: &Buffer,
    batches: &[TexturedInstanceBatch<'_>],
) -> Result<(), GraphicsError> {
    let required = batches.iter().try_fold(0_usize, |bytes, batch| {
        batch
            .model_view_projections
            .len()
            .checked_mul(INSTANCE_TRANSFORM_SIZE)
            .and_then(|batch_bytes| bytes.checked_add(batch_bytes))
            .ok_or_else(|| error("instance transform write exceeds address space"))
    })?;
    if u64::try_from(required).map_err(|_| error("instance transform write exceeds u64"))?
        > buffer.size
    {
        return Err(error("instance transform write exceeds allocation"));
    }
    let device = surface.device();
    let mut mapped = ptr::null_mut();
    check(
        unsafe {
            device.functions.map_memory.expect("loaded function")(
                device.handle,
                buffer.memory,
                0,
                buffer.size,
                0,
                &raw mut mapped,
            )
        },
        "vkMapMemory for instance transforms",
    )?;
    unsafe {
        let mut offset = 0_usize;
        for batch in batches {
            let bytes = mem::size_of_val(batch.model_view_projections);
            ptr::copy_nonoverlapping(
                batch.model_view_projections.as_ptr().cast::<u8>(),
                mapped.cast::<u8>().add(offset),
                bytes,
            );
            offset += bytes;
        }
        device.functions.unmap_memory.expect("loaded function")(device.handle, buffer.memory);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_image(
    surface: &ClearSurface<'_>,
    width: u32,
    height: u32,
    format: vk::VkFormat,
    usage: u32,
    aspect: u32,
    samples: vk::VkSampleCountFlagBits,
    mip_levels: u32,
) -> Result<Image, GraphicsError> {
    let info = vk::VkImageCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
        imageType: vk::VK_IMAGE_TYPE_2D,
        format,
        extent: vk::VkExtent3D {
            width,
            height,
            depth: 1,
        },
        mipLevels: mip_levels,
        arrayLayers: 1,
        samples,
        tiling: vk::VK_IMAGE_TILING_OPTIMAL,
        usage,
        sharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
        initialLayout: vk::VK_IMAGE_LAYOUT_UNDEFINED,
        ..Default::default()
    };
    let device = surface.device();
    let mut image = Image::default();
    check(
        unsafe {
            device.functions.create_image.expect("loaded function")(
                device.handle,
                &raw const info,
                ptr::null(),
                &raw mut image.handle,
            )
        },
        "vkCreateImage for textured slice",
    )?;
    if let Err(failure) = complete_image_storage(device, &mut image, format, aspect, mip_levels) {
        // SAFETY: The device is live and the destroy helper skips null child handles left by
        // partial construction.
        unsafe { destroy_image_device(device, image) };
        return Err(failure);
    }
    Ok(image)
}

fn complete_image_storage(
    device: &super::Device,
    image: &mut Image,
    format: vk::VkFormat,
    aspect: u32,
    mip_levels: u32,
) -> Result<(), GraphicsError> {
    let mut requirements = vk::VkMemoryRequirements::default();
    unsafe {
        device
            .functions
            .get_image_memory_requirements
            .expect("loaded function")(device.handle, image.handle, &raw mut requirements);
    };
    let memory_type = find_memory_type(
        device,
        requirements.memoryTypeBits,
        vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
    )
    .ok_or_else(|| error("no device-local Vulkan image memory type"))?;
    let allocate = vk::VkMemoryAllocateInfo {
        sType: vk::VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        allocationSize: requirements.size,
        memoryTypeIndex: memory_type,
        ..Default::default()
    };
    check(
        unsafe {
            device.functions.allocate_memory.expect("loaded function")(
                device.handle,
                &raw const allocate,
                ptr::null(),
                &raw mut image.memory,
            )
        },
        "vkAllocateMemory for image",
    )?;
    check(
        unsafe {
            device.functions.bind_image_memory.expect("loaded function")(
                device.handle,
                image.handle,
                image.memory,
                0,
            )
        },
        "vkBindImageMemory",
    )?;
    let view_info = vk::VkImageViewCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
        image: image.handle,
        viewType: vk::VK_IMAGE_VIEW_TYPE_2D,
        format,
        subresourceRange: vk::VkImageSubresourceRange {
            aspectMask: aspect,
            levelCount: mip_levels,
            layerCount: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    check(
        unsafe {
            device.functions.create_image_view.expect("loaded function")(
                device.handle,
                &raw const view_info,
                ptr::null(),
                &raw mut image.view,
            )
        },
        "vkCreateImageView for textured slice",
    )?;
    Ok(())
}

/// Creates the layered depth image backing one shadow map array, its per-layer rendering views,
/// and its whole-array sampling view.
fn create_shadow_map_array_storage(
    surface: &ClearSurface<'_>,
    size: u32,
    layers: u32,
) -> Result<ShadowMapArrayResource, GraphicsError> {
    let layer_count =
        usize::try_from(layers).map_err(|_| error("shadow map array layer count exceeds usize"))?;
    let info = vk::VkImageCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
        imageType: vk::VK_IMAGE_TYPE_2D,
        format: DEPTH_FORMAT,
        extent: vk::VkExtent3D {
            width: size,
            height: size,
            depth: 1,
        },
        mipLevels: 1,
        arrayLayers: layers,
        samples: vk::VK_SAMPLE_COUNT_1_BIT,
        tiling: vk::VK_IMAGE_TILING_OPTIMAL,
        usage: (vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT | vk::VK_IMAGE_USAGE_SAMPLED_BIT)
            as u32,
        sharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
        initialLayout: vk::VK_IMAGE_LAYOUT_UNDEFINED,
        ..Default::default()
    };
    let device = surface.device();
    let mut array = ShadowMapArrayResource {
        image: ptr::null_mut(),
        memory: ptr::null_mut(),
        layer_views: Vec::with_capacity(layer_count),
        array_view: ptr::null_mut(),
        size,
        layers,
        rendered: false,
    };
    check(
        unsafe {
            device.functions.create_image.expect("loaded function")(
                device.handle,
                &raw const info,
                ptr::null(),
                &raw mut array.image,
            )
        },
        "vkCreateImage for shadow map array",
    )?;
    if let Err(failure) = complete_shadow_map_array_storage(device, &mut array) {
        destroy_shadow_map_array_device(device, array);
        return Err(failure);
    }
    Ok(array)
}

fn complete_shadow_map_array_storage(
    device: &super::Device,
    array: &mut ShadowMapArrayResource,
) -> Result<(), GraphicsError> {
    let mut requirements = vk::VkMemoryRequirements::default();
    unsafe {
        device
            .functions
            .get_image_memory_requirements
            .expect("loaded function")(device.handle, array.image, &raw mut requirements);
    };
    let memory_type = find_memory_type(
        device,
        requirements.memoryTypeBits,
        vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
    )
    .ok_or_else(|| error("no device-local Vulkan image memory type"))?;
    let allocate = vk::VkMemoryAllocateInfo {
        sType: vk::VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        allocationSize: requirements.size,
        memoryTypeIndex: memory_type,
        ..Default::default()
    };
    check(
        unsafe {
            device.functions.allocate_memory.expect("loaded function")(
                device.handle,
                &raw const allocate,
                ptr::null(),
                &raw mut array.memory,
            )
        },
        "vkAllocateMemory for shadow map array",
    )?;
    check(
        unsafe {
            device.functions.bind_image_memory.expect("loaded function")(
                device.handle,
                array.image,
                array.memory,
                0,
            )
        },
        "vkBindImageMemory for shadow map array",
    )?;
    for layer in 0..array.layers {
        let view_info = vk::VkImageViewCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
            image: array.image,
            viewType: vk::VK_IMAGE_VIEW_TYPE_2D,
            format: DEPTH_FORMAT,
            subresourceRange: vk::VkImageSubresourceRange {
                aspectMask: vk::VK_IMAGE_ASPECT_DEPTH_BIT as u32,
                levelCount: 1,
                baseArrayLayer: layer,
                layerCount: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut view = ptr::null_mut();
        check(
            unsafe {
                device.functions.create_image_view.expect("loaded function")(
                    device.handle,
                    &raw const view_info,
                    ptr::null(),
                    &raw mut view,
                )
            },
            "vkCreateImageView for shadow map array layer",
        )?;
        array.layer_views.push(view);
    }
    let view_info = vk::VkImageViewCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
        image: array.image,
        viewType: vk::VK_IMAGE_VIEW_TYPE_2D_ARRAY,
        format: DEPTH_FORMAT,
        subresourceRange: vk::VkImageSubresourceRange {
            aspectMask: vk::VK_IMAGE_ASPECT_DEPTH_BIT as u32,
            levelCount: 1,
            baseArrayLayer: 0,
            layerCount: array.layers,
            ..Default::default()
        },
        ..Default::default()
    };
    check(
        unsafe {
            device.functions.create_image_view.expect("loaded function")(
                device.handle,
                &raw const view_info,
                ptr::null(),
                &raw mut array.array_view,
            )
        },
        "vkCreateImageView for shadow map array",
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn create_pipeline(
    surface: &ClearSurface<'_>,
    bytes: &[u8],
    sample_count: vk::VkSampleCountFlagBits,
    instanced: bool,
) -> Result<PipelineResource, GraphicsError> {
    let device = surface.device();
    let bindings = [
        layout_binding(
            0,
            vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
            vk::VK_SHADER_STAGE_VERTEX_BIT as u32,
        ),
        layout_binding(
            1,
            vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
            vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
        ),
        layout_binding(
            2,
            vk::VK_DESCRIPTOR_TYPE_SAMPLER,
            vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
        ),
    ];
    let layout_info = vk::VkDescriptorSetLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        bindingCount: 3,
        pBindings: bindings.as_ptr(),
        ..Default::default()
    };
    let mut resource = PipelineResource {
        set_layout: ptr::null_mut(),
        layout: ptr::null_mut(),
        pipeline: ptr::null_mut(),
        descriptor_pool: ptr::null_mut(),
        sampler: ptr::null_mut(),
        bindings: Vec::new(),
    };
    check(
        unsafe {
            device
                .functions
                .create_descriptor_set_layout
                .expect("loaded function")(
                device.handle,
                &raw const layout_info,
                ptr::null(),
                &raw mut resource.set_layout,
            )
        },
        "vkCreateDescriptorSetLayout for textured pipeline",
    )?;
    let pipeline_layout = vk::VkPipelineLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        setLayoutCount: 1,
        pSetLayouts: &raw const resource.set_layout,
        ..Default::default()
    };
    check(
        unsafe {
            device
                .functions
                .create_pipeline_layout
                .expect("loaded function")(
                device.handle,
                &raw const pipeline_layout,
                ptr::null(),
                &raw mut resource.layout,
            )
        },
        "vkCreatePipelineLayout for textured pipeline",
    )?;
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect();
    let module_info = vk::VkShaderModuleCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        codeSize: bytes.len(),
        pCode: words.as_ptr(),
        ..Default::default()
    };
    let mut module = ptr::null_mut();
    check(
        unsafe {
            device
                .functions
                .create_shader_module
                .expect("loaded function")(
                device.handle,
                &raw const module_info,
                ptr::null(),
                &raw mut module,
            )
        },
        "vkCreateShaderModule for cube",
    )?;
    let stages = [
        shader_stage(
            vk::VK_SHADER_STAGE_VERTEX_BIT,
            module,
            if instanced {
                c"instanced_vertex"
            } else {
                c"cube_vertex"
            },
        ),
        shader_stage(vk::VK_SHADER_STAGE_FRAGMENT_BIT, module, c"cube_fragment"),
    ];
    let bindings = [
        vk::VkVertexInputBindingDescription {
            binding: 0,
            stride: u32::try_from(mem::size_of::<Vertex>()).expect("vertex size fits u32"),
            inputRate: vk::VK_VERTEX_INPUT_RATE_VERTEX,
        },
        vk::VkVertexInputBindingDescription {
            binding: 1,
            stride: u32::try_from(INSTANCE_TRANSFORM_SIZE)
                .expect("instance transform stride fits u32"),
            inputRate: vk::VK_VERTEX_INPUT_RATE_INSTANCE,
        },
    ];
    let attributes = [
        attribute(0, 0, vk::VK_FORMAT_R32G32B32_SFLOAT, 0),
        attribute(1, 0, vk::VK_FORMAT_R32G32B32_SFLOAT, 12),
        attribute(2, 0, vk::VK_FORMAT_R32G32_SFLOAT, 24),
        attribute(3, 1, vk::VK_FORMAT_R32G32B32A32_SFLOAT, 0),
        attribute(4, 1, vk::VK_FORMAT_R32G32B32A32_SFLOAT, 16),
        attribute(5, 1, vk::VK_FORMAT_R32G32B32A32_SFLOAT, 32),
        attribute(6, 1, vk::VK_FORMAT_R32G32B32A32_SFLOAT, 48),
    ];
    let vertex_input = vk::VkPipelineVertexInputStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        vertexBindingDescriptionCount: if instanced { 2 } else { 1 },
        pVertexBindingDescriptions: bindings.as_ptr(),
        vertexAttributeDescriptionCount: if instanced { 7 } else { 3 },
        pVertexAttributeDescriptions: attributes.as_ptr(),
        ..Default::default()
    };
    let assembly = vk::VkPipelineInputAssemblyStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
        topology: vk::VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,
        ..Default::default()
    };
    let viewport = vk::VkPipelineViewportStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
        viewportCount: 1,
        scissorCount: 1,
        ..Default::default()
    };
    let raster = vk::VkPipelineRasterizationStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
        polygonMode: vk::VK_POLYGON_MODE_FILL,
        cullMode: vk::VK_CULL_MODE_BACK_BIT as u32,
        frontFace: vk::VK_FRONT_FACE_COUNTER_CLOCKWISE,
        lineWidth: 1.0,
        ..Default::default()
    };
    let multisample = vk::VkPipelineMultisampleStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
        rasterizationSamples: sample_count,
        ..Default::default()
    };
    let depth = vk::VkPipelineDepthStencilStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
        depthTestEnable: vk::VK_TRUE,
        depthWriteEnable: vk::VK_TRUE,
        depthCompareOp: vk::VK_COMPARE_OP_LESS,
        minDepthBounds: 0.0,
        maxDepthBounds: 1.0,
        ..Default::default()
    };
    let blend_attachment = vk::VkPipelineColorBlendAttachmentState {
        colorWriteMask: (vk::VK_COLOR_COMPONENT_R_BIT
            | vk::VK_COLOR_COMPONENT_G_BIT
            | vk::VK_COLOR_COMPONENT_B_BIT
            | vk::VK_COLOR_COMPONENT_A_BIT) as u32,
        ..Default::default()
    };
    let blend = vk::VkPipelineColorBlendStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
        attachmentCount: 1,
        pAttachments: &raw const blend_attachment,
        ..Default::default()
    };
    let dynamic_states = [vk::VK_DYNAMIC_STATE_VIEWPORT, vk::VK_DYNAMIC_STATE_SCISSOR];
    let dynamic = vk::VkPipelineDynamicStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
        dynamicStateCount: 2,
        pDynamicStates: dynamic_states.as_ptr(),
        ..Default::default()
    };
    let color_format = surface.swapchain.format;
    let rendering = vk::VkPipelineRenderingCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
        colorAttachmentCount: 1,
        pColorAttachmentFormats: &raw const color_format,
        depthAttachmentFormat: DEPTH_FORMAT,
        ..Default::default()
    };
    let pipeline_info = vk::VkGraphicsPipelineCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
        pNext: (&raw const rendering).cast(),
        stageCount: 2,
        pStages: stages.as_ptr(),
        pVertexInputState: &raw const vertex_input,
        pInputAssemblyState: &raw const assembly,
        pViewportState: &raw const viewport,
        pRasterizationState: &raw const raster,
        pMultisampleState: &raw const multisample,
        pDepthStencilState: &raw const depth,
        pColorBlendState: &raw const blend,
        pDynamicState: &raw const dynamic,
        layout: resource.layout,
        basePipelineIndex: -1,
        ..Default::default()
    };
    let result = check(
        unsafe {
            device
                .functions
                .create_graphics_pipelines
                .expect("loaded function")(
                device.handle,
                ptr::null_mut(),
                1,
                &raw const pipeline_info,
                ptr::null(),
                &raw mut resource.pipeline,
            )
        },
        "vkCreateGraphicsPipelines for cube",
    );
    unsafe {
        device
            .functions
            .destroy_shader_module
            .expect("loaded function")(device.handle, module, ptr::null());
    };
    result?;
    resource.descriptor_pool = create_descriptor_pool(device)?;
    Ok(resource)
}

fn create_descriptor_pool(device: &super::Device) -> Result<vk::VkDescriptorPool, GraphicsError> {
    create_pipeline_descriptor_pool(
        device,
        "textured",
        vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
    )
}

fn create_material_descriptor_pool(
    device: &super::Device,
) -> Result<vk::VkDescriptorPool, GraphicsError> {
    create_pipeline_descriptor_pool(
        device,
        "material",
        vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
    )
}

const fn material_filter(filter: SamplerFilter) -> vk::VkFilter {
    match filter {
        SamplerFilter::Nearest => vk::VK_FILTER_NEAREST,
        SamplerFilter::Linear => vk::VK_FILTER_LINEAR,
    }
}

const fn material_address(address: SamplerAddress) -> vk::VkSamplerAddressMode {
    match address {
        SamplerAddress::Repeat => vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
        SamplerAddress::ClampToEdge => vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
    }
}

const fn material_vertex_format(format: VertexFormat) -> vk::VkFormat {
    match format {
        VertexFormat::Float32 => vk::VK_FORMAT_R32_SFLOAT,
        VertexFormat::Float32x2 => vk::VK_FORMAT_R32G32_SFLOAT,
        VertexFormat::Float32x3 => vk::VK_FORMAT_R32G32B32_SFLOAT,
        VertexFormat::Float32x4 => vk::VK_FORMAT_R32G32B32A32_SFLOAT,
        VertexFormat::Uint32 => vk::VK_FORMAT_R32_UINT,
        VertexFormat::Uint32x2 => vk::VK_FORMAT_R32G32_UINT,
        VertexFormat::Uint32x3 => vk::VK_FORMAT_R32G32B32_UINT,
        VertexFormat::Uint32x4 => vk::VK_FORMAT_R32G32B32A32_UINT,
        VertexFormat::Sint32 => vk::VK_FORMAT_R32_SINT,
        VertexFormat::Sint32x2 => vk::VK_FORMAT_R32G32_SINT,
        VertexFormat::Sint32x3 => vk::VK_FORMAT_R32G32B32_SINT,
        VertexFormat::Sint32x4 => vk::VK_FORMAT_R32G32B32A32_SINT,
    }
}

#[allow(clippy::too_many_lines)]
fn create_material_pipeline(
    surface: &ClearSurface<'_>,
    bytes: &[u8],
    config: &MaterialPipelineConfig<'_>,
    sample_count: vk::VkSampleCountFlagBits,
) -> Result<MaterialPipelineResource, GraphicsError> {
    let device = surface.device();
    let stages = (vk::VK_SHADER_STAGE_VERTEX_BIT | vk::VK_SHADER_STAGE_FRAGMENT_BIT) as u32;
    let mut layout_bindings = Vec::new();
    if let Some((binding, _)) = config.uniform {
        layout_bindings.push(layout_binding(
            binding,
            vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
            stages,
        ));
    }
    if let Some((binding, _)) = config.storage {
        layout_bindings.push(layout_binding(
            binding,
            vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC,
            stages,
        ));
    }
    for &binding in config.texture_bindings {
        layout_bindings.push(layout_binding(
            binding,
            vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
            stages,
        ));
    }
    for slot in config.sampler_bindings {
        layout_bindings.push(layout_binding(
            slot.binding,
            vk::VK_DESCRIPTOR_TYPE_SAMPLER,
            stages,
        ));
    }
    if let Some(binding) = config.depth_texture_binding {
        layout_bindings.push(layout_binding(
            binding,
            vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
            stages,
        ));
    }
    if let Some(binding) = config.depth_texture_array_binding {
        layout_bindings.push(layout_binding(
            binding,
            vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
            stages,
        ));
    }
    if let Some(binding) = config.comparison_sampler_binding {
        layout_bindings.push(layout_binding(
            binding,
            vk::VK_DESCRIPTOR_TYPE_SAMPLER,
            stages,
        ));
    }
    let layout_info = vk::VkDescriptorSetLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        bindingCount: u32::try_from(layout_bindings.len())
            .map_err(|_| error("material binding count exceeds u32"))?,
        pBindings: layout_bindings.as_ptr(),
        ..Default::default()
    };
    let mut resource = MaterialPipelineResource {
        set_layout: ptr::null_mut(),
        layout: ptr::null_mut(),
        pipeline: ptr::null_mut(),
        overlay_pipeline: ptr::null_mut(),
        descriptor_pool: ptr::null_mut(),
        samplers: Vec::with_capacity(config.sampler_bindings.len()),
        uniform: config.uniform,
        storage: config.storage,
        texture_bindings: config.texture_bindings.to_vec(),
        depth_texture_binding: config.depth_texture_binding,
        depth_texture_array_binding: config.depth_texture_array_binding,
        comparison_sampler: None,
        bindings: Vec::new(),
    };
    check(
        unsafe {
            device
                .functions
                .create_descriptor_set_layout
                .expect("loaded function")(
                device.handle,
                &raw const layout_info,
                ptr::null(),
                &raw mut resource.set_layout,
            )
        },
        "vkCreateDescriptorSetLayout for material pipeline",
    )?;
    let pipeline_layout = vk::VkPipelineLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        setLayoutCount: 1,
        pSetLayouts: &raw const resource.set_layout,
        ..Default::default()
    };
    check(
        unsafe {
            device
                .functions
                .create_pipeline_layout
                .expect("loaded function")(
                device.handle,
                &raw const pipeline_layout,
                ptr::null(),
                &raw mut resource.layout,
            )
        },
        "vkCreatePipelineLayout for material pipeline",
    )?;
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect();
    let module_info = vk::VkShaderModuleCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        codeSize: bytes.len(),
        pCode: words.as_ptr(),
        ..Default::default()
    };
    let mut module = ptr::null_mut();
    check(
        unsafe {
            device
                .functions
                .create_shader_module
                .expect("loaded function")(
                device.handle,
                &raw const module_info,
                ptr::null(),
                &raw mut module,
            )
        },
        "vkCreateShaderModule for material pipeline",
    )?;
    let vertex_entry = CString::new(config.vertex_entry)
        .map_err(|_| error("material vertex entry point name contains a NUL byte"))?;
    let fragment_entry = CString::new(config.fragment_entry)
        .map_err(|_| error("material fragment entry point name contains a NUL byte"))?;
    let shader_stages = [
        shader_stage(vk::VK_SHADER_STAGE_VERTEX_BIT, module, &vertex_entry),
        shader_stage(vk::VK_SHADER_STAGE_FRAGMENT_BIT, module, &fragment_entry),
    ];
    let vertex_binding = vk::VkVertexInputBindingDescription {
        binding: 0,
        stride: config.stride,
        inputRate: vk::VK_VERTEX_INPUT_RATE_VERTEX,
    };
    let attributes: Vec<vk::VkVertexInputAttributeDescription> = config
        .attributes
        .iter()
        .map(|input| {
            attribute(
                input.location,
                0,
                material_vertex_format(input.format),
                input.offset,
            )
        })
        .collect();
    let vertex_input = vk::VkPipelineVertexInputStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        vertexBindingDescriptionCount: 1,
        pVertexBindingDescriptions: &raw const vertex_binding,
        vertexAttributeDescriptionCount: u32::try_from(attributes.len())
            .map_err(|_| error("material attribute count exceeds u32"))?,
        pVertexAttributeDescriptions: attributes.as_ptr(),
        ..Default::default()
    };
    let assembly = vk::VkPipelineInputAssemblyStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
        topology: vk::VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,
        ..Default::default()
    };
    let viewport = vk::VkPipelineViewportStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
        viewportCount: 1,
        scissorCount: 1,
        ..Default::default()
    };
    let raster = vk::VkPipelineRasterizationStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
        polygonMode: vk::VK_POLYGON_MODE_FILL,
        cullMode: vk::VK_CULL_MODE_BACK_BIT as u32,
        frontFace: vk::VK_FRONT_FACE_COUNTER_CLOCKWISE,
        lineWidth: 1.0,
        ..Default::default()
    };
    let multisample = vk::VkPipelineMultisampleStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
        rasterizationSamples: sample_count,
        alphaToCoverageEnable: if matches!(config.blend, BlendMode::Cutout) {
            vk::VK_TRUE
        } else {
            vk::VK_FALSE
        },
        ..Default::default()
    };
    let (depth_test, depth_write, depth_compare) = match config.depth {
        DepthMode::TestWrite => (vk::VK_TRUE, vk::VK_TRUE, vk::VK_COMPARE_OP_LESS),
        DepthMode::TestOnly => (vk::VK_TRUE, vk::VK_FALSE, vk::VK_COMPARE_OP_LESS),
        DepthMode::TestWriteGreater => (vk::VK_TRUE, vk::VK_TRUE, vk::VK_COMPARE_OP_GREATER),
        DepthMode::TestOnlyGreater => (vk::VK_TRUE, vk::VK_FALSE, vk::VK_COMPARE_OP_GREATER),
        DepthMode::Off => (vk::VK_FALSE, vk::VK_FALSE, vk::VK_COMPARE_OP_LESS),
    };
    let depth = vk::VkPipelineDepthStencilStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
        depthTestEnable: depth_test,
        depthWriteEnable: depth_write,
        depthCompareOp: depth_compare,
        minDepthBounds: 0.0,
        maxDepthBounds: 1.0,
        ..Default::default()
    };
    let write_mask = (vk::VK_COLOR_COMPONENT_R_BIT
        | vk::VK_COLOR_COMPONENT_G_BIT
        | vk::VK_COLOR_COMPONENT_B_BIT
        | vk::VK_COLOR_COMPONENT_A_BIT) as u32;
    let blend_attachment = if matches!(config.blend, BlendMode::PremultipliedTranslucent) {
        vk::VkPipelineColorBlendAttachmentState {
            blendEnable: vk::VK_TRUE,
            srcColorBlendFactor: vk::VK_BLEND_FACTOR_ONE,
            dstColorBlendFactor: vk::VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            colorBlendOp: vk::VK_BLEND_OP_ADD,
            srcAlphaBlendFactor: vk::VK_BLEND_FACTOR_ONE,
            dstAlphaBlendFactor: vk::VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            alphaBlendOp: vk::VK_BLEND_OP_ADD,
            colorWriteMask: write_mask,
        }
    } else {
        vk::VkPipelineColorBlendAttachmentState {
            colorWriteMask: write_mask,
            ..Default::default()
        }
    };
    let blend = vk::VkPipelineColorBlendStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
        attachmentCount: 1,
        pAttachments: &raw const blend_attachment,
        ..Default::default()
    };
    let dynamic_states = [vk::VK_DYNAMIC_STATE_VIEWPORT, vk::VK_DYNAMIC_STATE_SCISSOR];
    let dynamic = vk::VkPipelineDynamicStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
        dynamicStateCount: 2,
        pDynamicStates: dynamic_states.as_ptr(),
        ..Default::default()
    };
    let color_format = surface.swapchain.format;
    let rendering = vk::VkPipelineRenderingCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
        colorAttachmentCount: 1,
        pColorAttachmentFormats: &raw const color_format,
        depthAttachmentFormat: DEPTH_FORMAT,
        ..Default::default()
    };
    let pipeline_info = vk::VkGraphicsPipelineCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
        pNext: (&raw const rendering).cast(),
        stageCount: 2,
        pStages: shader_stages.as_ptr(),
        pVertexInputState: &raw const vertex_input,
        pInputAssemblyState: &raw const assembly,
        pViewportState: &raw const viewport,
        pRasterizationState: &raw const raster,
        pMultisampleState: &raw const multisample,
        pDepthStencilState: &raw const depth,
        pColorBlendState: &raw const blend,
        pDynamicState: &raw const dynamic,
        layout: resource.layout,
        basePipelineIndex: -1,
        ..Default::default()
    };
    let result = check(
        unsafe {
            device
                .functions
                .create_graphics_pipelines
                .expect("loaded function")(
                device.handle,
                ptr::null_mut(),
                1,
                &raw const pipeline_info,
                ptr::null(),
                &raw mut resource.pipeline,
            )
        },
        "vkCreateGraphicsPipelines for material pipeline",
    );
    let overlay_result = if result.is_ok() && config.depth == DepthMode::Off {
        let overlay_multisample = vk::VkPipelineMultisampleStateCreateInfo {
            rasterizationSamples: vk::VK_SAMPLE_COUNT_1_BIT,
            ..multisample
        };
        let overlay_rendering = vk::VkPipelineRenderingCreateInfo {
            depthAttachmentFormat: vk::VK_FORMAT_UNDEFINED,
            ..rendering
        };
        let overlay_info = vk::VkGraphicsPipelineCreateInfo {
            pNext: (&raw const overlay_rendering).cast(),
            pMultisampleState: &raw const overlay_multisample,
            ..pipeline_info
        };
        check(
            unsafe {
                device
                    .functions
                    .create_graphics_pipelines
                    .expect("loaded function")(
                    device.handle,
                    ptr::null_mut(),
                    1,
                    &raw const overlay_info,
                    ptr::null(),
                    &raw mut resource.overlay_pipeline,
                )
            },
            "vkCreateGraphicsPipelines for material overlay pipeline",
        )
    } else {
        Ok(())
    };
    unsafe {
        device
            .functions
            .destroy_shader_module
            .expect("loaded function")(device.handle, module, ptr::null());
    };
    result?;
    overlay_result?;
    for slot in config.sampler_bindings {
        let filter = material_filter(slot.filter);
        let address = material_address(slot.address);
        let mipmap_mode = match slot.filter {
            SamplerFilter::Nearest => vk::VK_SAMPLER_MIPMAP_MODE_NEAREST,
            SamplerFilter::Linear => vk::VK_SAMPLER_MIPMAP_MODE_LINEAR,
        };
        let sampler_info = vk::VkSamplerCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,
            magFilter: filter,
            minFilter: filter,
            mipmapMode: mipmap_mode,
            addressModeU: address,
            addressModeV: address,
            addressModeW: address,
            maxAnisotropy: 1.0,
            maxLod: LOD_CLAMP_NONE,
            ..Default::default()
        };
        let mut sampler = ptr::null_mut();
        check(
            unsafe {
                device.functions.create_sampler.expect("loaded function")(
                    device.handle,
                    &raw const sampler_info,
                    ptr::null(),
                    &raw mut sampler,
                )
            },
            "vkCreateSampler for material pipeline",
        )?;
        resource.samplers.push((slot.binding, sampler));
    }
    if let Some(binding) = config.comparison_sampler_binding {
        let sampler_info = vk::VkSamplerCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,
            magFilter: vk::VK_FILTER_LINEAR,
            minFilter: vk::VK_FILTER_LINEAR,
            mipmapMode: vk::VK_SAMPLER_MIPMAP_MODE_NEAREST,
            addressModeU: vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
            addressModeV: vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
            addressModeW: vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
            maxAnisotropy: 1.0,
            compareEnable: vk::VK_TRUE,
            compareOp: vk::VK_COMPARE_OP_LESS_OR_EQUAL,
            maxLod: 0.0,
            ..Default::default()
        };
        let mut sampler = ptr::null_mut();
        check(
            unsafe {
                device.functions.create_sampler.expect("loaded function")(
                    device.handle,
                    &raw const sampler_info,
                    ptr::null(),
                    &raw mut sampler,
                )
            },
            "vkCreateSampler for material comparison sampler",
        )?;
        resource.comparison_sampler = Some((binding, sampler));
    }
    resource.descriptor_pool = create_material_descriptor_pool(device)?;
    Ok(resource)
}

/// Builds a depth-only pipeline: the module's vertex entry point rasterized into a shadow map's
/// depth attachment with no fragment stage or color target, at one sample.
#[allow(clippy::too_many_lines)]
fn create_shadow_pipeline(
    surface: &ClearSurface<'_>,
    bytes: &[u8],
    config: &ShadowPipelineConfig<'_>,
) -> Result<ShadowPipelineResource, GraphicsError> {
    let device = surface.device();
    let mut layout_bindings = Vec::new();
    if let Some((binding, _)) = config.uniform {
        layout_bindings.push(layout_binding(
            binding,
            vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
            vk::VK_SHADER_STAGE_VERTEX_BIT as u32,
        ));
    }
    if let Some((binding, _)) = config.storage {
        layout_bindings.push(layout_binding(
            binding,
            vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC,
            vk::VK_SHADER_STAGE_VERTEX_BIT as u32,
        ));
    }
    let layout_info = vk::VkDescriptorSetLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        bindingCount: u32::try_from(layout_bindings.len())
            .map_err(|_| error("shadow binding count exceeds u32"))?,
        pBindings: layout_bindings.as_ptr(),
        ..Default::default()
    };
    let mut resource = ShadowPipelineResource {
        set_layout: ptr::null_mut(),
        layout: ptr::null_mut(),
        pipeline: ptr::null_mut(),
        descriptor_pool: ptr::null_mut(),
        uniform: config.uniform,
        storage: config.storage,
        descriptor: None,
    };
    check(
        unsafe {
            device
                .functions
                .create_descriptor_set_layout
                .expect("loaded function")(
                device.handle,
                &raw const layout_info,
                ptr::null(),
                &raw mut resource.set_layout,
            )
        },
        "vkCreateDescriptorSetLayout for shadow pipeline",
    )?;
    let pipeline_layout = vk::VkPipelineLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        setLayoutCount: 1,
        pSetLayouts: &raw const resource.set_layout,
        ..Default::default()
    };
    check(
        unsafe {
            device
                .functions
                .create_pipeline_layout
                .expect("loaded function")(
                device.handle,
                &raw const pipeline_layout,
                ptr::null(),
                &raw mut resource.layout,
            )
        },
        "vkCreatePipelineLayout for shadow pipeline",
    )?;
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect();
    let module_info = vk::VkShaderModuleCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        codeSize: bytes.len(),
        pCode: words.as_ptr(),
        ..Default::default()
    };
    let mut module = ptr::null_mut();
    check(
        unsafe {
            device
                .functions
                .create_shader_module
                .expect("loaded function")(
                device.handle,
                &raw const module_info,
                ptr::null(),
                &raw mut module,
            )
        },
        "vkCreateShaderModule for shadow pipeline",
    )?;
    let vertex_entry = CString::new(config.vertex_entry)
        .map_err(|_| error("shadow vertex entry point name contains a NUL byte"))?;
    let shader_stages = [shader_stage(
        vk::VK_SHADER_STAGE_VERTEX_BIT,
        module,
        &vertex_entry,
    )];
    let vertex_binding = vk::VkVertexInputBindingDescription {
        binding: 0,
        stride: config.stride,
        inputRate: vk::VK_VERTEX_INPUT_RATE_VERTEX,
    };
    let attributes: Vec<vk::VkVertexInputAttributeDescription> = config
        .attributes
        .iter()
        .map(|input| {
            attribute(
                input.location,
                0,
                material_vertex_format(input.format),
                input.offset,
            )
        })
        .collect();
    let vertex_input = vk::VkPipelineVertexInputStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        vertexBindingDescriptionCount: 1,
        pVertexBindingDescriptions: &raw const vertex_binding,
        vertexAttributeDescriptionCount: u32::try_from(attributes.len())
            .map_err(|_| error("shadow attribute count exceeds u32"))?,
        pVertexAttributeDescriptions: attributes.as_ptr(),
        ..Default::default()
    };
    let assembly = vk::VkPipelineInputAssemblyStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
        topology: vk::VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,
        ..Default::default()
    };
    let viewport = vk::VkPipelineViewportStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
        viewportCount: 1,
        scissorCount: 1,
        ..Default::default()
    };
    let raster = vk::VkPipelineRasterizationStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
        polygonMode: vk::VK_POLYGON_MODE_FILL,
        cullMode: vk::VK_CULL_MODE_BACK_BIT as u32,
        frontFace: vk::VK_FRONT_FACE_COUNTER_CLOCKWISE,
        lineWidth: 1.0,
        ..Default::default()
    };
    let multisample = vk::VkPipelineMultisampleStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
        rasterizationSamples: vk::VK_SAMPLE_COUNT_1_BIT,
        ..Default::default()
    };
    let depth = vk::VkPipelineDepthStencilStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
        depthTestEnable: vk::VK_TRUE,
        depthWriteEnable: vk::VK_TRUE,
        depthCompareOp: vk::VK_COMPARE_OP_LESS,
        minDepthBounds: 0.0,
        maxDepthBounds: 1.0,
        ..Default::default()
    };
    let blend = vk::VkPipelineColorBlendStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
        ..Default::default()
    };
    let dynamic_states = [vk::VK_DYNAMIC_STATE_VIEWPORT, vk::VK_DYNAMIC_STATE_SCISSOR];
    let dynamic = vk::VkPipelineDynamicStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
        dynamicStateCount: 2,
        pDynamicStates: dynamic_states.as_ptr(),
        ..Default::default()
    };
    let rendering = vk::VkPipelineRenderingCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
        depthAttachmentFormat: DEPTH_FORMAT,
        ..Default::default()
    };
    let pipeline_info = vk::VkGraphicsPipelineCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
        pNext: (&raw const rendering).cast(),
        stageCount: 1,
        pStages: shader_stages.as_ptr(),
        pVertexInputState: &raw const vertex_input,
        pInputAssemblyState: &raw const assembly,
        pViewportState: &raw const viewport,
        pRasterizationState: &raw const raster,
        pMultisampleState: &raw const multisample,
        pDepthStencilState: &raw const depth,
        pColorBlendState: &raw const blend,
        pDynamicState: &raw const dynamic,
        layout: resource.layout,
        basePipelineIndex: -1,
        ..Default::default()
    };
    let result = check(
        unsafe {
            device
                .functions
                .create_graphics_pipelines
                .expect("loaded function")(
                device.handle,
                ptr::null_mut(),
                1,
                &raw const pipeline_info,
                ptr::null(),
                &raw mut resource.pipeline,
            )
        },
        "vkCreateGraphicsPipelines for shadow pipeline",
    );
    unsafe {
        device
            .functions
            .destroy_shader_module
            .expect("loaded function")(device.handle, module, ptr::null());
    };
    result?;
    resource.descriptor_pool = create_material_descriptor_pool(device)?;
    Ok(resource)
}

#[allow(clippy::too_many_lines)]
fn create_postprocess_pipeline(
    surface: &ClearSurface<'_>,
    bytes: &[u8],
) -> Result<PipelineResource, GraphicsError> {
    let device = surface.device();
    let bindings = [
        layout_binding(
            0,
            vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
            vk::VK_SHADER_STAGE_VERTEX_BIT as u32,
        ),
        layout_binding(
            1,
            vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
            vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
        ),
        layout_binding(
            2,
            vk::VK_DESCRIPTOR_TYPE_SAMPLER,
            vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
        ),
    ];
    let layout_info = vk::VkDescriptorSetLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        bindingCount: 3,
        pBindings: bindings.as_ptr(),
        ..Default::default()
    };
    let mut resource = PipelineResource {
        set_layout: ptr::null_mut(),
        layout: ptr::null_mut(),
        pipeline: ptr::null_mut(),
        descriptor_pool: ptr::null_mut(),
        sampler: ptr::null_mut(),
        bindings: Vec::new(),
    };
    check(
        unsafe {
            device
                .functions
                .create_descriptor_set_layout
                .expect("loaded function")(
                device.handle,
                &raw const layout_info,
                ptr::null(),
                &raw mut resource.set_layout,
            )
        },
        "vkCreateDescriptorSetLayout for postprocess pipeline",
    )?;
    let pipeline_layout = vk::VkPipelineLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        setLayoutCount: 1,
        pSetLayouts: &raw const resource.set_layout,
        ..Default::default()
    };
    check(
        unsafe {
            device
                .functions
                .create_pipeline_layout
                .expect("loaded function")(
                device.handle,
                &raw const pipeline_layout,
                ptr::null(),
                &raw mut resource.layout,
            )
        },
        "vkCreatePipelineLayout for postprocess pipeline",
    )?;
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect();
    let module_info = vk::VkShaderModuleCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        codeSize: bytes.len(),
        pCode: words.as_ptr(),
        ..Default::default()
    };
    let mut module = ptr::null_mut();
    check(
        unsafe {
            device
                .functions
                .create_shader_module
                .expect("loaded function")(
                device.handle,
                &raw const module_info,
                ptr::null(),
                &raw mut module,
            )
        },
        "vkCreateShaderModule for postprocess pipeline",
    )?;
    let stages = [
        shader_stage(vk::VK_SHADER_STAGE_VERTEX_BIT, module, c"post_vertex"),
        shader_stage(vk::VK_SHADER_STAGE_FRAGMENT_BIT, module, c"post_fragment"),
    ];
    let vertex_input = vk::VkPipelineVertexInputStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        ..Default::default()
    };
    let assembly = vk::VkPipelineInputAssemblyStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
        topology: vk::VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,
        ..Default::default()
    };
    let viewport = vk::VkPipelineViewportStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
        viewportCount: 1,
        scissorCount: 1,
        ..Default::default()
    };
    let raster = vk::VkPipelineRasterizationStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
        polygonMode: vk::VK_POLYGON_MODE_FILL,
        cullMode: vk::VK_CULL_MODE_NONE as u32,
        frontFace: vk::VK_FRONT_FACE_COUNTER_CLOCKWISE,
        lineWidth: 1.0,
        ..Default::default()
    };
    let multisample = vk::VkPipelineMultisampleStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
        rasterizationSamples: vk::VK_SAMPLE_COUNT_1_BIT,
        ..Default::default()
    };
    let blend_attachment = vk::VkPipelineColorBlendAttachmentState {
        colorWriteMask: (vk::VK_COLOR_COMPONENT_R_BIT
            | vk::VK_COLOR_COMPONENT_G_BIT
            | vk::VK_COLOR_COMPONENT_B_BIT
            | vk::VK_COLOR_COMPONENT_A_BIT) as u32,
        ..Default::default()
    };
    let blend = vk::VkPipelineColorBlendStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
        attachmentCount: 1,
        pAttachments: &raw const blend_attachment,
        ..Default::default()
    };
    let dynamic_states = [vk::VK_DYNAMIC_STATE_VIEWPORT, vk::VK_DYNAMIC_STATE_SCISSOR];
    let dynamic = vk::VkPipelineDynamicStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
        dynamicStateCount: 2,
        pDynamicStates: dynamic_states.as_ptr(),
        ..Default::default()
    };
    let color_format = surface.swapchain.format;
    let rendering = vk::VkPipelineRenderingCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
        colorAttachmentCount: 1,
        pColorAttachmentFormats: &raw const color_format,
        ..Default::default()
    };
    let pipeline_info = vk::VkGraphicsPipelineCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
        pNext: (&raw const rendering).cast(),
        stageCount: 2,
        pStages: stages.as_ptr(),
        pVertexInputState: &raw const vertex_input,
        pInputAssemblyState: &raw const assembly,
        pViewportState: &raw const viewport,
        pRasterizationState: &raw const raster,
        pMultisampleState: &raw const multisample,
        pColorBlendState: &raw const blend,
        pDynamicState: &raw const dynamic,
        layout: resource.layout,
        basePipelineIndex: -1,
        ..Default::default()
    };
    let result = check(
        unsafe {
            device
                .functions
                .create_graphics_pipelines
                .expect("loaded function")(
                device.handle,
                ptr::null_mut(),
                1,
                &raw const pipeline_info,
                ptr::null(),
                &raw mut resource.pipeline,
            )
        },
        "vkCreateGraphicsPipelines for postprocess pipeline",
    );
    unsafe {
        device
            .functions
            .destroy_shader_module
            .expect("loaded function")(device.handle, module, ptr::null());
    }
    result?;
    let sampler_info = vk::VkSamplerCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,
        magFilter: vk::VK_FILTER_LINEAR,
        minFilter: vk::VK_FILTER_LINEAR,
        mipmapMode: vk::VK_SAMPLER_MIPMAP_MODE_NEAREST,
        addressModeU: vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
        addressModeV: vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
        addressModeW: vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
        maxAnisotropy: 1.0,
        maxLod: 0.0,
        ..Default::default()
    };
    check(
        unsafe {
            device.functions.create_sampler.expect("loaded function")(
                device.handle,
                &raw const sampler_info,
                ptr::null(),
                &raw mut resource.sampler,
            )
        },
        "vkCreateSampler for postprocess pipeline",
    )?;
    resource.descriptor_pool = create_postprocess_descriptor_pool(device)?;
    Ok(resource)
}

fn create_postprocess_descriptor_pool(
    device: &super::Device,
) -> Result<vk::VkDescriptorPool, GraphicsError> {
    create_pipeline_descriptor_pool(device, "postprocess", vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER)
}

fn create_pipeline_descriptor_pool(
    device: &super::Device,
    label: &str,
    uniform_type: vk::VkDescriptorType,
) -> Result<vk::VkDescriptorPool, GraphicsError> {
    let pool_sizes = [
        pool_size(uniform_type),
        pool_size(vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC),
        pool_size(vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE),
        pool_size(vk::VK_DESCRIPTOR_TYPE_SAMPLER),
    ];
    let pool_info = vk::VkDescriptorPoolCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
        maxSets: 64,
        poolSizeCount: 4,
        pPoolSizes: pool_sizes.as_ptr(),
        ..Default::default()
    };
    let mut pool = ptr::null_mut();
    check(
        unsafe {
            device
                .functions
                .create_descriptor_pool
                .expect("loaded function")(
                device.handle,
                &raw const pool_info,
                ptr::null(),
                &raw mut pool,
            )
        },
        &format!("vkCreateDescriptorPool for {label} pipeline"),
    )?;
    Ok(pool)
}

fn find_memory_type(device: &super::Device, compatible: u32, required: u32) -> Option<u32> {
    let mut properties = vk::VkPhysicalDeviceMemoryProperties::default();
    unsafe {
        device
            .instance
            .functions
            .get_physical_device_memory_properties
            .expect("loaded function")(device.adapter.handle, &raw mut properties);
    };
    properties.memoryTypes[..usize::try_from(properties.memoryTypeCount).ok()?]
        .iter()
        .enumerate()
        .find(|(index, memory)| {
            compatible & (1_u32 << index) != 0 && memory.propertyFlags & required == required
        })
        .and_then(|(index, _)| u32::try_from(index).ok())
}

fn destroy_buffer(surface: &ClearSurface<'_>, buffer: Buffer) {
    unsafe { destroy_buffer_device(surface.device(), buffer) }
}
unsafe fn destroy_buffer_device(device: &super::Device, buffer: Buffer) {
    unsafe {
        if !buffer.handle.is_null() {
            device.functions.destroy_buffer.expect("loaded function")(
                device.handle,
                buffer.handle,
                ptr::null(),
            );
        }
        if !buffer.memory.is_null() {
            device.functions.free_memory.expect("loaded function")(
                device.handle,
                buffer.memory,
                ptr::null(),
            );
        }
    }
}
fn destroy_image(surface: &ClearSurface<'_>, image: Image) {
    unsafe { destroy_image_device(surface.device(), image) }
}
unsafe fn destroy_image_device(device: &super::Device, image: Image) {
    unsafe {
        if !image.view.is_null() {
            device
                .functions
                .destroy_image_view
                .expect("loaded function")(device.handle, image.view, ptr::null());
        }
        if !image.handle.is_null() {
            device.functions.destroy_image.expect("loaded function")(
                device.handle,
                image.handle,
                ptr::null(),
            );
        }
        if !image.memory.is_null() {
            device.functions.free_memory.expect("loaded function")(
                device.handle,
                image.memory,
                ptr::null(),
            );
        }
    }
}

fn bytes_of<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(ptr::from_ref(value).cast(), mem::size_of::<T>()) }
}
fn bytes_of_slice<T>(values: &[T]) -> &[u8] {
    unsafe { slice::from_raw_parts(values.as_ptr().cast(), mem::size_of_val(values)) }
}
const fn depth_subresource_range() -> vk::VkImageSubresourceRange {
    depth_subresource_layers(1)
}
const fn depth_subresource_layers(layers: u32) -> vk::VkImageSubresourceRange {
    vk::VkImageSubresourceRange {
        aspectMask: vk::VK_IMAGE_ASPECT_DEPTH_BIT as u32,
        baseMipLevel: 0,
        levelCount: 1,
        baseArrayLayer: 0,
        layerCount: layers,
    }
}
const fn color_subresource_levels(levels: u32) -> vk::VkImageSubresourceRange {
    vk::VkImageSubresourceRange {
        aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
        baseMipLevel: 0,
        levelCount: levels,
        baseArrayLayer: 0,
        layerCount: 1,
    }
}
#[allow(clippy::too_many_arguments)]
fn image_barrier(
    image: vk::VkImage,
    old: vk::VkImageLayout,
    new: vk::VkImageLayout,
    src_stage: vk::VkPipelineStageFlags2,
    dst_stage: vk::VkPipelineStageFlags2,
    src_access: vk::VkAccessFlags2,
    dst_access: vk::VkAccessFlags2,
    range: vk::VkImageSubresourceRange,
) -> vk::VkImageMemoryBarrier2 {
    vk::VkImageMemoryBarrier2 {
        sType: vk::VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
        srcStageMask: src_stage,
        srcAccessMask: src_access,
        dstStageMask: dst_stage,
        dstAccessMask: dst_access,
        oldLayout: old,
        newLayout: new,
        srcQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
        dstQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
        image,
        subresourceRange: range,
        ..Default::default()
    }
}
fn pipeline_barrier(surface: &ClearSurface<'_>, barrier: &vk::VkImageMemoryBarrier2) {
    pipeline_barriers(surface, slice::from_ref(barrier));
}
fn pipeline_barriers(surface: &ClearSurface<'_>, barriers: &[vk::VkImageMemoryBarrier2]) {
    let dependency = vk::VkDependencyInfo {
        sType: vk::VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
        imageMemoryBarrierCount: u32::try_from(barriers.len()).expect("barrier count fits u32"),
        pImageMemoryBarriers: barriers.as_ptr(),
        ..Default::default()
    };
    unsafe {
        surface
            .device()
            .functions
            .cmd_pipeline_barrier2
            .expect("loaded function")(surface.command_buffer, &raw const dependency);
    }
}
fn descriptor_write(
    set: vk::VkDescriptorSet,
    binding: u32,
    descriptor_type: vk::VkDescriptorType,
    info: *const c_void,
) -> vk::VkWriteDescriptorSet {
    let mut write = vk::VkWriteDescriptorSet {
        sType: vk::VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
        dstSet: set,
        dstBinding: binding,
        descriptorCount: 1,
        descriptorType: descriptor_type,
        ..Default::default()
    };
    if matches!(
        descriptor_type,
        vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER
            | vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC
            | vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC
    ) {
        write.pBufferInfo = info.cast();
    } else {
        write.pImageInfo = info.cast();
    }
    write
}
const fn layout_binding(
    binding: u32,
    descriptor_type: vk::VkDescriptorType,
    stages: u32,
) -> vk::VkDescriptorSetLayoutBinding {
    vk::VkDescriptorSetLayoutBinding {
        binding,
        descriptorType: descriptor_type,
        descriptorCount: 1,
        stageFlags: stages,
        pImmutableSamplers: ptr::null(),
    }
}
const fn pool_size(descriptor_type: vk::VkDescriptorType) -> vk::VkDescriptorPoolSize {
    vk::VkDescriptorPoolSize {
        type_: descriptor_type,
        descriptorCount: 64,
    }
}
const fn attribute(
    location: u32,
    binding: u32,
    format: vk::VkFormat,
    offset: u32,
) -> vk::VkVertexInputAttributeDescription {
    vk::VkVertexInputAttributeDescription {
        location,
        binding,
        format,
        offset,
    }
}
fn shader_stage(
    stage: vk::VkShaderStageFlagBits,
    module: vk::VkShaderModule,
    name: &core::ffi::CStr,
) -> vk::VkPipelineShaderStageCreateInfo {
    vk::VkPipelineShaderStageCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
        stage,
        module,
        pName: name.as_ptr(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::timestamp_tick_delta;

    #[test]
    fn timestamp_delta_handles_queue_counter_wraparound() {
        assert_eq!(timestamp_tick_delta(1_000, 1_025, 64), 25);
        assert_eq!(timestamp_tick_delta(250, 7, 8), 13);
    }
}
