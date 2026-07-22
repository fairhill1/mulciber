use core::ffi::c_void;
use core::{mem, ptr};
use std::format;
use std::vec::Vec;

use mulciber_platform::{SurfaceTarget, WindowMetrics};

use std::ffi::CString;

use super::{ClearSurface, MetalFrameToken, objc, required};
use crate::graphics::{
    BlendMode, DepthMode, MaterialPipelineConfig, MeshIndices, PostprocessPipelineConfig,
    Rgba8TextureFormat, SamplerAddress, SamplerFilter, ShadowPipelineConfig, mip_extent,
};
use crate::resource::{Arena, DestroyRequest, ResourceId, ResourceKind};
use crate::{
    ClearColor, DeviceRequest, FrameAcquire, FrameDisposition, GeometrySource, GraphicsError,
    MaterialRecord, PresentFeedback, SampleCount, ShaderArtifact, ShadowPrepass, ShadowRecord,
    ShadowSource, SurfaceInfo, TexturedInstanceBatch, TexturedSceneDraw, Vertex, VertexFormat,
};

use objc::{Object, Origin3, Region3, Size3};

const PIXEL_FORMAT_BGRA8_UNORM_SRGB: usize = 81;
const PIXEL_FORMAT_RGBA8_UNORM: usize = 70;
const PIXEL_FORMAT_RGBA8_UNORM_SRGB: usize = 71;
const PIXEL_FORMAT_DEPTH32_FLOAT: usize = 252;
const PIXEL_FORMAT_INVALID: usize = 0;
const VERTEX_FORMAT_FLOAT: usize = 28;
const VERTEX_FORMAT_FLOAT2: usize = 29;
const VERTEX_FORMAT_FLOAT3: usize = 30;
const VERTEX_FORMAT_FLOAT4: usize = 31;
const VERTEX_FORMAT_INT: usize = 32;
const VERTEX_FORMAT_INT2: usize = 33;
const VERTEX_FORMAT_INT3: usize = 34;
const VERTEX_FORMAT_INT4: usize = 35;
const VERTEX_FORMAT_UINT: usize = 36;
const VERTEX_FORMAT_UINT2: usize = 37;
const VERTEX_FORMAT_UINT3: usize = 38;
const VERTEX_FORMAT_UINT4: usize = 39;
/// Buffer index feeding material vertex data. Declared material binding slots are capped at
/// [`crate::MATERIAL_SLOT_LIMIT`], so this index cannot collide with a WGSL buffer binding.
const MATERIAL_VERTEX_BUFFER_INDEX: usize = 30;
const VERTEX_STEP_FUNCTION_PER_INSTANCE: usize = 2;
const LOAD_ACTION_CLEAR: usize = 2;
const STORE_ACTION_STORE: usize = 1;
const STORE_ACTION_DONT_CARE: usize = 0;
const STORE_ACTION_MULTISAMPLE_RESOLVE: usize = 2;
const PRIMITIVE_TYPE_TRIANGLE: usize = 3;
const INDEX_TYPE_UINT16: usize = 0;
const INDEX_TYPE_UINT32: usize = 1;
const STORAGE_MODE_PRIVATE: usize = 2;
const STORAGE_MODE_MEMORYLESS: usize = 3;
const TEXTURE_TYPE_2D_ARRAY: usize = 3;
const TEXTURE_TYPE_2D_MULTISAMPLE: usize = 4;
const TEXTURE_USAGE_SHADER_READ: usize = 1;
const TEXTURE_USAGE_RENDER_TARGET: usize = 4;
const COMPARE_FUNCTION_LESS: usize = 1;
const COMPARE_FUNCTION_LESS_EQUAL: usize = 3;
const COMPARE_FUNCTION_GREATER: usize = 4;
const COMPARE_FUNCTION_ALWAYS: usize = 7;
/// Far-plane depth clear for conventional less-compare scenes; reversed-Z material scenes
/// clear to 0.0 instead, selected per submission by the validated `depth_clear` argument.
const DEPTH_CLEAR_FAR: f32 = 1.0;
const BLEND_FACTOR_ONE: usize = 1;
const BLEND_FACTOR_ONE_MINUS_SOURCE_ALPHA: usize = 5;
const SAMPLER_FILTER_NEAREST: usize = 0;
const SAMPLER_FILTER_LINEAR: usize = 1;
const SAMPLER_MIP_FILTER_NEAREST: usize = 1;
const SAMPLER_MIP_FILTER_LINEAR: usize = 2;
const SAMPLER_ADDRESS_CLAMP_TO_EDGE: usize = 0;
const SAMPLER_ADDRESS_REPEAT: usize = 2;
const DRAW_UNIFORM_SIZE: usize = 64;
const DRAW_UNIFORM_STRIDE: usize = 256;
/// Alignment for per-record offsets into the frame's read-only storage region, matching the
/// uniform stride and every Metal buffer-offset requirement.
const STORAGE_OFFSET_ALIGNMENT: usize = 256;
const INSTANCE_TRANSFORM_SIZE: usize = 64;
/// Vertex buffer index carrying instance-rate attributes for material and shadow records,
/// beside the per-vertex data at [`MATERIAL_VERTEX_BUFFER_INDEX`] and clear of the material
/// binding slots 0 through 15.
const MATERIAL_INSTANCE_BUFFER_INDEX: usize = 29;

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
    storage: Object,
    parts: Vec<MeshPartResource>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MeshPartResource {
    index_offset: usize,
    indirect_offset: usize,
    index_count: u32,
    index_type: usize,
}

fn pack_mesh_storage(
    vertices: &[u8],
    index_parts: &[MeshIndices<'_>],
) -> Result<(Vec<u8>, Vec<MeshPartResource>), GraphicsError> {
    let mut cursor = vertices
        .len()
        .checked_next_multiple_of(4)
        .ok_or_else(|| GraphicsError::new("Metal mesh index offsets overflow"))?;
    let mut parts = Vec::new();
    parts.try_reserve_exact(index_parts.len()).map_err(|_| {
        GraphicsError::new("Metal mesh part metadata allocation could not be reserved")
    })?;
    for (part_index, indices) in index_parts.iter().enumerate() {
        let bytes = mesh_index_bytes(*indices);
        let index_offset = cursor;
        cursor = cursor.checked_add(bytes.len()).ok_or_else(|| {
            GraphicsError::new(format!(
                "Metal mesh index part {part_index} end offset overflows"
            ))
        })?;
        cursor = cursor.checked_next_multiple_of(4).ok_or_else(|| {
            GraphicsError::new(format!(
                "Metal mesh index part {part_index} alignment overflows"
            ))
        })?;
        parts.push(MeshPartResource {
            index_offset,
            indirect_offset: 0,
            index_count: u32::try_from(indices.len()).map_err(|_| {
                GraphicsError::new(format!(
                    "Metal mesh index part {part_index} count exceeds u32"
                ))
            })?,
            index_type: match indices {
                MeshIndices::U16(_) => INDEX_TYPE_UINT16,
                MeshIndices::U32(_) => INDEX_TYPE_UINT32,
            },
        });
    }
    for part in &mut parts {
        part.indirect_offset = cursor;
        cursor = cursor
            .checked_add(mem::size_of::<IndexedIndirectArguments>())
            .ok_or_else(|| GraphicsError::new("Metal mesh indirect offsets overflow"))?;
    }
    let allocation_size = cursor
        .checked_next_multiple_of(16)
        .ok_or_else(|| GraphicsError::new("Metal mesh allocation size overflows"))?;
    let mut packed = Vec::new();
    packed
        .try_reserve_exact(allocation_size)
        .map_err(|_| GraphicsError::new("Metal mesh storage allocation could not be reserved"))?;
    packed.resize(allocation_size, 0);
    packed[..vertices.len()].copy_from_slice(vertices);
    for ((indices, part), part_index) in index_parts.iter().zip(&parts).zip(0..) {
        let bytes = mesh_index_bytes(*indices);
        let index_end = part
            .index_offset
            .checked_add(bytes.len())
            .ok_or_else(|| GraphicsError::new("Metal mesh index copy range overflows"))?;
        packed[part.index_offset..index_end].copy_from_slice(bytes);
        let draw = IndexedIndirectArguments {
            index_count: part.index_count,
            instance_count: 1,
            index_start: 0,
            base_vertex: 0,
            base_instance: 0,
        };
        let draw_bytes = unsafe {
            core::slice::from_raw_parts(ptr::from_ref(&draw).cast::<u8>(), mem::size_of_val(&draw))
        };
        let indirect_end = part
            .indirect_offset
            .checked_add(draw_bytes.len())
            .ok_or_else(|| {
                GraphicsError::new(format!(
                    "Metal mesh indirect part {part_index} copy range overflows"
                ))
            })?;
        packed[part.indirect_offset..indirect_end].copy_from_slice(draw_bytes);
    }
    Ok((packed, parts))
}

fn mesh_index_bytes(indices: MeshIndices<'_>) -> &[u8] {
    match indices {
        MeshIndices::U16(indices) => unsafe {
            core::slice::from_raw_parts(indices.as_ptr().cast(), mem::size_of_val(indices))
        },
        MeshIndices::U32(indices) => unsafe {
            core::slice::from_raw_parts(indices.as_ptr().cast(), mem::size_of_val(indices))
        },
    }
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
    uniform_size: u32,
}

struct MaterialPipelineResource {
    pipeline: Object,
    /// Single-sample no-depth variant for the presentable overlay pass; null unless the
    /// pipeline declares [`DepthMode::Off`].
    overlay_pipeline: Object,
    depth_state: Object,
    /// One pipeline-owned sampler state per declared slot as (binding, sampler).
    samplers: Vec<(u32, Object)>,
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
    comparison_sampler: Option<(u32, Object)>,
    /// Declared per-instance stride in bytes; zero when no instance layout is declared.
    instance_stride: usize,
}

struct ShadowMapResource {
    texture: Object,
    /// Whether any shadow pass has rendered into this map; sampling before that is rejected.
    rendered: bool,
}

struct ShadowMapArrayResource {
    texture: Object,
    /// Whether any cascaded shadow pass has rendered into this array; sampling before that is
    /// rejected.
    rendered: bool,
}

struct ShadowPipelineResource {
    pipeline: Object,
    depth_state: Object,
    /// Declared uniform slot as (binding, size).
    uniform: Option<(u32, u32)>,
    /// Declared read-only storage slot as (binding, size).
    storage: Option<(u32, u32)>,
    /// Whether the pipeline runs a declared fragment stage; buffer bindings mirror to the
    /// fragment stage when set.
    fragment: bool,
    /// Declared texture binding numbers in ascending order.
    texture_bindings: Vec<u32>,
    /// One pipeline-owned sampler state per declared slot as (binding, sampler).
    samplers: Vec<(u32, Object)>,
    /// Declared per-instance stride in bytes; zero when no instance layout is declared.
    instance_stride: usize,
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

/// Geometry for one material record resolved at encoding: an uploaded mesh's arena index, or
/// offsets into the frame's transient-geometry region recomputed in staging order.
#[derive(Clone, Copy)]
enum ResolvedGeometry {
    Mesh {
        mesh: usize,
        part: usize,
    },
    Transient {
        vertex_offset: usize,
        index_offset: usize,
        index_count: usize,
        index_type: usize,
    },
}

#[derive(Clone, Copy)]
enum PreparedScene<'resources> {
    Draws(&'resources [TexturedSceneDraw<'resources>]),
    Instances,
    Materials(&'resources [MaterialRecord<'resources>]),
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
    /// Frame-transient read-only storage region for material and shadow records, in bytes.
    storage: Object,
    storage_capacity: usize,
    /// Frame-transient indexed-geometry region for material records, in bytes.
    transient_geometry: Object,
    transient_capacity: usize,
    instance_transforms: Object,
    instance_capacity: usize,
    /// Frame-transient instance-rate attribute region for material and shadow records, in
    /// bytes.
    record_instances: Object,
    record_instance_capacity: usize,
    resolved_instance_batches: Vec<ResolvedInstanceBatch>,
    meshes: Arena<MeshResource>,
    textures: Arena<TextureResource>,
    pipelines: Arena<PipelineResource>,
    instanced_pipelines: Arena<PipelineResource>,
    material_pipelines: Arena<MaterialPipelineResource>,
    shadow_maps: Arena<ShadowMapResource>,
    shadow_map_arrays: Arena<ShadowMapArrayResource>,
    shadow_pipelines: Arena<ShadowPipelineResource>,
    postprocess_pipelines: Arena<PostprocessPipelineResource>,
    targets: Arena<TargetResource>,
    postprocess_targets: Arena<PostprocessTargetResource>,
}

impl<'window> TexturedSession<'window> {
    #[allow(clippy::too_many_lines)]
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
        let storage = match unsafe {
            required(
                objc::object_two_usizes(
                    surface.device,
                    c"newBufferWithLength:options:",
                    STORAGE_OFFSET_ALIGNMENT,
                    0,
                ),
                "Metal record storage buffer",
            )
        } {
            Ok(buffer) => buffer,
            Err(failure) => {
                unsafe { objc::void(uniform, c"release") };
                return Err(failure);
            }
        };
        let transient_geometry = match unsafe {
            required(
                objc::object_two_usizes(
                    surface.device,
                    c"newBufferWithLength:options:",
                    STORAGE_OFFSET_ALIGNMENT,
                    0,
                ),
                "Metal transient geometry buffer",
            )
        } {
            Ok(buffer) => buffer,
            Err(failure) => {
                unsafe {
                    objc::void(storage, c"release");
                    objc::void(uniform, c"release");
                };
                return Err(failure);
            }
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
                unsafe {
                    objc::void(transient_geometry, c"release");
                    objc::void(storage, c"release");
                    objc::void(uniform, c"release");
                };
                return Err(failure);
            }
        };
        let record_instances = match unsafe {
            required(
                objc::object_two_usizes(
                    surface.device,
                    c"newBufferWithLength:options:",
                    STORAGE_OFFSET_ALIGNMENT,
                    0,
                ),
                "Metal record instance buffer",
            )
        } {
            Ok(buffer) => buffer,
            Err(failure) => {
                unsafe {
                    objc::void(instance_transforms, c"release");
                    objc::void(transient_geometry, c"release");
                    objc::void(storage, c"release");
                    objc::void(uniform, c"release");
                };
                return Err(failure);
            }
        };
        Ok((
            Self {
                surface,
                sample_count,
                uniform,
                uniform_capacity: 1,
                storage,
                storage_capacity: STORAGE_OFFSET_ALIGNMENT,
                transient_geometry,
                transient_capacity: STORAGE_OFFSET_ALIGNMENT,
                instance_transforms,
                instance_capacity: 1,
                record_instances,
                record_instance_capacity: STORAGE_OFFSET_ALIGNMENT,
                resolved_instance_batches: Vec::new(),
                meshes: Arena::new("mesh"),
                textures: Arena::new("texture"),
                pipelines: Arena::new("textured pipeline"),
                instanced_pipelines: Arena::new("instanced textured pipeline"),
                material_pipelines: Arena::new("material pipeline"),
                shadow_maps: Arena::new("shadow map"),
                shadow_map_arrays: Arena::new("shadow map array"),
                shadow_pipelines: Arena::new("shadow pipeline"),
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

    pub(crate) const fn gpu_timing_support(&self) -> crate::GpuTimingSupport {
        crate::GpuTimingSupport::Frame
    }

    pub(crate) fn set_gpu_timing_enabled(&mut self, enabled: bool) -> Result<(), GraphicsError> {
        self.surface.enable_gpu_timing(enabled);
        Ok(())
    }

    pub(crate) fn take_gpu_timings(&mut self) -> crate::GpuTimingFeedback {
        self.surface.take_gpu_timings()
    }

    pub(crate) fn acquire(
        &mut self,
        metrics: WindowMetrics,
    ) -> Result<FrameAcquire<TexturedFrameToken>, GraphicsError> {
        let acquisition = self.surface.acquire_drawable(metrics)?;
        self.reclaim_stale_targets();
        Ok(acquisition.map_ready(TexturedFrameToken))
    }

    pub(crate) fn take_present_feedback(&mut self) -> PresentFeedback {
        self.surface.take_present_feedback()
    }

    pub(crate) fn create_mesh(
        &mut self,
        vertices: &[Vertex],
        parts: &[MeshIndices<'_>],
    ) -> Result<ResourceId, GraphicsError> {
        let bytes = unsafe {
            core::slice::from_raw_parts(vertices.as_ptr().cast(), mem::size_of_val(vertices))
        };
        self.create_mesh_from_bytes(bytes, parts)
    }

    pub(crate) fn create_mesh_from_bytes(
        &mut self,
        vertices: &[u8],
        index_parts: &[MeshIndices<'_>],
    ) -> Result<ResourceId, GraphicsError> {
        let (packed, parts) = pack_mesh_storage(vertices, index_parts)?;
        unsafe {
            let storage = required(
                objc::object_bytes(
                    self.surface.device,
                    c"newBufferWithBytes:length:options:",
                    packed.as_ptr().cast(),
                    packed.len(),
                    0,
                ),
                "Metal mesh storage buffer",
            )?;
            self.meshes.insert(MeshResource { storage, parts })
        }
    }

    pub(crate) fn create_texture(
        &mut self,
        width: u32,
        height: u32,
        levels: &[&[u8]],
        format: Rgba8TextureFormat,
    ) -> Result<ResourceId, GraphicsError> {
        unsafe {
            let pixel_format = match format {
                Rgba8TextureFormat::Srgb => PIXEL_FORMAT_RGBA8_UNORM_SRGB,
                Rgba8TextureFormat::Unorm => PIXEL_FORMAT_RGBA8_UNORM,
            };
            let descriptor = required(
                objc::object_three_usizes_bool(
                    objc::class(c"MTLTextureDescriptor"),
                    c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
                    pixel_format,
                    usize::try_from(width)
                        .map_err(|_| GraphicsError::new("texture width exceeds usize"))?,
                    usize::try_from(height)
                        .map_err(|_| GraphicsError::new("texture height exceeds usize"))?,
                    levels.len() > 1,
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
            for (level, texels) in levels.iter().enumerate() {
                let level_index = u32::try_from(level)
                    .map_err(|_| GraphicsError::new("mip chain length exceeds u32"))?;
                let level_width = usize::try_from(mip_extent(width, level_index))
                    .map_err(|_| GraphicsError::new("texture width exceeds usize"))?;
                let level_height = usize::try_from(mip_extent(height, level_index))
                    .map_err(|_| GraphicsError::new("texture height exceeds usize"))?;
                objc::void_region_usize_bytes_usize(
                    texture,
                    c"replaceRegion:mipmapLevel:withBytes:bytesPerRow:",
                    Region3 {
                        origin: Origin3 { x: 0, y: 0, z: 0 },
                        size: Size3 {
                            width: level_width,
                            height: level_height,
                            depth: 1,
                        },
                    },
                    level,
                    texels.as_ptr().cast(),
                    level_width * 4,
                );
            }
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
        config: &PostprocessPipelineConfig,
    ) -> Result<ResourceId, GraphicsError> {
        self.postprocess_pipelines
            .insert(create_postprocess_pipeline(
                self.surface.device,
                shader.payload(),
                config,
            )?)
    }

    pub(crate) fn create_material_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
        config: &MaterialPipelineConfig<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        self.material_pipelines.insert(create_material_pipeline(
            self.surface.device,
            shader.payload(),
            config,
            self.sample_count,
        )?)
    }

    pub(crate) fn create_shadow_map(&mut self, size: u32) -> Result<ResourceId, GraphicsError> {
        let extent = usize::try_from(size)
            .map_err(|_| GraphicsError::new("shadow map extent exceeds usize"))?;
        let texture = create_target_texture(
            self.surface.device,
            PIXEL_FORMAT_DEPTH32_FLOAT,
            extent,
            extent,
            1,
            TEXTURE_USAGE_RENDER_TARGET | TEXTURE_USAGE_SHADER_READ,
        )?;
        self.shadow_maps.insert(ShadowMapResource {
            texture,
            rendered: false,
        })
    }

    pub(crate) fn create_shadow_map_array(
        &mut self,
        size: u32,
        layers: u32,
    ) -> Result<ResourceId, GraphicsError> {
        let extent = usize::try_from(size)
            .map_err(|_| GraphicsError::new("shadow map array extent exceeds usize"))?;
        let layer_count = usize::try_from(layers)
            .map_err(|_| GraphicsError::new("shadow map array layer count exceeds usize"))?;
        let texture = unsafe {
            let descriptor = required(
                objc::object_three_usizes_bool(
                    objc::class(c"MTLTextureDescriptor"),
                    c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
                    PIXEL_FORMAT_DEPTH32_FLOAT,
                    extent,
                    extent,
                    false,
                ),
                "Metal shadow map array descriptor",
            )?;
            objc::void_usize(descriptor, c"setTextureType:", TEXTURE_TYPE_2D_ARRAY);
            objc::void_usize(descriptor, c"setArrayLength:", layer_count);
            objc::void_usize(descriptor, c"setStorageMode:", STORAGE_MODE_PRIVATE);
            objc::void_usize(
                descriptor,
                c"setUsage:",
                TEXTURE_USAGE_RENDER_TARGET | TEXTURE_USAGE_SHADER_READ,
            );
            required(
                objc::object_object(
                    self.surface.device,
                    c"newTextureWithDescriptor:",
                    descriptor,
                ),
                "Metal shadow map array texture",
            )?
        };
        self.shadow_map_arrays.insert(ShadowMapArrayResource {
            texture,
            rendered: false,
        })
    }

    pub(crate) fn create_shadow_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
        config: &ShadowPipelineConfig<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        self.shadow_pipelines.insert(create_shadow_pipeline(
            self.surface.device,
            shader.payload(),
            config,
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

    /// Creates the two-pass targets: offscreen scene storage at `scene_extent` — the
    /// render-scale-adjusted extent — while presentation stays at the surface extent carried
    /// by `info`.
    pub(crate) fn create_postprocess_targets(
        &mut self,
        info: SurfaceInfo,
        scene_extent: crate::SurfaceExtent,
    ) -> Result<ResourceId, GraphicsError> {
        self.reclaim_stale_targets();
        let width = usize::try_from(scene_extent.width())
            .map_err(|_| GraphicsError::new("target width exceeds usize"))?;
        let height = usize::try_from(scene_extent.height())
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
        self.encode_present(
            token,
            PreparedScene::Draws(draws),
            None,
            target,
            clear,
            DEPTH_CLEAR_FAR,
        )
    }

    pub(crate) fn draw_scene_postprocessed_and_present(
        &mut self,
        token: TexturedFrameToken,
        draws: &[TexturedSceneDraw<'_>],
        postprocess_pipeline: ResourceId,
        targets: ResourceId,
        uniform: &[u8],
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
            None,
            &[],
            postprocess_pipeline,
            target,
            uniform,
            clear,
            DEPTH_CLEAR_FAR,
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
        self.encode_present(
            token,
            PreparedScene::Instances,
            None,
            target,
            clear,
            DEPTH_CLEAR_FAR,
        )
    }

    pub(crate) fn draw_instanced_scene_postprocessed_and_present(
        &mut self,
        token: TexturedFrameToken,
        batches: &[TexturedInstanceBatch<'_>],
        postprocess_pipeline: ResourceId,
        targets: ResourceId,
        uniform: &[u8],
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
            None,
            &[],
            postprocess_pipeline,
            target,
            uniform,
            clear,
            DEPTH_CLEAR_FAR,
        )
    }

    /// Rejects sampling a shadow map that neither an earlier frame nor this submission's shadow
    /// pass has rendered. This runs before the frame token is consumed so the rejection cannot
    /// strand an acquired drawable.
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
        self.prepare_material_scene(records, &[], shadow)?;
        self.encode_present(
            token,
            PreparedScene::Materials(records),
            shadow,
            target,
            clear,
            depth_clear,
        )
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
        uniform: &[u8],
        clear: ClearColor,
        depth_clear: f32,
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
        self.prepare_material_scene(records, overlay, shadow)?;
        self.encode_postprocessed_present(
            token,
            PreparedScene::Materials(records),
            shadow,
            overlay,
            postprocess_pipeline,
            target,
            uniform,
            clear,
            depth_clear,
        )
    }

    /// Checks handles and stages per-record data for the scene records, any overlay records,
    /// and any shadow records in that fixed order, so encoding recomputes the same uniform
    /// slots and storage offsets.
    #[allow(clippy::too_many_lines)]
    fn prepare_material_scene(
        &mut self,
        records: &[MaterialRecord<'_>],
        overlay: &[MaterialRecord<'_>],
        shadow: Option<&ShadowPrepass<'_>>,
    ) -> Result<(), GraphicsError> {
        if let Some(shadow) = shadow {
            match shadow {
                ShadowPrepass::Single(pass) => {
                    self.shadow_maps.get(pass.map.id())?;
                }
                ShadowPrepass::Cascaded(pass) => {
                    self.shadow_map_arrays.get(pass.map.id())?;
                }
            }
            for record in shadow.records() {
                self.meshes.get(record.geometry.mesh().id())?;
                self.shadow_pipelines.get(record.pipeline.id())?;
                for texture in record.textures {
                    self.textures.get(texture.id())?;
                }
            }
        }
        for record in records.iter().chain(overlay) {
            if let Some(mesh) = record.geometry.uploaded_mesh() {
                self.meshes.get(mesh.id())?;
            }
            self.material_pipelines.get(record.pipeline.id())?;
            for texture in record.textures {
                self.textures.get(texture.id())?;
            }
            match record.shadow_map {
                Some(ShadowSource::Map(map)) => {
                    self.shadow_maps.get(map.id())?;
                }
                Some(ShadowSource::Array(array)) => {
                    self.shadow_map_arrays.get(array.id())?;
                }
                None => {}
            }
        }
        let shadow_records = shadow.map_or(0, |shadow| shadow.records().count());
        let uniform_slots = records
            .len()
            .checked_add(overlay.len())
            .and_then(|slots| slots.checked_add(shadow_records))
            .ok_or_else(|| GraphicsError::new("Metal material uniform capacity overflow"))?;
        if uniform_slots > self.uniform_capacity {
            let capacity = uniform_slots
                .checked_next_power_of_two()
                .ok_or_else(|| GraphicsError::new("Metal material uniform capacity overflow"))?;
            let bytes = capacity
                .checked_mul(DRAW_UNIFORM_STRIDE)
                .ok_or_else(|| GraphicsError::new("Metal material uniform storage is too large"))?;
            let replacement = unsafe {
                required(
                    objc::object_two_usizes(
                        self.surface.device,
                        c"newBufferWithLength:options:",
                        bytes,
                        0,
                    ),
                    "Metal material uniform buffer",
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
                    contents.cast::<u8>().add(index * DRAW_UNIFORM_STRIDE),
                    uniform.len(),
                );
            }
        }
        let storage_bytes = records
            .iter()
            .chain(overlay)
            .map(|record| record.storage)
            .chain(
                shadow
                    .into_iter()
                    .flat_map(|shadow| shadow.records().map(|record| record.storage)),
            )
            .try_fold(0_usize, |total, storage| {
                storage
                    .len()
                    .checked_next_multiple_of(STORAGE_OFFSET_ALIGNMENT)
                    .and_then(|aligned| total.checked_add(aligned))
                    .ok_or_else(|| GraphicsError::new("Metal record storage offsets overflow"))
            })?;
        if storage_bytes > self.storage_capacity {
            let capacity = storage_bytes
                .checked_next_power_of_two()
                .ok_or_else(|| GraphicsError::new("Metal record storage capacity overflow"))?;
            let replacement = unsafe {
                required(
                    objc::object_two_usizes(
                        self.surface.device,
                        c"newBufferWithLength:options:",
                        capacity,
                        0,
                    ),
                    "Metal record storage buffer",
                )?
            };
            unsafe { objc::void(self.storage, c"release") };
            self.storage = replacement;
            self.storage_capacity = capacity;
        }
        if storage_bytes > 0 {
            unsafe {
                let contents = objc::pointer_value(self.storage, c"contents");
                if contents.is_null() {
                    return Err(GraphicsError::new(
                        "Metal record storage buffer has no CPU address",
                    ));
                }
                let sources = records
                    .iter()
                    .chain(overlay)
                    .map(|record| record.storage)
                    .chain(
                        shadow
                            .into_iter()
                            .flat_map(|shadow| shadow.records().map(|record| record.storage)),
                    );
                let mut offset = 0_usize;
                for storage in sources {
                    ptr::copy_nonoverlapping(
                        storage.as_ptr(),
                        contents.cast::<u8>().add(offset),
                        storage.len(),
                    );
                    offset += storage.len().next_multiple_of(STORAGE_OFFSET_ALIGNMENT);
                }
            }
        }
        self.stage_transient_geometry(records, overlay)?;
        self.stage_record_instances(records, overlay, shadow)?;
        match shadow {
            Some(ShadowPrepass::Single(pass)) => {
                let index = self.shadow_maps.index_of(pass.map.id())?;
                self.shadow_maps[index].rendered = true;
            }
            Some(ShadowPrepass::Cascaded(pass)) => {
                let index = self.shadow_map_arrays.index_of(pass.map.id())?;
                self.shadow_map_arrays[index].rendered = true;
            }
            None => {}
        }
        Ok(())
    }

    /// Copies every transient-geometry record supply into the frame's shared geometry region:
    /// per record, aligned vertex bytes followed by aligned index bytes, scene records then
    /// overlay records in record order, so encoding recomputes the same offsets.
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
                GeometrySource::Mesh(_) | GeometrySource::MeshPart(_) => None,
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
                    .ok_or_else(|| GraphicsError::new("Metal transient geometry offsets overflow"))
            })?;
        if geometry_bytes > self.transient_capacity {
            let capacity = geometry_bytes
                .checked_next_power_of_two()
                .ok_or_else(|| GraphicsError::new("Metal transient geometry capacity overflow"))?;
            let replacement = unsafe {
                required(
                    objc::object_two_usizes(
                        self.surface.device,
                        c"newBufferWithLength:options:",
                        capacity,
                        0,
                    ),
                    "Metal transient geometry buffer",
                )?
            };
            unsafe { objc::void(self.transient_geometry, c"release") };
            self.transient_geometry = replacement;
            self.transient_capacity = capacity;
        }
        if geometry_bytes > 0 {
            unsafe {
                let contents = objc::pointer_value(self.transient_geometry, c"contents");
                if contents.is_null() {
                    return Err(GraphicsError::new(
                        "Metal transient geometry buffer has no CPU address",
                    ));
                }
                let mut offset = 0_usize;
                for record in records.iter().chain(overlay) {
                    let GeometrySource::Transient(geometry) = record.geometry else {
                        continue;
                    };
                    ptr::copy_nonoverlapping(
                        geometry.vertices.as_ptr(),
                        contents.cast::<u8>().add(offset),
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
                        contents.cast::<u8>().add(offset),
                        geometry.indices.byte_len(),
                    );
                    offset += geometry
                        .indices
                        .byte_len()
                        .next_multiple_of(STORAGE_OFFSET_ALIGNMENT);
                }
            }
        }
        Ok(())
    }

    /// Copies every record's instance supply into the frame's shared instance region: scene
    /// records, then overlay records, then shadow records in record order at aligned offsets,
    /// so encoding recomputes the same offsets.
    fn stage_record_instances(
        &mut self,
        records: &[MaterialRecord<'_>],
        overlay: &[MaterialRecord<'_>],
        shadow: Option<&ShadowPrepass<'_>>,
    ) -> Result<(), GraphicsError> {
        let supplies = || {
            records
                .iter()
                .chain(overlay)
                .map(|record| record.instances)
                .chain(
                    shadow
                        .into_iter()
                        .flat_map(|shadow| shadow.records().map(|record| record.instances)),
                )
        };
        let instance_bytes = supplies().try_fold(0_usize, |total, instances| {
            instances
                .len()
                .checked_next_multiple_of(STORAGE_OFFSET_ALIGNMENT)
                .and_then(|aligned| total.checked_add(aligned))
                .ok_or_else(|| GraphicsError::new("Metal record instance offsets overflow"))
        })?;
        if instance_bytes > self.record_instance_capacity {
            let capacity = instance_bytes
                .checked_next_power_of_two()
                .ok_or_else(|| GraphicsError::new("Metal record instance capacity overflow"))?;
            let replacement = unsafe {
                required(
                    objc::object_two_usizes(
                        self.surface.device,
                        c"newBufferWithLength:options:",
                        capacity,
                        0,
                    ),
                    "Metal record instance buffer",
                )?
            };
            unsafe { objc::void(self.record_instances, c"release") };
            self.record_instances = replacement;
            self.record_instance_capacity = capacity;
        }
        if supplies().all(<[u8]>::is_empty) {
            return Ok(());
        }
        unsafe {
            let contents = objc::pointer_value(self.record_instances, c"contents");
            if contents.is_null() {
                return Err(GraphicsError::new(
                    "Metal record instance buffer has no CPU address",
                ));
            }
            let mut offset = 0_usize;
            for instances in supplies() {
                ptr::copy_nonoverlapping(
                    instances.as_ptr(),
                    contents.cast::<u8>().add(offset),
                    instances.len(),
                );
                offset += instances.len().next_multiple_of(STORAGE_OFFSET_ALIGNMENT);
            }
        }
        Ok(())
    }

    /// Encodes the depth-only shadow work — one encoder for a single map, or one encoder per
    /// cascade layer — on the frame's command buffer, ordered before the scene encoder that
    /// samples the result.
    unsafe fn encode_shadow_prepass(
        &self,
        command: Object,
        shadow: &ShadowPrepass<'_>,
        uniform_base: usize,
        storage_base: usize,
        instance_base: usize,
    ) -> Result<(), GraphicsError> {
        let mut uniform_index = uniform_base;
        let mut storage_offset = storage_base;
        let mut instance_offset = instance_base;
        match shadow {
            ShadowPrepass::Single(pass) => {
                let map = &self.shadow_maps[self.shadow_maps.index_of(pass.map.id())?];
                unsafe {
                    self.encode_shadow_layer(
                        command,
                        map.texture,
                        None,
                        pass.records,
                        &mut uniform_index,
                        &mut storage_offset,
                        &mut instance_offset,
                    )
                }
            }
            ShadowPrepass::Cascaded(pass) => {
                let array =
                    &self.shadow_map_arrays[self.shadow_map_arrays.index_of(pass.map.id())?];
                for (layer, records) in pass.cascades.iter().enumerate() {
                    unsafe {
                        self.encode_shadow_layer(
                            command,
                            array.texture,
                            Some(layer),
                            records,
                            &mut uniform_index,
                            &mut storage_offset,
                            &mut instance_offset,
                        )?;
                    }
                }
                Ok(())
            }
        }
    }

    /// Encodes one depth-only pass into a shadow target — the whole map, or one array layer —
    /// clearing it to the far plane before its records.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    unsafe fn encode_shadow_layer(
        &self,
        command: Object,
        texture: Object,
        layer: Option<usize>,
        records: &[ShadowRecord<'_>],
        uniform_index: &mut usize,
        storage_offset: &mut usize,
        instance_offset: &mut usize,
    ) -> Result<(), GraphicsError> {
        unsafe {
            let pass = required(
                objc::object(
                    objc::class(c"MTLRenderPassDescriptor"),
                    c"renderPassDescriptor",
                ),
                "Metal shadow render-pass descriptor",
            )?;
            let depth = required(
                objc::object(pass, c"depthAttachment"),
                "shadow depth attachment",
            )?;
            objc::void_object(depth, c"setTexture:", texture);
            if let Some(layer) = layer {
                objc::void_usize(depth, c"setSlice:", layer);
            }
            objc::void_usize(depth, c"setLoadAction:", LOAD_ACTION_CLEAR);
            objc::void_usize(depth, c"setStoreAction:", STORE_ACTION_STORE);
            objc::void_f64(depth, c"setClearDepth:", 1.0);
            let encoder = required(
                objc::object_object(command, c"renderCommandEncoderWithDescriptor:", pass),
                "Metal shadow render encoder",
            )?;
            for record in records {
                let pipeline =
                    &self.shadow_pipelines[self.shadow_pipelines.index_of(record.pipeline.id())?];
                let mesh = &self.meshes[self.meshes.index_of(record.geometry.mesh().id())?];
                let part = &mesh.parts[usize::try_from(record.geometry.part_index())
                    .expect("validated mesh part index fits usize")];
                objc::void_object(encoder, c"setRenderPipelineState:", pipeline.pipeline);
                objc::void_object(encoder, c"setDepthStencilState:", pipeline.depth_state);
                if let Some((binding, _)) = pipeline.uniform {
                    let slot = usize::try_from(binding).expect("validated slot fits usize");
                    objc::void_object_two_usizes(
                        encoder,
                        c"setVertexBuffer:offset:atIndex:",
                        self.uniform,
                        *uniform_index * DRAW_UNIFORM_STRIDE,
                        slot,
                    );
                    if pipeline.fragment {
                        objc::void_object_two_usizes(
                            encoder,
                            c"setFragmentBuffer:offset:atIndex:",
                            self.uniform,
                            *uniform_index * DRAW_UNIFORM_STRIDE,
                            slot,
                        );
                    }
                }
                if let Some((binding, _)) = pipeline.storage {
                    let slot = usize::try_from(binding).expect("validated slot fits usize");
                    objc::void_object_two_usizes(
                        encoder,
                        c"setVertexBuffer:offset:atIndex:",
                        self.storage,
                        *storage_offset,
                        slot,
                    );
                    if pipeline.fragment {
                        objc::void_object_two_usizes(
                            encoder,
                            c"setFragmentBuffer:offset:atIndex:",
                            self.storage,
                            *storage_offset,
                            slot,
                        );
                    }
                }
                for (texture, &binding) in record.textures.iter().zip(&pipeline.texture_bindings) {
                    let resource = &self.textures[self.textures.index_of(texture.id())?];
                    let slot = usize::try_from(binding).expect("validated slot fits usize");
                    objc::void_object_usize(
                        encoder,
                        c"setFragmentTexture:atIndex:",
                        resource.texture,
                        slot,
                    );
                }
                for &(binding, sampler) in &pipeline.samplers {
                    let slot = usize::try_from(binding).expect("validated slot fits usize");
                    objc::void_object_usize(
                        encoder,
                        c"setFragmentSamplerState:atIndex:",
                        sampler,
                        slot,
                    );
                }
                *uniform_index += 1;
                *storage_offset += record
                    .storage
                    .len()
                    .next_multiple_of(STORAGE_OFFSET_ALIGNMENT);
                objc::void_object_two_usizes(
                    encoder,
                    c"setVertexBuffer:offset:atIndex:",
                    mesh.storage,
                    0,
                    MATERIAL_VERTEX_BUFFER_INDEX,
                );
                if let Some(instance_count) =
                    record.instances.len().checked_div(pipeline.instance_stride)
                {
                    objc::void_object_two_usizes(
                        encoder,
                        c"setVertexBuffer:offset:atIndex:",
                        self.record_instances,
                        *instance_offset,
                        MATERIAL_INSTANCE_BUFFER_INDEX,
                    );
                    objc::void_three_usizes_object_two_usizes(
                        encoder,
                        c"drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:",
                        PRIMITIVE_TYPE_TRIANGLE,
                        usize::try_from(part.index_count).expect("u32 index count fits usize"),
                        part.index_type,
                        mesh.storage,
                        part.index_offset,
                        instance_count,
                    );
                } else {
                    objc::void_two_usizes_object_usize_object_usize(
                        encoder,
                        c"drawIndexedPrimitives:indexType:indexBuffer:indexBufferOffset:indirectBuffer:indirectBufferOffset:",
                        PRIMITIVE_TYPE_TRIANGLE,
                        part.index_type,
                        mesh.storage,
                        part.index_offset,
                        mesh.storage,
                        part.indirect_offset,
                    );
                }
                *instance_offset += record
                    .instances
                    .len()
                    .next_multiple_of(STORAGE_OFFSET_ALIGNMENT);
            }
            objc::void(encoder, c"endEncoding");
        }
        Ok(())
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
            ResourceKind::MaterialPipeline => {
                release_material_pipeline(self.material_pipelines.remove(request.id)?);
            }
            ResourceKind::ShadowMap => release_shadow_map(self.shadow_maps.remove(request.id)?),
            ResourceKind::ShadowMapArray => {
                release_shadow_map_array(self.shadow_map_arrays.remove(request.id)?);
            }
            ResourceKind::ShadowPipeline => {
                release_shadow_pipeline(self.shadow_pipelines.remove(request.id)?);
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
            ResourceKind::MaterialPipeline => self
                .material_pipelines
                .remove_if_live(request.id)
                .map(release_material_pipeline),
            ResourceKind::ShadowMap => self
                .shadow_maps
                .remove_if_live(request.id)
                .map(release_shadow_map),
            ResourceKind::ShadowMapArray => self
                .shadow_map_arrays
                .remove_if_live(request.id)
                .map(release_shadow_map_array),
            ResourceKind::ShadowPipeline => self
                .shadow_pipelines
                .remove_if_live(request.id)
                .map(release_shadow_pipeline),
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
        shadow: Option<&ShadowPrepass<'_>>,
        target: usize,
        clear: ClearColor,
        depth_clear: f32,
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
            objc::void_f64(depth, c"setClearDepth:", f64::from(depth_clear));
            let command = required(
                objc::object(self.surface.queue, c"commandBuffer"),
                "Metal cube command buffer",
            )?;
            if let (Some(shadow), PreparedScene::Materials(records)) = (shadow, scene) {
                self.encode_shadow_prepass(
                    command,
                    shadow,
                    records.len(),
                    material_storage_len(records),
                    material_instances_len(records),
                )?;
            }
            let encoder = required(
                objc::object_object(command, c"renderCommandEncoderWithDescriptor:", pass),
                "Metal cube render encoder",
            )?;
            self.encode_prepared_scene(encoder, scene)?;
            objc::void(encoder, c"endEncoding");
            self.surface.present_commit(command, drawable);
            token.0.drawable = ptr::null_mut();
        }
        Ok(FrameDisposition::Presented(token.info().generation()))
    }

    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    fn encode_postprocessed_present(
        &mut self,
        mut token: TexturedFrameToken,
        scene: PreparedScene<'_>,
        shadow: Option<&ShadowPrepass<'_>>,
        overlay: &[MaterialRecord<'_>],
        postprocess_pipeline: usize,
        target: usize,
        uniform: &[u8],
        clear: ClearColor,
        depth_clear: f32,
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
            objc::void_f64(scene_depth, c"setClearDepth:", f64::from(depth_clear));

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
            if let (Some(shadow), PreparedScene::Materials(records)) = (shadow, scene) {
                self.encode_shadow_prepass(
                    command,
                    shadow,
                    records.len() + overlay.len(),
                    material_storage_len(records) + material_storage_len(overlay),
                    material_instances_len(records) + material_instances_len(overlay),
                )?;
            }
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
            if postprocess.uniform_size != 0 {
                objc::void_bytes_usize_usize(
                    post_encoder,
                    c"setFragmentBytes:length:atIndex:",
                    uniform.as_ptr().cast(),
                    uniform.len(),
                    0,
                );
            }
            objc::void_three_usizes(
                post_encoder,
                c"drawPrimitives:vertexStart:vertexCount:",
                PRIMITIVE_TYPE_TRIANGLE,
                0,
                3,
            );
            if let (false, PreparedScene::Materials(records)) = (overlay.is_empty(), scene) {
                self.encode_material_records(
                    post_encoder,
                    overlay,
                    records.len(),
                    material_storage_len(records),
                    transient_geometry_len(records),
                    material_instances_len(records),
                    true,
                )?;
            }
            objc::void(post_encoder, c"endEncoding");
            self.surface.present_commit(command, drawable);
            token.0.drawable = ptr::null_mut();
        }
        Ok(FrameDisposition::Presented(token.info().generation()))
    }

    #[allow(clippy::too_many_lines)]
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
                        let part = &mesh.parts[0];
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
                            mesh.storage,
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
                            part.index_type,
                            mesh.storage,
                            part.index_offset,
                            mesh.storage,
                            part.indirect_offset,
                        );
                    }
                }
                PreparedScene::Materials(records) => {
                    self.encode_material_records(encoder, records, 0, 0, 0, 0, false)?;
                }
                PreparedScene::Instances => {
                    for batch in &self.resolved_instance_batches {
                        let pipeline = &self.instanced_pipelines[batch.pipeline];
                        let mesh = &self.meshes[batch.mesh];
                        let part = &mesh.parts[0];
                        let texture = &self.textures[batch.texture];
                        objc::void_object(encoder, c"setRenderPipelineState:", pipeline.pipeline);
                        objc::void_object(encoder, c"setDepthStencilState:", pipeline.depth_state);
                        objc::void_object_two_usizes(
                            encoder,
                            c"setVertexBuffer:offset:atIndex:",
                            mesh.storage,
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
                            usize::try_from(part.index_count).expect("u32 index count fits usize"),
                            part.index_type,
                            mesh.storage,
                            part.index_offset,
                            batch.instance_count,
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Encodes one non-empty material record sequence on an active render encoder.
    ///
    /// The bases select where this sequence's uniform slots, storage bytes, and transient
    /// geometry bytes begin inside the frame's shared regions, matching the staging order of
    /// scene records, then overlay records, then shadow records. Overlay encoding binds each
    /// pipeline's single-sample no-depth variant and sets no depth-stencil state, because the
    /// presentable pass carries no depth attachment.
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    unsafe fn encode_material_records(
        &self,
        encoder: Object,
        records: &[MaterialRecord<'_>],
        uniform_base: usize,
        storage_base: usize,
        transient_base: usize,
        instance_base: usize,
        overlay: bool,
    ) -> Result<(), GraphicsError> {
        unsafe {
            let mut storage_offset = storage_base;
            let mut transient_offset = transient_base;
            let mut instance_offset = instance_base;
            for (index, record) in records.iter().enumerate() {
                let pipeline = &self.material_pipelines
                    [self.material_pipelines.index_of(record.pipeline.id())?];
                let geometry = match record.geometry {
                    GeometrySource::Mesh(mesh) => ResolvedGeometry::Mesh {
                        mesh: self.meshes.index_of(mesh.id())?,
                        part: 0,
                    },
                    GeometrySource::MeshPart(part) => ResolvedGeometry::Mesh {
                        mesh: self.meshes.index_of(part.mesh().id())?,
                        part: part.index(),
                    },
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
                            vertex_offset,
                            index_offset,
                            index_count: supply.indices.len(),
                            index_type: match supply.indices {
                                MeshIndices::U16(_) => INDEX_TYPE_UINT16,
                                MeshIndices::U32(_) => INDEX_TYPE_UINT32,
                            },
                        }
                    }
                };
                if overlay {
                    if pipeline.overlay_pipeline.is_null() {
                        return Err(GraphicsError::new(
                            "Metal material pipeline lacks the overlay variant its record needs",
                        ));
                    }
                    objc::void_object(
                        encoder,
                        c"setRenderPipelineState:",
                        pipeline.overlay_pipeline,
                    );
                } else {
                    objc::void_object(encoder, c"setRenderPipelineState:", pipeline.pipeline);
                    objc::void_object(encoder, c"setDepthStencilState:", pipeline.depth_state);
                }
                if let Some((binding, _)) = pipeline.uniform {
                    let slot = usize::try_from(binding).expect("validated slot fits usize");
                    objc::void_object_two_usizes(
                        encoder,
                        c"setVertexBuffer:offset:atIndex:",
                        self.uniform,
                        (uniform_base + index) * DRAW_UNIFORM_STRIDE,
                        slot,
                    );
                    objc::void_object_two_usizes(
                        encoder,
                        c"setFragmentBuffer:offset:atIndex:",
                        self.uniform,
                        (uniform_base + index) * DRAW_UNIFORM_STRIDE,
                        slot,
                    );
                }
                if let Some((binding, _)) = pipeline.storage {
                    let slot = usize::try_from(binding).expect("validated slot fits usize");
                    objc::void_object_two_usizes(
                        encoder,
                        c"setVertexBuffer:offset:atIndex:",
                        self.storage,
                        storage_offset,
                        slot,
                    );
                    objc::void_object_two_usizes(
                        encoder,
                        c"setFragmentBuffer:offset:atIndex:",
                        self.storage,
                        storage_offset,
                        slot,
                    );
                }
                storage_offset += record
                    .storage
                    .len()
                    .next_multiple_of(STORAGE_OFFSET_ALIGNMENT);
                match geometry {
                    ResolvedGeometry::Mesh { mesh, .. } => objc::void_object_two_usizes(
                        encoder,
                        c"setVertexBuffer:offset:atIndex:",
                        self.meshes[mesh].storage,
                        0,
                        MATERIAL_VERTEX_BUFFER_INDEX,
                    ),
                    ResolvedGeometry::Transient { vertex_offset, .. } => {
                        objc::void_object_two_usizes(
                            encoder,
                            c"setVertexBuffer:offset:atIndex:",
                            self.transient_geometry,
                            vertex_offset,
                            MATERIAL_VERTEX_BUFFER_INDEX,
                        );
                    }
                }
                for (texture, &binding) in record.textures.iter().zip(&pipeline.texture_bindings) {
                    let resource = &self.textures[self.textures.index_of(texture.id())?];
                    let slot = usize::try_from(binding).expect("validated slot fits usize");
                    objc::void_object_usize(
                        encoder,
                        c"setVertexTexture:atIndex:",
                        resource.texture,
                        slot,
                    );
                    objc::void_object_usize(
                        encoder,
                        c"setFragmentTexture:atIndex:",
                        resource.texture,
                        slot,
                    );
                }
                for &(binding, sampler) in &pipeline.samplers {
                    let slot = usize::try_from(binding).expect("validated slot fits usize");
                    objc::void_object_usize(
                        encoder,
                        c"setVertexSamplerState:atIndex:",
                        sampler,
                        slot,
                    );
                    objc::void_object_usize(
                        encoder,
                        c"setFragmentSamplerState:atIndex:",
                        sampler,
                        slot,
                    );
                }
                let shadow_binding = match record.shadow_map {
                    Some(ShadowSource::Map(map)) => pipeline
                        .depth_texture_binding
                        .map(|binding| {
                            self.shadow_maps
                                .index_of(map.id())
                                .map(|index| (self.shadow_maps[index].texture, binding))
                        })
                        .transpose()?,
                    Some(ShadowSource::Array(array)) => pipeline
                        .depth_texture_array_binding
                        .map(|binding| {
                            self.shadow_map_arrays
                                .index_of(array.id())
                                .map(|index| (self.shadow_map_arrays[index].texture, binding))
                        })
                        .transpose()?,
                    None => None,
                };
                if let Some((texture, binding)) = shadow_binding {
                    let slot = usize::try_from(binding).expect("validated slot fits usize");
                    objc::void_object_usize(encoder, c"setVertexTexture:atIndex:", texture, slot);
                    objc::void_object_usize(encoder, c"setFragmentTexture:atIndex:", texture, slot);
                }
                if let Some((binding, sampler)) = pipeline.comparison_sampler {
                    let slot = usize::try_from(binding).expect("validated slot fits usize");
                    objc::void_object_usize(
                        encoder,
                        c"setVertexSamplerState:atIndex:",
                        sampler,
                        slot,
                    );
                    objc::void_object_usize(
                        encoder,
                        c"setFragmentSamplerState:atIndex:",
                        sampler,
                        slot,
                    );
                }
                let instance_count = if let Some(count) =
                    record.instances.len().checked_div(pipeline.instance_stride)
                {
                    objc::void_object_two_usizes(
                        encoder,
                        c"setVertexBuffer:offset:atIndex:",
                        self.record_instances,
                        instance_offset,
                        MATERIAL_INSTANCE_BUFFER_INDEX,
                    );
                    count
                } else {
                    1
                };
                instance_offset += record
                    .instances
                    .len()
                    .next_multiple_of(STORAGE_OFFSET_ALIGNMENT);
                match geometry {
                    ResolvedGeometry::Mesh { mesh, part } => {
                        let mesh = &self.meshes[mesh];
                        let part = &mesh.parts[part];
                        if pipeline.instance_stride > 0 {
                            objc::void_three_usizes_object_two_usizes(
                                encoder,
                                c"drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:",
                                PRIMITIVE_TYPE_TRIANGLE,
                                usize::try_from(part.index_count)
                                    .expect("u32 index count fits usize"),
                                part.index_type,
                                mesh.storage,
                                part.index_offset,
                                instance_count,
                            );
                        } else {
                            objc::void_two_usizes_object_usize_object_usize(
                                encoder,
                                c"drawIndexedPrimitives:indexType:indexBuffer:indexBufferOffset:indirectBuffer:indirectBufferOffset:",
                                PRIMITIVE_TYPE_TRIANGLE,
                                part.index_type,
                                mesh.storage,
                                part.index_offset,
                                mesh.storage,
                                part.indirect_offset,
                            );
                        }
                    }
                    ResolvedGeometry::Transient {
                        index_offset,
                        index_count,
                        index_type,
                        ..
                    } => {
                        objc::void_three_usizes_object_two_usizes(
                            encoder,
                            c"drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:",
                            PRIMITIVE_TYPE_TRIANGLE,
                            index_count,
                            index_type,
                            self.transient_geometry,
                            index_offset,
                            instance_count,
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
        for pipeline in self.material_pipelines.take_all() {
            release_material_pipeline(pipeline);
        }
        for pipeline in self.shadow_pipelines.take_all() {
            release_shadow_pipeline(pipeline);
        }
        for map in self.shadow_maps.take_all() {
            release_shadow_map(map);
        }
        for array in self.shadow_map_arrays.take_all() {
            release_shadow_map_array(array);
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
            if !self.storage.is_null() {
                objc::void(self.storage, c"release");
                self.storage = ptr::null_mut();
            }
            if !self.transient_geometry.is_null() {
                objc::void(self.transient_geometry, c"release");
                self.transient_geometry = ptr::null_mut();
            }
            if !self.instance_transforms.is_null() {
                objc::void(self.instance_transforms, c"release");
                self.instance_transforms = ptr::null_mut();
            }
            if !self.record_instances.is_null() {
                objc::void(self.record_instances, c"release");
                self.record_instances = ptr::null_mut();
            }
        }

        // `shutdown` moves the surface out and deliberately suppresses this
        // session's destructor. Release the now-empty arenas' allocations here
        // so that path does not retain their backing storage.
        self.pipelines = Arena::new("textured pipeline");
        self.instanced_pipelines = Arena::new("instanced textured pipeline");
        self.material_pipelines = Arena::new("material pipeline");
        self.shadow_maps = Arena::new("shadow map");
        self.shadow_map_arrays = Arena::new("shadow map array");
        self.shadow_pipelines = Arena::new("shadow pipeline");
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
    let MeshResource { storage, parts: _ } = mesh;
    unsafe {
        objc::void(storage, c"release");
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
    let PostprocessPipelineResource {
        pipeline,
        sampler,
        uniform_size: _,
    } = pipeline;
    unsafe {
        objc::void(sampler, c"release");
        objc::void(pipeline, c"release");
    }
}

#[allow(clippy::needless_pass_by_value)]
fn release_material_pipeline(pipeline: MaterialPipelineResource) {
    unsafe {
        for &(_, sampler) in pipeline
            .samplers
            .iter()
            .chain(pipeline.comparison_sampler.iter())
        {
            objc::void(sampler, c"release");
        }
        objc::void(pipeline.depth_state, c"release");
        objc::void(pipeline.pipeline, c"release");
        if !pipeline.overlay_pipeline.is_null() {
            objc::void(pipeline.overlay_pipeline, c"release");
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn release_shadow_map(map: ShadowMapResource) {
    let ShadowMapResource {
        texture,
        rendered: _,
    } = map;
    unsafe {
        objc::void(texture, c"release");
    }
}

#[allow(clippy::needless_pass_by_value)]
fn release_shadow_map_array(array: ShadowMapArrayResource) {
    let ShadowMapArrayResource {
        texture,
        rendered: _,
    } = array;
    unsafe {
        objc::void(texture, c"release");
    }
}

#[allow(clippy::needless_pass_by_value)]
fn release_shadow_pipeline(pipeline: ShadowPipelineResource) {
    let ShadowPipelineResource {
        pipeline,
        depth_state,
        uniform: _,
        storage: _,
        fragment: _,
        texture_bindings: _,
        samplers,
        instance_stride: _,
    } = pipeline;
    unsafe {
        for (_, sampler) in samplers {
            objc::void(sampler, c"release");
        }
        objc::void(depth_state, c"release");
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

const fn material_vertex_format(format: VertexFormat) -> usize {
    match format {
        VertexFormat::Float32 => VERTEX_FORMAT_FLOAT,
        VertexFormat::Float32x2 => VERTEX_FORMAT_FLOAT2,
        VertexFormat::Float32x3 => VERTEX_FORMAT_FLOAT3,
        VertexFormat::Float32x4 => VERTEX_FORMAT_FLOAT4,
        VertexFormat::Uint32 => VERTEX_FORMAT_UINT,
        VertexFormat::Uint32x2 => VERTEX_FORMAT_UINT2,
        VertexFormat::Uint32x3 => VERTEX_FORMAT_UINT3,
        VertexFormat::Uint32x4 => VERTEX_FORMAT_UINT4,
        VertexFormat::Sint32 => VERTEX_FORMAT_INT,
        VertexFormat::Sint32x2 => VERTEX_FORMAT_INT2,
        VertexFormat::Sint32x3 => VERTEX_FORMAT_INT3,
        VertexFormat::Sint32x4 => VERTEX_FORMAT_INT4,
    }
}

#[allow(clippy::too_many_lines)]
fn create_material_pipeline(
    device: Object,
    bytes: &[u8],
    config: &MaterialPipelineConfig<'_>,
    sample_count: usize,
) -> Result<MaterialPipelineResource, GraphicsError> {
    let vertex_name = CString::new(config.vertex_entry)
        .map_err(|_| GraphicsError::new("material vertex entry point name contains a NUL byte"))?;
    let fragment_name = CString::new(config.fragment_entry).map_err(|_| {
        GraphicsError::new("material fragment entry point name contains a NUL byte")
    })?;
    unsafe {
        let data = required(
            dispatch_data_create(
                bytes.as_ptr().cast(),
                bytes.len(),
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            "Metal material library data",
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
                "loading material metallib failed: {}",
                objc::description(library_error)
            )));
        }
        let vertex = required(
            objc::object_object(
                library,
                c"newFunctionWithName:",
                objc::ns_string(&vertex_name),
            ),
            "Metal material vertex function",
        )?;
        let fragment = required(
            objc::object_object(
                library,
                c"newFunctionWithName:",
                objc::ns_string(&fragment_name),
            ),
            "Metal material fragment function",
        )?;
        let descriptor = required(
            objc::object(objc::class(c"MTLRenderPipelineDescriptor"), c"new"),
            "Metal material pipeline descriptor",
        )?;
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
            "material pipeline colors",
        )?;
        let color = required(
            objc::object_usize(colors, c"objectAtIndexedSubscript:", 0),
            "material pipeline color zero",
        )?;
        objc::void_usize(color, c"setPixelFormat:", PIXEL_FORMAT_BGRA8_UNORM_SRGB);
        match config.blend {
            BlendMode::Opaque => {}
            BlendMode::Cutout => {
                objc::void_bool(descriptor, c"setAlphaToCoverageEnabled:", true);
            }
            BlendMode::PremultipliedTranslucent => {
                objc::void_bool(color, c"setBlendingEnabled:", true);
                objc::void_usize(color, c"setSourceRGBBlendFactor:", BLEND_FACTOR_ONE);
                objc::void_usize(
                    color,
                    c"setDestinationRGBBlendFactor:",
                    BLEND_FACTOR_ONE_MINUS_SOURCE_ALPHA,
                );
                objc::void_usize(color, c"setSourceAlphaBlendFactor:", BLEND_FACTOR_ONE);
                objc::void_usize(
                    color,
                    c"setDestinationAlphaBlendFactor:",
                    BLEND_FACTOR_ONE_MINUS_SOURCE_ALPHA,
                );
            }
        }

        let vertex_descriptor = required(
            objc::object(objc::class(c"MTLVertexDescriptor"), c"vertexDescriptor"),
            "Metal material vertex descriptor",
        )?;
        let attributes = required(
            objc::object(vertex_descriptor, c"attributes"),
            "material vertex attributes",
        )?;
        for input in config.attributes {
            let attribute = required(
                objc::object_usize(
                    attributes,
                    c"objectAtIndexedSubscript:",
                    usize::try_from(input.location).expect("validated location fits usize"),
                ),
                "Metal material vertex attribute",
            )?;
            objc::void_usize(
                attribute,
                c"setFormat:",
                material_vertex_format(input.format),
            );
            objc::void_usize(
                attribute,
                c"setOffset:",
                usize::try_from(input.offset).expect("validated offset fits usize"),
            );
            objc::void_usize(attribute, c"setBufferIndex:", MATERIAL_VERTEX_BUFFER_INDEX);
        }
        let layouts = required(
            objc::object(vertex_descriptor, c"layouts"),
            "material vertex layouts",
        )?;
        let layout = required(
            objc::object_usize(
                layouts,
                c"objectAtIndexedSubscript:",
                MATERIAL_VERTEX_BUFFER_INDEX,
            ),
            "material vertex layout",
        )?;
        objc::void_usize(
            layout,
            c"setStride:",
            usize::try_from(config.stride).expect("validated stride fits usize"),
        );
        if config.instance_stride > 0 {
            for input in config.instance_attributes {
                let attribute = required(
                    objc::object_usize(
                        attributes,
                        c"objectAtIndexedSubscript:",
                        usize::try_from(input.location).expect("validated location fits usize"),
                    ),
                    "Metal material instance attribute",
                )?;
                objc::void_usize(
                    attribute,
                    c"setFormat:",
                    material_vertex_format(input.format),
                );
                objc::void_usize(
                    attribute,
                    c"setOffset:",
                    usize::try_from(input.offset).expect("validated offset fits usize"),
                );
                objc::void_usize(
                    attribute,
                    c"setBufferIndex:",
                    MATERIAL_INSTANCE_BUFFER_INDEX,
                );
            }
            let instance_layout = required(
                objc::object_usize(
                    layouts,
                    c"objectAtIndexedSubscript:",
                    MATERIAL_INSTANCE_BUFFER_INDEX,
                ),
                "material instance layout",
            )?;
            objc::void_usize(
                instance_layout,
                c"setStride:",
                usize::try_from(config.instance_stride).expect("validated stride fits usize"),
            );
            objc::void_usize(
                instance_layout,
                c"setStepFunction:",
                VERTEX_STEP_FUNCTION_PER_INSTANCE,
            );
        }
        objc::void_object(descriptor, c"setVertexDescriptor:", vertex_descriptor);

        let mut pipeline_error = ptr::null_mut();
        let pipeline = objc::object_object_out(
            device,
            c"newRenderPipelineStateWithDescriptor:error:",
            descriptor,
            &raw mut pipeline_error,
        );
        if pipeline.is_null() {
            return Err(GraphicsError::new(format!(
                "creating Metal material pipeline failed: {}",
                objc::description(pipeline_error)
            )));
        }
        let overlay_pipeline = if config.depth == DepthMode::Off {
            objc::void_usize(descriptor, c"setSampleCount:", 1);
            objc::void_usize(
                descriptor,
                c"setDepthAttachmentPixelFormat:",
                PIXEL_FORMAT_INVALID,
            );
            let mut overlay_error = ptr::null_mut();
            let overlay = objc::object_object_out(
                device,
                c"newRenderPipelineStateWithDescriptor:error:",
                descriptor,
                &raw mut overlay_error,
            );
            if overlay.is_null() {
                objc::void(pipeline, c"release");
                return Err(GraphicsError::new(format!(
                    "creating Metal material overlay pipeline failed: {}",
                    objc::description(overlay_error)
                )));
            }
            overlay
        } else {
            ptr::null_mut()
        };
        let depth_descriptor = required(
            objc::object(objc::class(c"MTLDepthStencilDescriptor"), c"new"),
            "Metal material depth descriptor",
        )?;
        let (compare_function, depth_write) = match config.depth {
            DepthMode::TestWrite => (COMPARE_FUNCTION_LESS, true),
            DepthMode::TestOnly => (COMPARE_FUNCTION_LESS, false),
            DepthMode::TestWriteGreater => (COMPARE_FUNCTION_GREATER, true),
            DepthMode::TestOnlyGreater => (COMPARE_FUNCTION_GREATER, false),
            DepthMode::Off => (COMPARE_FUNCTION_ALWAYS, false),
        };
        objc::void_usize(
            depth_descriptor,
            c"setDepthCompareFunction:",
            compare_function,
        );
        objc::void_bool(depth_descriptor, c"setDepthWriteEnabled:", depth_write);
        let depth_state = required(
            objc::object_object(
                device,
                c"newDepthStencilStateWithDescriptor:",
                depth_descriptor,
            ),
            "Metal material depth state",
        )?;
        let mut samplers = Vec::with_capacity(config.sampler_bindings.len());
        for slot in config.sampler_bindings {
            let filter = match slot.filter {
                SamplerFilter::Nearest => SAMPLER_FILTER_NEAREST,
                SamplerFilter::Linear => SAMPLER_FILTER_LINEAR,
            };
            let mip_filter = match slot.filter {
                SamplerFilter::Nearest => SAMPLER_MIP_FILTER_NEAREST,
                SamplerFilter::Linear => SAMPLER_MIP_FILTER_LINEAR,
            };
            let address = match slot.address {
                SamplerAddress::Repeat => SAMPLER_ADDRESS_REPEAT,
                SamplerAddress::ClampToEdge => SAMPLER_ADDRESS_CLAMP_TO_EDGE,
            };
            let sampler_descriptor = required(
                objc::object(objc::class(c"MTLSamplerDescriptor"), c"new"),
                "Metal material sampler descriptor",
            )?;
            objc::void_usize(sampler_descriptor, c"setMinFilter:", filter);
            objc::void_usize(sampler_descriptor, c"setMagFilter:", filter);
            objc::void_usize(sampler_descriptor, c"setMipFilter:", mip_filter);
            objc::void_usize(sampler_descriptor, c"setSAddressMode:", address);
            objc::void_usize(sampler_descriptor, c"setTAddressMode:", address);
            let sampler = required(
                objc::object_object(
                    device,
                    c"newSamplerStateWithDescriptor:",
                    sampler_descriptor,
                ),
                "Metal material sampler",
            )?;
            objc::void(sampler_descriptor, c"release");
            samplers.push((slot.binding, sampler));
        }
        let comparison_sampler = if let Some(binding) = config.comparison_sampler_binding {
            let sampler_descriptor = required(
                objc::object(objc::class(c"MTLSamplerDescriptor"), c"new"),
                "Metal comparison sampler descriptor",
            )?;
            objc::void_usize(sampler_descriptor, c"setMinFilter:", SAMPLER_FILTER_LINEAR);
            objc::void_usize(sampler_descriptor, c"setMagFilter:", SAMPLER_FILTER_LINEAR);
            objc::void_usize(
                sampler_descriptor,
                c"setSAddressMode:",
                SAMPLER_ADDRESS_CLAMP_TO_EDGE,
            );
            objc::void_usize(
                sampler_descriptor,
                c"setTAddressMode:",
                SAMPLER_ADDRESS_CLAMP_TO_EDGE,
            );
            objc::void_usize(
                sampler_descriptor,
                c"setCompareFunction:",
                COMPARE_FUNCTION_LESS_EQUAL,
            );
            let sampler = required(
                objc::object_object(
                    device,
                    c"newSamplerStateWithDescriptor:",
                    sampler_descriptor,
                ),
                "Metal comparison sampler",
            )?;
            objc::void(sampler_descriptor, c"release");
            Some((binding, sampler))
        } else {
            None
        };
        for object in [depth_descriptor, descriptor, fragment, vertex, library] {
            objc::void(object, c"release");
        }
        Ok(MaterialPipelineResource {
            pipeline,
            overlay_pipeline,
            depth_state,
            samplers,
            uniform: config.uniform,
            storage: config.storage,
            texture_bindings: config.texture_bindings.to_vec(),
            depth_texture_binding: config.depth_texture_binding,
            depth_texture_array_binding: config.depth_texture_array_binding,
            comparison_sampler,
            instance_stride: usize::try_from(config.instance_stride)
                .expect("validated stride fits usize"),
        })
    }
}

/// Aligned bytes the material records occupy in the frame's read-only storage region; shadow
/// records pack after them. Sizes were bounds-checked when the scene was prepared.
fn material_storage_len(records: &[MaterialRecord<'_>]) -> usize {
    records
        .iter()
        .map(|record| {
            record
                .storage
                .len()
                .next_multiple_of(STORAGE_OFFSET_ALIGNMENT)
        })
        .sum()
}

/// Aligned bytes the records' instance supplies occupy in the frame's shared instance region;
/// overlay and shadow supplies pack after them. Sizes were bounds-checked when the scene was
/// prepared.
fn material_instances_len(records: &[MaterialRecord<'_>]) -> usize {
    records
        .iter()
        .map(|record| {
            record
                .instances
                .len()
                .next_multiple_of(STORAGE_OFFSET_ALIGNMENT)
        })
        .sum()
}

/// Aligned bytes the records' transient-geometry supplies occupy in the frame's shared geometry
/// region; overlay supplies pack after them. Sizes were bounds-checked when the scene was
/// prepared.
fn transient_geometry_len(records: &[MaterialRecord<'_>]) -> usize {
    records
        .iter()
        .filter_map(|record| match record.geometry {
            GeometrySource::Transient(geometry) => Some(
                geometry
                    .vertices
                    .len()
                    .next_multiple_of(STORAGE_OFFSET_ALIGNMENT)
                    + geometry
                        .indices
                        .byte_len()
                        .next_multiple_of(STORAGE_OFFSET_ALIGNMENT),
            ),
            GeometrySource::Mesh(_) | GeometrySource::MeshPart(_) => None,
        })
        .sum()
}

/// Builds a depth-only pipeline: the module's vertex entry point rasterized into a shadow map's
/// depth attachment with no fragment stage or color target, at one sample.
#[allow(clippy::too_many_lines)]
fn create_shadow_pipeline(
    device: Object,
    bytes: &[u8],
    config: &ShadowPipelineConfig<'_>,
) -> Result<ShadowPipelineResource, GraphicsError> {
    let vertex_name = CString::new(config.vertex_entry)
        .map_err(|_| GraphicsError::new("shadow vertex entry point name contains a NUL byte"))?;
    unsafe {
        let data = required(
            dispatch_data_create(
                bytes.as_ptr().cast(),
                bytes.len(),
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            "Metal shadow library data",
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
                "loading shadow metallib failed: {}",
                objc::description(library_error)
            )));
        }
        let vertex = required(
            objc::object_object(
                library,
                c"newFunctionWithName:",
                objc::ns_string(&vertex_name),
            ),
            "Metal shadow vertex function",
        )?;
        let fragment = if let Some(entry) = config.fragment_entry {
            let fragment_name = CString::new(entry).map_err(|_| {
                GraphicsError::new("shadow fragment entry point name contains a NUL byte")
            })?;
            required(
                objc::object_object(
                    library,
                    c"newFunctionWithName:",
                    objc::ns_string(&fragment_name),
                ),
                "Metal shadow fragment function",
            )?
        } else {
            ptr::null_mut()
        };
        let descriptor = required(
            objc::object(objc::class(c"MTLRenderPipelineDescriptor"), c"new"),
            "Metal shadow pipeline descriptor",
        )?;
        objc::void_object(descriptor, c"setVertexFunction:", vertex);
        if !fragment.is_null() {
            objc::void_object(descriptor, c"setFragmentFunction:", fragment);
        }
        objc::void_usize(descriptor, c"setSampleCount:", 1);
        objc::void_usize(
            descriptor,
            c"setDepthAttachmentPixelFormat:",
            PIXEL_FORMAT_DEPTH32_FLOAT,
        );

        let vertex_descriptor = required(
            objc::object(objc::class(c"MTLVertexDescriptor"), c"vertexDescriptor"),
            "Metal shadow vertex descriptor",
        )?;
        let attributes = required(
            objc::object(vertex_descriptor, c"attributes"),
            "shadow vertex attributes",
        )?;
        for input in config.attributes {
            let attribute = required(
                objc::object_usize(
                    attributes,
                    c"objectAtIndexedSubscript:",
                    usize::try_from(input.location).expect("validated location fits usize"),
                ),
                "Metal shadow vertex attribute",
            )?;
            objc::void_usize(
                attribute,
                c"setFormat:",
                material_vertex_format(input.format),
            );
            objc::void_usize(
                attribute,
                c"setOffset:",
                usize::try_from(input.offset).expect("validated offset fits usize"),
            );
            objc::void_usize(attribute, c"setBufferIndex:", MATERIAL_VERTEX_BUFFER_INDEX);
        }
        let layouts = required(
            objc::object(vertex_descriptor, c"layouts"),
            "shadow vertex layouts",
        )?;
        let layout = required(
            objc::object_usize(
                layouts,
                c"objectAtIndexedSubscript:",
                MATERIAL_VERTEX_BUFFER_INDEX,
            ),
            "shadow vertex layout",
        )?;
        objc::void_usize(
            layout,
            c"setStride:",
            usize::try_from(config.stride).expect("validated stride fits usize"),
        );
        if config.instance_stride > 0 {
            for input in config.instance_attributes {
                let attribute = required(
                    objc::object_usize(
                        attributes,
                        c"objectAtIndexedSubscript:",
                        usize::try_from(input.location).expect("validated location fits usize"),
                    ),
                    "Metal shadow instance attribute",
                )?;
                objc::void_usize(
                    attribute,
                    c"setFormat:",
                    material_vertex_format(input.format),
                );
                objc::void_usize(
                    attribute,
                    c"setOffset:",
                    usize::try_from(input.offset).expect("validated offset fits usize"),
                );
                objc::void_usize(
                    attribute,
                    c"setBufferIndex:",
                    MATERIAL_INSTANCE_BUFFER_INDEX,
                );
            }
            let instance_layout = required(
                objc::object_usize(
                    layouts,
                    c"objectAtIndexedSubscript:",
                    MATERIAL_INSTANCE_BUFFER_INDEX,
                ),
                "shadow instance layout",
            )?;
            objc::void_usize(
                instance_layout,
                c"setStride:",
                usize::try_from(config.instance_stride).expect("validated stride fits usize"),
            );
            objc::void_usize(
                instance_layout,
                c"setStepFunction:",
                VERTEX_STEP_FUNCTION_PER_INSTANCE,
            );
        }
        objc::void_object(descriptor, c"setVertexDescriptor:", vertex_descriptor);

        let mut pipeline_error = ptr::null_mut();
        let pipeline = objc::object_object_out(
            device,
            c"newRenderPipelineStateWithDescriptor:error:",
            descriptor,
            &raw mut pipeline_error,
        );
        if pipeline.is_null() {
            return Err(GraphicsError::new(format!(
                "creating Metal shadow pipeline failed: {}",
                objc::description(pipeline_error)
            )));
        }
        let depth_descriptor = required(
            objc::object(objc::class(c"MTLDepthStencilDescriptor"), c"new"),
            "Metal shadow depth descriptor",
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
            "Metal shadow depth state",
        )?;
        let mut samplers = Vec::with_capacity(config.sampler_bindings.len());
        for slot in config.sampler_bindings {
            let filter = match slot.filter {
                SamplerFilter::Nearest => SAMPLER_FILTER_NEAREST,
                SamplerFilter::Linear => SAMPLER_FILTER_LINEAR,
            };
            let mip_filter = match slot.filter {
                SamplerFilter::Nearest => SAMPLER_MIP_FILTER_NEAREST,
                SamplerFilter::Linear => SAMPLER_MIP_FILTER_LINEAR,
            };
            let address = match slot.address {
                SamplerAddress::Repeat => SAMPLER_ADDRESS_REPEAT,
                SamplerAddress::ClampToEdge => SAMPLER_ADDRESS_CLAMP_TO_EDGE,
            };
            let sampler_descriptor = required(
                objc::object(objc::class(c"MTLSamplerDescriptor"), c"new"),
                "Metal shadow sampler descriptor",
            )?;
            objc::void_usize(sampler_descriptor, c"setMinFilter:", filter);
            objc::void_usize(sampler_descriptor, c"setMagFilter:", filter);
            objc::void_usize(sampler_descriptor, c"setMipFilter:", mip_filter);
            objc::void_usize(sampler_descriptor, c"setSAddressMode:", address);
            objc::void_usize(sampler_descriptor, c"setTAddressMode:", address);
            let sampler = required(
                objc::object_object(
                    device,
                    c"newSamplerStateWithDescriptor:",
                    sampler_descriptor,
                ),
                "Metal shadow sampler",
            )?;
            objc::void(sampler_descriptor, c"release");
            samplers.push((slot.binding, sampler));
        }
        for object in [depth_descriptor, descriptor, fragment, vertex, library] {
            if !object.is_null() {
                objc::void(object, c"release");
            }
        }
        Ok(ShadowPipelineResource {
            pipeline,
            depth_state,
            uniform: config.uniform,
            storage: config.storage,
            fragment: config.fragment_entry.is_some(),
            texture_bindings: config.texture_bindings.to_vec(),
            samplers,
            instance_stride: usize::try_from(config.instance_stride)
                .expect("validated stride fits usize"),
        })
    }
}

fn create_postprocess_pipeline(
    device: Object,
    bytes: &[u8],
    config: &PostprocessPipelineConfig,
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
        Ok(PostprocessPipelineResource {
            pipeline,
            sampler,
            uniform_size: config.uniform_size,
        })
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

#[cfg(test)]
mod tests {
    use super::{INDEX_TYPE_UINT16, INDEX_TYPE_UINT32, MeshIndices, pack_mesh_storage};

    #[test]
    fn mixed_width_mesh_parts_pack_aligned_offsets_counts_and_types() {
        let u16_indices = [0_u16, 1, 2];
        let u32_indices = [1_u32, 3, 2];
        let (packed, parts) = pack_mesh_storage(
            &[0_u8; 7],
            &[
                MeshIndices::U16(&u16_indices),
                MeshIndices::U32(&u32_indices),
            ],
        )
        .expect("valid mixed-width layout");

        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].index_offset, 8);
        assert_eq!(parts[0].index_count, 3);
        assert_eq!(parts[0].index_type, INDEX_TYPE_UINT16);
        assert_eq!(parts[0].indirect_offset, 28);
        assert_eq!(parts[1].index_offset, 16);
        assert_eq!(parts[1].index_count, 3);
        assert_eq!(parts[1].index_type, INDEX_TYPE_UINT32);
        assert_eq!(parts[1].indirect_offset, 48);
        assert_eq!(packed.len(), 80);
        assert!(parts.iter().all(|part| part.index_offset % 4 == 0));
        assert!(parts.iter().all(|part| part.indirect_offset % 4 == 0));
    }
}
