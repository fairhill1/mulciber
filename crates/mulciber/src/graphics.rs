use core::cell::RefCell;
use std::format;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::vec::Vec;

use mulciber_platform::{SurfaceTarget, WindowMetrics};

use crate::backend;
use crate::resource::{DestroyRequest, DropQueue, ResourceId, ResourceKind, ResourceLease};
use crate::shader;
use crate::{
    ClearColor, FrameAcquire, FrameDisposition, GraphicsError, GraphicsErrorKind, ShaderArtifact,
    SurfaceExtent, SurfaceInfo,
};

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
/// Maximum lazy resource handles reclaimed at one completed-frame boundary. Vulkan meshes own
/// three allocations, so the native destruction count may be a small multiple of this budget.
const LAZY_RECLAIM_BUDGET: usize = 8;

/// Multisample count supported by the first textured rendering slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleCount {
    /// One sample per pixel.
    One,
    /// Four samples per pixel.
    Four,
}

/// Preferences used while selecting a surface-compatible graphics device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceRequest {
    /// Preferred sample count. Unsupported four-sample rendering falls back observably to one.
    pub preferred_sample_count: SampleCount,
}

impl Default for DeviceRequest {
    fn default() -> Self {
        Self {
            preferred_sample_count: SampleCount::Four,
        }
    }
}

/// Granularity of GPU duration diagnostics exposed by the selected backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GpuTimingSupport {
    /// The selected queue cannot produce GPU duration timestamps.
    Unsupported,
    /// Only the complete submitted command-buffer duration is available.
    Frame,
    /// The backend can measure the fixed rendering regions inside the submitted frame.
    Regions,
}

/// Native choices made while opening graphics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceSelection {
    backend: &'static str,
    sample_count: SampleCount,
    gpu_timing: GpuTimingSupport,
}

impl DeviceSelection {
    /// Native backend selected for this compilation target.
    #[must_use]
    pub const fn backend(&self) -> &'static str {
        self.backend
    }

    /// Actual sample count, including a visible fallback from four to one.
    #[must_use]
    pub const fn sample_count(&self) -> SampleCount {
        self.sample_count
    }

    /// GPU duration timing supported by the selected queue, independently of whether collection
    /// was enabled in [`DeviceRequest`].
    #[must_use]
    pub const fn gpu_timing_support(&self) -> GpuTimingSupport {
        self.gpu_timing
    }
}

struct Shared<'window> {
    id: u64,
    inner: Rc<RefCell<Option<backend::TexturedSession<'window>>>>,
    drops: Rc<DropQueue>,
}

impl Clone for Shared<'_> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            inner: Rc::clone(&self.inner),
            drops: Rc::clone(&self.drops),
        }
    }
}

/// The distinct device, queue, and surface owners opened for one native graphics session.
pub struct OpenedGraphics<'window> {
    /// Resource creation owner.
    pub device: Device<'window>,
    /// Submission owner.
    pub queue: Queue<'window>,
    /// Presentation owner.
    pub surface: Surface<'window>,
    /// Observable native selection.
    pub selection: DeviceSelection,
}

impl<'window> OpenedGraphics<'window> {
    /// Opens the target-selected native backend and produces distinct logical owners.
    ///
    /// # Errors
    ///
    /// Returns an error when validation is unavailable, no surface-compatible device exists, or
    /// native device and presentation setup fails.
    pub fn open(
        target: SurfaceTarget<'window>,
        metrics: WindowMetrics,
        request: DeviceRequest,
    ) -> Result<Self, GraphicsError> {
        let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            return Err(GraphicsError::internal(
                "graphics session identity space is exhausted",
            ));
        }
        let (session, sample_count) = backend::TexturedSession::new(target, metrics, request)?;
        let gpu_timing = session.gpu_timing_support();
        let shared = Shared {
            id,
            inner: Rc::new(RefCell::new(Some(session))),
            drops: Rc::new(DropQueue::default()),
        };
        Ok(Self {
            device: Device {
                shared: shared.clone(),
            },
            queue: Queue {
                shared: shared.clone(),
            },
            surface: Surface { shared },
            selection: DeviceSelection {
                backend: backend::BACKEND_NAME,
                sample_count,
                gpu_timing,
            },
        })
    }

    /// Drains GPU and presentation ownership and destroys the native session.
    ///
    /// All acquired frames must already be presented or abandoned. Resource handles may remain;
    /// they become inert identifiers after shutdown.
    ///
    /// # Errors
    ///
    /// Returns a deferred frame error or native completion, validation, or destruction failure.
    pub fn shutdown(self) -> Result<(), GraphicsError> {
        let Self {
            device,
            queue,
            surface,
            selection: _,
        } = self;
        if Rc::strong_count(&surface.shared.inner) != 3 {
            return Err(GraphicsError::lifecycle(
                "cannot shut graphics down while an acquired frame is live",
            ));
        }
        drop(device);
        drop(queue);
        let mut session = surface
            .shared
            .inner
            .borrow_mut()
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("graphics session is already shut down"))?;
        let pending = surface.shared.drops.take_bounded(usize::MAX);
        session.reclaim_resources(&pending)?;
        session.shutdown()
    }
}

/// Resource creation owner for one native session.
pub struct Device<'window> {
    shared: Shared<'window>,
}

impl Device<'_> {
    /// Uploads fixed-layout vertices and `u16` indices.
    ///
    /// Meshes that need 32-bit indices declare their layout and pass [`MeshIndices::U32`]
    /// through [`Device::create_mesh_with_layout`].
    ///
    /// # Errors
    ///
    /// Returns an error for empty geometry, an out-of-range index, or native allocation/upload
    /// failure.
    pub fn create_mesh(&self, vertices: &[Vertex], indices: &[u16]) -> Result<Mesh, GraphicsError> {
        if vertices.is_empty() || indices.is_empty() {
            return Err(GraphicsError::invalid_request(
                "mesh vertices and indices must be non-empty",
            ));
        }
        if indices
            .iter()
            .any(|&index| usize::from(index) >= vertices.len())
        {
            return Err(GraphicsError::invalid_request(
                "mesh contains an out-of-range index",
            ));
        }
        let id = session_mut(&self.shared)?.create_mesh(vertices, indices)?;
        Ok(Mesh {
            lease: self.lease(id, ResourceKind::Mesh),
            layout: VertexLayout::VERTEX.to_owned_layout(),
        })
    }

    /// Uploads indexed geometry from raw vertex bytes against a declared layout.
    ///
    /// The layout is retained with the mesh: a material draw whose mesh and pipeline layouts
    /// differ is rejected at submission.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid layout, vertex bytes that are empty or not a multiple of
    /// the stride, empty indices, an out-of-range index, or native allocation/upload failure.
    pub fn create_mesh_with_layout(
        &self,
        layout: VertexLayout<'_>,
        vertices: &[u8],
        indices: MeshIndices<'_>,
    ) -> Result<Mesh, GraphicsError> {
        let owned = validate_vertex_layout(layout)?;
        let stride = usize::try_from(layout.stride).map_err(|_| {
            GraphicsError::invalid_request("vertex layout stride exceeds this target")
        })?;
        if vertices.is_empty() || !vertices.len().is_multiple_of(stride) {
            return Err(GraphicsError::invalid_request(
                "mesh vertex bytes must be a non-zero multiple of the layout stride",
            ));
        }
        let vertex_count = vertices.len() / stride;
        if indices.is_empty() {
            return Err(GraphicsError::invalid_request(
                "mesh vertices and indices must be non-empty",
            ));
        }
        if indices.out_of_range(vertex_count) {
            return Err(GraphicsError::invalid_request(
                "mesh contains an out-of-range index",
            ));
        }
        let id = session_mut(&self.shared)?.create_mesh_from_bytes(vertices, indices)?;
        Ok(Mesh {
            lease: self.lease(id, ResourceKind::Mesh),
            layout: owned,
        })
    }

    /// Uploads a tightly packed RGBA8 sRGB texture.
    ///
    /// # Errors
    ///
    /// Returns an error for empty dimensions, a mismatched byte count, overflow, or native upload
    /// failure.
    pub fn create_rgba8_srgb_texture(
        &self,
        width: u32,
        height: u32,
        texels: &[u8],
    ) -> Result<Texture, GraphicsError> {
        validate_mip_level(width, height, 0, texels)?;
        let id = session_mut(&self.shared)?.create_texture(width, height, &[texels])?;
        Ok(Texture {
            lease: self.lease(id, ResourceKind::Texture),
        })
    }

    /// Uploads a tightly packed RGBA8 sRGB texture with an application-supplied mip chain.
    ///
    /// `levels[0]` holds the base image; each following level halves both dimensions (flooring at
    /// one texel), and the chain must run to its final 1×1 level. The application owns mip
    /// content, including its downsampling filter and color-space handling.
    ///
    /// # Errors
    ///
    /// Returns an error for empty dimensions, a chain that does not run from the base level to
    /// 1×1, a level whose byte count does not match its dimensions, overflow, or native upload
    /// failure.
    pub fn create_rgba8_srgb_texture_with_mips(
        &self,
        width: u32,
        height: u32,
        levels: &[&[u8]],
    ) -> Result<Texture, GraphicsError> {
        if width == 0 || height == 0 {
            return Err(GraphicsError::invalid_request(
                "texture byte count does not match its dimensions",
            ));
        }
        let expected_levels = full_mip_chain_len(width, height);
        if levels.len() != expected_levels {
            return Err(GraphicsError::invalid_request(format!(
                "texture mip chain supplies {} levels but {width}x{height} needs {expected_levels} \
                 levels to reach 1x1",
                levels.len()
            )));
        }
        for (level, texels) in (0_u32..).zip(levels) {
            validate_mip_level(width, height, level, texels)?;
        }
        let id = session_mut(&self.shared)?.create_texture(width, height, levels)?;
        Ok(Texture {
            lease: self.lease(id, ResourceKind::Texture),
        })
    }

    /// Creates a depth-tested textured pipeline from target-selected offline shader code.
    ///
    /// # Errors
    ///
    /// Returns an error when native shader loading or pipeline creation fails.
    pub fn create_textured_pipeline(
        &self,
        shader: ShaderArtifact<'_>,
    ) -> Result<TexturedPipeline, GraphicsError> {
        let id = session_mut(&self.shared)?.create_pipeline(shader)?;
        Ok(TexturedPipeline {
            lease: self.lease(id, ResourceKind::TexturedPipeline),
        })
    }

    /// Creates a depth-tested textured pipeline whose vertex stage consumes one model-view-
    /// projection matrix per instance.
    ///
    /// The shader module must contain `instanced_vertex` and `cube_fragment` entry points. Matrix
    /// columns occupy vertex locations 3 through 6 with per-instance stepping.
    ///
    /// # Errors
    ///
    /// Returns an error when native shader loading or pipeline creation fails.
    pub fn create_instanced_textured_pipeline(
        &self,
        shader: ShaderArtifact<'_>,
    ) -> Result<InstancedTexturedPipeline, GraphicsError> {
        let id = session_mut(&self.shared)?.create_instanced_pipeline(shader)?;
        Ok(InstancedTexturedPipeline {
            lease: self.lease(id, ResourceKind::InstancedTexturedPipeline),
        })
    }

    /// Creates the single-sample fullscreen pipeline for the post-processing checkpoint.
    ///
    /// The shader module must contain `post_vertex` and `post_fragment` entry points. The
    /// fragment stage samples the resolved scene color through bindings 1 and 2.
    ///
    /// # Errors
    ///
    /// Returns an error when native shader loading, sampler creation, or pipeline creation fails.
    pub fn create_postprocess_pipeline(
        &self,
        shader: ShaderArtifact<'_>,
    ) -> Result<PostprocessPipeline, GraphicsError> {
        let id = session_mut(&self.shared)?.create_postprocess_pipeline(shader)?;
        Ok(PostprocessPipeline {
            lease: self.lease(id, ResourceKind::PostprocessPipeline),
        })
    }

    /// Creates a depth-tested pipeline from an application-authored shader module, vertex layout,
    /// and binding declaration.
    ///
    /// The declaration is validated against the interface `mulciber-shader` recorded in the
    /// artifact; a mismatch names the offending attribute or slot. The pipeline uses the
    /// session's selected sample count plus the declared [`BlendMode`] and [`DepthMode`].
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid layout, a missing entry point, a declaration that does not
    /// match the artifact's recorded interface, an interface construct outside the current
    /// material vocabulary, or native shader loading and pipeline creation failure.
    pub fn create_material_pipeline(
        &self,
        descriptor: MaterialPipelineDescriptor<'_>,
    ) -> Result<MaterialPipeline, GraphicsError> {
        let layout = validate_vertex_layout(descriptor.vertex_layout)?;
        let instance_layout = descriptor
            .instance_layout
            .map(|instance| validate_instance_layout(&layout, instance))
            .transpose()?;
        let interface = descriptor.shader.parse_interface();
        let vertex_entry = find_entry_point(
            &interface,
            descriptor.vertex_entry,
            shader::INTERFACE_STAGE_VERTEX,
            "vertex",
        )?;
        find_entry_point(
            &interface,
            descriptor.fragment_entry,
            shader::INTERFACE_STAGE_FRAGMENT,
            "fragment",
        )?;
        validate_layouts_against_entry(&layout, instance_layout.as_ref(), vertex_entry)?;
        let declaration = validate_bindings_against_interface(descriptor.bindings, &interface)?;
        let config = MaterialPipelineConfig {
            vertex_entry: descriptor.vertex_entry,
            fragment_entry: descriptor.fragment_entry,
            stride: layout.stride,
            attributes: &layout.attributes,
            instance_stride: instance_layout
                .as_ref()
                .map_or(0, |instance| instance.stride),
            instance_attributes: instance_layout
                .as_ref()
                .map_or(&[][..], |instance| &instance.attributes),
            uniform: declaration.uniform,
            storage: declaration.storage,
            texture_bindings: &declaration.texture_bindings,
            sampler_bindings: &declaration.sampler_bindings,
            depth_texture_binding: declaration.depth_texture,
            depth_texture_array_binding: declaration.depth_texture_array,
            comparison_sampler_binding: declaration.comparison_sampler,
            blend: descriptor.blend,
            depth: descriptor.depth,
        };
        let id = session_mut(&self.shared)?.create_material_pipeline(descriptor.shader, &config)?;
        Ok(MaterialPipeline {
            lease: self.lease(id, ResourceKind::MaterialPipeline),
            layout,
            uniform_size: declaration.uniform.map_or(0, |(_, size)| size),
            storage_size: declaration.storage.map_or(0, |(_, size)| size),
            instance_stride: instance_layout.map_or(0, |instance| instance.stride),
            texture_count: declaration.texture_bindings.len(),
            shadow_slot: if declaration.depth_texture.is_some() {
                Some(ShadowSlotKind::Map)
            } else if declaration.depth_texture_array.is_some() {
                Some(ShadowSlotKind::Array)
            } else {
                None
            },
            depth: descriptor.depth,
        })
    }

    /// Creates a square sampleable depth target for shadow passes.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero extent, an extent above [`SHADOW_MAP_SIZE_LIMIT`], or native
    /// image allocation failure.
    pub fn create_shadow_map(&self, size: u32) -> Result<ShadowMap, GraphicsError> {
        if size == 0 || size > SHADOW_MAP_SIZE_LIMIT {
            return Err(GraphicsError::invalid_request(format!(
                "shadow map extent {size} is outside the supported 1 through \
                 {SHADOW_MAP_SIZE_LIMIT}"
            )));
        }
        let id = session_mut(&self.shared)?.create_shadow_map(size)?;
        Ok(ShadowMap {
            lease: self.lease(id, ResourceKind::ShadowMap),
            size,
        })
    }

    /// Creates a square layered sampleable depth target for cascaded shadow passes, one
    /// cascade per layer.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero extent, an extent above [`SHADOW_MAP_SIZE_LIMIT`], a zero
    /// layer count, a layer count above [`SHADOW_MAP_LAYER_LIMIT`], or native image allocation
    /// failure.
    pub fn create_shadow_map_array(
        &self,
        size: u32,
        layers: u32,
    ) -> Result<ShadowMapArray, GraphicsError> {
        if size == 0 || size > SHADOW_MAP_SIZE_LIMIT {
            return Err(GraphicsError::invalid_request(format!(
                "shadow map array extent {size} is outside the supported 1 through \
                 {SHADOW_MAP_SIZE_LIMIT}"
            )));
        }
        if layers == 0 || layers > SHADOW_MAP_LAYER_LIMIT {
            return Err(GraphicsError::invalid_request(format!(
                "shadow map array layer count {layers} is outside the supported 1 through \
                 {SHADOW_MAP_LAYER_LIMIT}"
            )));
        }
        let id = session_mut(&self.shared)?.create_shadow_map_array(size, layers)?;
        Ok(ShadowMapArray {
            lease: self.lease(id, ResourceKind::ShadowMapArray),
            size,
            layers,
        })
    }

    /// Creates a depth-only pipeline from an application-authored shader module for shadow
    /// passes.
    ///
    /// The pipeline runs the named vertex entry point into a [`ShadowMap`]'s depth target,
    /// testing and writing depth. Shadow pipelines support at most one uniform binding and one
    /// read-only storage binding (so skinned casters shadow with the same bone palette as
    /// their material records). A caster that must carve fragments out of the depth result —
    /// typically a foliage cutout alpha test — additionally names a fragment entry point,
    /// which unlocks texture and sampler bindings for the test; without a fragment entry the
    /// pipeline runs no fragment stage and the module must record no other bindings. An
    /// instance layout mirrors the caster's material pipeline so scattered geometry shadows
    /// through the same per-instance transforms.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid layout, a missing entry point, a declaration that does
    /// not match the artifact's recorded interface, a binding outside the supported kinds, or
    /// native shader loading and pipeline creation failure.
    pub fn create_shadow_pipeline(
        &self,
        descriptor: ShadowPipelineDescriptor<'_>,
    ) -> Result<ShadowPipeline, GraphicsError> {
        let layout = validate_vertex_layout(descriptor.vertex_layout)?;
        let instance_layout = descriptor
            .instance_layout
            .map(|instance| validate_instance_layout(&layout, instance))
            .transpose()?;
        let interface = descriptor.shader.parse_interface();
        let vertex_entry = find_entry_point(
            &interface,
            descriptor.vertex_entry,
            shader::INTERFACE_STAGE_VERTEX,
            "vertex",
        )?;
        if let Some(fragment_entry) = descriptor.fragment_entry {
            find_entry_point(
                &interface,
                fragment_entry,
                shader::INTERFACE_STAGE_FRAGMENT,
                "fragment",
            )?;
        }
        let (consumed, consumed_instance) =
            validate_layouts_cover_entry(&layout, instance_layout.as_ref(), vertex_entry)?;
        let declaration = validate_bindings_against_interface(descriptor.bindings, &interface)?;
        if declaration.depth_texture.is_some()
            || declaration.depth_texture_array.is_some()
            || declaration.comparison_sampler.is_some()
        {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                "shadow pipelines do not sample depth resources",
            ));
        }
        if descriptor.fragment_entry.is_none()
            && (!declaration.texture_bindings.is_empty()
                || !declaration.sampler_bindings.is_empty())
        {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                "shadow pipelines support texture and sampler bindings only with a declared \
                 fragment entry point",
            ));
        }
        let config = ShadowPipelineConfig {
            vertex_entry: descriptor.vertex_entry,
            fragment_entry: descriptor.fragment_entry,
            stride: layout.stride,
            attributes: &consumed,
            instance_stride: instance_layout
                .as_ref()
                .map_or(0, |instance| instance.stride),
            instance_attributes: &consumed_instance,
            uniform: declaration.uniform,
            storage: declaration.storage,
            texture_bindings: &declaration.texture_bindings,
            sampler_bindings: &declaration.sampler_bindings,
        };
        let id = session_mut(&self.shared)?.create_shadow_pipeline(descriptor.shader, &config)?;
        Ok(ShadowPipeline {
            lease: self.lease(id, ResourceKind::ShadowPipeline),
            layout,
            uniform_size: declaration.uniform.map_or(0, |(_, size)| size),
            storage_size: declaration.storage.map_or(0, |(_, size)| size),
            instance_stride: instance_layout.map_or(0, |instance| instance.stride),
            texture_count: declaration.texture_bindings.len(),
        })
    }

    /// Creates depth and optional multisample color storage for one surface generation.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty extent or native image allocation failure.
    pub fn create_render_targets(&self, info: SurfaceInfo) -> Result<RenderTargets, GraphicsError> {
        let id = session_mut(&self.shared)?.create_render_targets(info)?;
        Ok(RenderTargets {
            lease: self.lease(id, ResourceKind::RenderTargets),
            info,
        })
    }

    /// Creates depth, resolved scene color, and optional multisample color storage for one surface
    /// generation, rendered at the surface's native extent.
    ///
    /// The resolved scene color is both a render target and the sampled input to the fullscreen
    /// post-processing pass.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty extent or native image allocation failure.
    pub fn create_postprocess_targets(
        &self,
        info: SurfaceInfo,
    ) -> Result<PostprocessTargets, GraphicsError> {
        self.create_scaled_postprocess_targets(info, RenderScale::NATIVE)
    }

    /// Creates postprocess targets whose offscreen scene extent is scaled relative to the
    /// presentable extent.
    ///
    /// The scene pass renders into the scaled offscreen storage, and the fullscreen
    /// post-processing pass resamples it to the surface's native extent through its linear
    /// sampler, so a scale below native trades scene-pass fill cost for sharpness while text
    /// or overlays drawn by the postprocess stage stay native. Scales above native
    /// supersample. Both dimensions floor at one texel.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty extent or native image allocation failure.
    pub fn create_scaled_postprocess_targets(
        &self,
        info: SurfaceInfo,
        scale: RenderScale,
    ) -> Result<PostprocessTargets, GraphicsError> {
        let scene_extent = scale.scene_extent(info.extent());
        let id = session_mut(&self.shared)?.create_postprocess_targets(info, scene_extent)?;
        Ok(PostprocessTargets {
            lease: self.lease(id, ResourceKind::PostprocessTargets),
            info,
            scale,
        })
    }

    /// Destroys an uploaded mesh after its last submitted GPU use completes.
    ///
    /// Dropping the handle performs the same reclamation lazily at the next mutable graphics
    /// operation. This explicit form reports stale or mixed-session handles immediately.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_mesh(&self, mut mesh: Mesh) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut mesh.lease, ResourceKind::Mesh)
    }

    /// Destroys an uploaded texture after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_texture(&self, mut texture: Texture) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut texture.lease, ResourceKind::Texture)
    }

    /// Destroys a textured pipeline after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_textured_pipeline(
        &self,
        mut pipeline: TexturedPipeline,
    ) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut pipeline.lease, ResourceKind::TexturedPipeline)
    }

    /// Destroys an instanced textured pipeline after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_instanced_textured_pipeline(
        &self,
        mut pipeline: InstancedTexturedPipeline,
    ) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut pipeline.lease, ResourceKind::InstancedTexturedPipeline)
    }

    /// Destroys a postprocess pipeline after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_postprocess_pipeline(
        &self,
        mut pipeline: PostprocessPipeline,
    ) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut pipeline.lease, ResourceKind::PostprocessPipeline)
    }

    /// Destroys a material pipeline after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_material_pipeline(
        &self,
        mut pipeline: MaterialPipeline,
    ) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut pipeline.lease, ResourceKind::MaterialPipeline)
    }

    /// Destroys a shadow map after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_shadow_map(&self, mut map: ShadowMap) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut map.lease, ResourceKind::ShadowMap)
    }

    /// Destroys a shadow map array after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_shadow_map_array(&self, mut array: ShadowMapArray) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut array.lease, ResourceKind::ShadowMapArray)
    }

    /// Destroys a shadow pipeline after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_shadow_pipeline(
        &self,
        mut pipeline: ShadowPipeline,
    ) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut pipeline.lease, ResourceKind::ShadowPipeline)
    }

    /// Destroys generation-dependent render targets after their last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_render_targets(&self, mut targets: RenderTargets) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut targets.lease, ResourceKind::RenderTargets)
    }

    /// Destroys generation-dependent postprocess targets after their last GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_postprocess_targets(
        &self,
        mut targets: PostprocessTargets,
    ) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut targets.lease, ResourceKind::PostprocessTargets)
    }

    fn lease(&self, id: ResourceId, kind: ResourceKind) -> ResourceLease {
        ResourceLease::new(self.shared.id, id, kind, Rc::clone(&self.shared.drops))
    }

    fn destroy_lease(
        &self,
        lease: &mut ResourceLease,
        kind: ResourceKind,
    ) -> Result<(), GraphicsError> {
        if lease.session != self.shared.id {
            return Err(GraphicsError::invalid_request(format!(
                "{} handle belongs to a different graphics session than this device",
                kind.label()
            )));
        }
        session_mut(&self.shared)?.destroy_resource(DestroyRequest { kind, id: lease.id })?;
        lease.disarm();
        Ok(())
    }
}

/// Submission owner for one native session.
pub struct Queue<'window> {
    shared: Shared<'window>,
}

impl Queue<'_> {
    /// Enables or disables asynchronous GPU duration diagnostics for future submissions.
    ///
    /// Enabling an unsupported queue succeeds so rendering remains available; capability and
    /// drain results keep the fallback observable.
    ///
    /// # Errors
    ///
    /// Returns an error after session shutdown or if native instrumentation allocation fails.
    pub fn set_gpu_timing_enabled(&mut self, enabled: bool) -> Result<(), GraphicsError> {
        session_mut(&self.shared)?.set_gpu_timing_enabled(enabled)
    }

    /// Drains completed GPU duration samples without waiting for unfinished GPU work.
    ///
    /// Samples may arrive one or more frames after submission. Their frame index is the same
    /// zero-based session index reported by [`PresentedFrame::index`], allowing an application to
    /// correlate GPU work with presentation feedback. Ignoring the drain costs bounded memory.
    ///
    /// # Errors
    ///
    /// Returns an error after session shutdown.
    pub fn take_gpu_timings(&mut self) -> Result<GpuTimingFeedback, GraphicsError> {
        Ok(session_mut(&self.shared)?.take_gpu_timings())
    }

    /// Renders one explicitly selected scene recipe, presents the frame, and consumes it.
    ///
    /// This stable verb prevents each experimentally extracted workload from growing another
    /// queue-method name. `SceneContent` and `SceneOutput` compose the narrow axes supported by the
    /// current slice; they are not a general command encoder.
    ///
    /// # Errors
    ///
    /// Returns the validation, native encoding, synchronization, submission, or presentation
    /// error produced by the selected recipe.
    pub fn render_and_present(
        &mut self,
        frame: Frame<'_>,
        submission: SceneSubmission<'_>,
    ) -> Result<FrameDisposition, GraphicsError> {
        if submission.shadow.is_some() && !matches!(submission.content, SceneContent::Material(_)) {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                "the shadow pass composes with material scene content only",
            ));
        }
        if submission.overlay.is_some()
            && !matches!(
                (submission.content, submission.output),
                (SceneContent::Material(_), SceneOutput::Postprocessed { .. })
            )
        {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                "the overlay pass composes with material scene content and postprocessed output \
                 only",
            ));
        }
        match (submission.content, submission.output) {
            (SceneContent::Textured(draws), SceneOutput::Direct(targets)) => self
                .draw_textured_scene_and_present(
                    frame,
                    TexturedScene {
                        draws,
                        targets,
                        clear: submission.clear,
                    },
                ),
            (SceneContent::Textured(draws), SceneOutput::Postprocessed { pipeline, targets }) => {
                self.draw_textured_scene_postprocessed_and_present(
                    frame,
                    PostprocessedScene {
                        draws,
                        postprocess_pipeline: pipeline,
                        targets,
                        clear: submission.clear,
                    },
                )
            }
            (SceneContent::Instanced(batches), SceneOutput::Direct(targets)) => self
                .draw_instanced_textured_scene_and_present(
                    frame,
                    batches,
                    targets,
                    submission.clear,
                ),
            (
                SceneContent::Instanced(batches),
                SceneOutput::Postprocessed { pipeline, targets },
            ) => self.draw_instanced_textured_scene_postprocessed_and_present(
                frame,
                batches,
                pipeline,
                targets,
                submission.clear,
            ),
            (SceneContent::Material(records), SceneOutput::Direct(targets)) => self
                .draw_material_scene_and_present(
                    frame,
                    records,
                    submission.shadow,
                    targets,
                    submission.clear,
                ),
            (SceneContent::Material(records), SceneOutput::Postprocessed { pipeline, targets }) => {
                self.draw_material_scene_postprocessed_and_present(
                    frame,
                    records,
                    submission.shadow,
                    submission.overlay,
                    pipeline,
                    targets,
                    submission.clear,
                )
            }
        }
    }

    /// Draws one indexed textured mesh, presents the frame, and consumes it.
    ///
    /// # Errors
    ///
    /// Returns an error for mixed-session or stale handles, a non-finite transform, or native
    /// encoding, submission, validation, or presentation failure.
    pub fn draw_textured_and_present(
        &mut self,
        frame: Frame<'_>,
        draw: TexturedDraw<'_>,
    ) -> Result<FrameDisposition, GraphicsError> {
        let scene_draw = TexturedSceneDraw {
            mesh: draw.mesh,
            texture: draw.texture,
            pipeline: draw.pipeline,
            model_view_projection: draw.model_view_projection,
        };
        self.draw_textured_scene_and_present(
            frame,
            TexturedScene {
                draws: core::slice::from_ref(&scene_draw),
                targets: draw.targets,
                clear: draw.clear,
            },
        )
    }

    /// Draws a non-empty sequence of textured objects in one depth-tested render pass, presents
    /// the frame, and consumes it.
    ///
    /// Each object independently selects its mesh, texture, pipeline, and transform. The targets
    /// and clear operation belong to the scene pass rather than being repeated per object.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty scene, mixed-session or stale handles, a non-finite transform,
    /// or native encoding, submission, validation, or presentation failure.
    pub fn draw_textured_scene_and_present(
        &mut self,
        mut frame: Frame<'_>,
        scene: TexturedScene<'_>,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_scene(frame.shared.id, frame.info, scene.draws, scene.targets)?;
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_scene_and_present(
            token,
            scene.draws,
            scene.targets.lease.id,
            scene.clear,
        )
    }

    /// Draws one indexed textured mesh into sampled offscreen color, runs one fullscreen
    /// post-processing pass, presents the frame, and consumes it.
    ///
    /// # Errors
    ///
    /// Returns an error for mixed-session or stale handles, a non-finite transform, or native
    /// encoding, synchronization, submission, validation, or presentation failure.
    pub fn draw_textured_postprocessed_and_present(
        &mut self,
        frame: Frame<'_>,
        draw: PostprocessedDraw<'_>,
    ) -> Result<FrameDisposition, GraphicsError> {
        let scene_draw = TexturedSceneDraw {
            mesh: draw.mesh,
            texture: draw.texture,
            pipeline: draw.scene_pipeline,
            model_view_projection: draw.model_view_projection,
        };
        self.draw_textured_scene_postprocessed_and_present(
            frame,
            PostprocessedScene {
                draws: core::slice::from_ref(&scene_draw),
                postprocess_pipeline: draw.postprocess_pipeline,
                targets: draw.targets,
                clear: draw.clear,
            },
        )
    }

    /// Draws a non-empty sequence of textured objects into resolved scene color, runs one
    /// fullscreen post-processing pass, presents the frame, and consumes it.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty scene, mixed-session or stale handles, a non-finite transform,
    /// or native encoding, synchronization, submission, validation, or presentation failure.
    pub fn draw_textured_scene_postprocessed_and_present(
        &mut self,
        mut frame: Frame<'_>,
        scene: PostprocessedScene<'_>,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_scene(frame.shared.id, frame.info, scene.draws, scene.targets)?;
        if scene.postprocess_pipeline.lease.session != self.shared.id {
            return Err(GraphicsError::invalid_request(
                "postprocess pipeline belongs to a different graphics session than the queue",
            ));
        }
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_scene_postprocessed_and_present(
            token,
            scene.draws,
            scene.postprocess_pipeline.lease.id,
            scene.targets.lease.id,
            scene.clear,
        )
    }

    /// Draws a non-empty sequence of non-empty textured instance batches in one depth-tested
    /// render pass, presents the frame, and consumes it.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty scene or batch, mixed-session or stale handles, a non-finite
    /// transform, an unsupported native count, or native encoding, submission, validation, or
    /// presentation failure.
    fn draw_instanced_textured_scene_and_present(
        &mut self,
        mut frame: Frame<'_>,
        batches: &[TexturedInstanceBatch<'_>],
        targets: &RenderTargets,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_instanced_scene(frame.shared.id, frame.info, batches, targets)?;
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_instanced_scene_and_present(
            token,
            batches,
            targets.lease.id,
            clear,
        )
    }

    /// Draws a non-empty sequence of non-empty textured instance batches into resolved scene
    /// color, runs one fullscreen post-processing pass, presents the frame, and consumes it.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty scene or batch, mixed-session or stale handles, a non-finite
    /// transform, an unsupported native count, or native encoding, synchronization, submission,
    /// validation, or presentation failure.
    fn draw_instanced_textured_scene_postprocessed_and_present(
        &mut self,
        mut frame: Frame<'_>,
        batches: &[TexturedInstanceBatch<'_>],
        postprocess_pipeline: &PostprocessPipeline,
        targets: &PostprocessTargets,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_instanced_scene(frame.shared.id, frame.info, batches, targets)?;
        if postprocess_pipeline.lease.session != self.shared.id {
            return Err(GraphicsError::invalid_request(
                "postprocess pipeline belongs to a different graphics session than the queue",
            ));
        }
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_instanced_scene_postprocessed_and_present(
            token,
            batches,
            postprocess_pipeline.lease.id,
            targets.lease.id,
            clear,
        )
    }

    /// Draws an optional depth-only shadow pass followed by a non-empty sequence of
    /// application-authored material records in one depth-tested render pass, presents the
    /// frame, and consumes it.
    fn draw_material_scene_and_present(
        &mut self,
        mut frame: Frame<'_>,
        records: &[MaterialRecord<'_>],
        shadow: Option<ShadowPrepass<'_>>,
        targets: &RenderTargets,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_material_scene(frame.shared.id, frame.info, records, targets)?;
        let depth_clear = material_scene_depth_clear(records)?;
        self.validate_shadow_pass(frame.shared.id, shadow.as_ref())?;
        session_ref(&self.shared)?.validate_shadow_sampling(records, shadow.as_ref())?;
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_material_scene_and_present(
            token,
            records,
            shadow.as_ref(),
            targets.lease.id,
            clear,
            depth_clear,
        )
    }

    /// Draws an optional depth-only shadow pass, then a non-empty sequence of
    /// application-authored material records into resolved scene color, runs one fullscreen
    /// post-processing pass, draws any overlay records into the presentable target at native
    /// extent, presents the frame, and consumes it.
    #[allow(clippy::too_many_arguments)]
    fn draw_material_scene_postprocessed_and_present(
        &mut self,
        mut frame: Frame<'_>,
        records: &[MaterialRecord<'_>],
        shadow: Option<ShadowPrepass<'_>>,
        overlay: Option<&[MaterialRecord<'_>]>,
        postprocess_pipeline: &PostprocessPipeline,
        targets: &PostprocessTargets,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_material_scene(frame.shared.id, frame.info, records, targets)?;
        let depth_clear = material_scene_depth_clear(records)?;
        self.validate_shadow_pass(frame.shared.id, shadow.as_ref())?;
        session_ref(&self.shared)?.validate_shadow_sampling(records, shadow.as_ref())?;
        if let Some(overlay) = overlay {
            self.validate_overlay_records(frame.shared.id, overlay)?;
        }
        if postprocess_pipeline.lease.session != self.shared.id {
            return Err(GraphicsError::invalid_request(
                "postprocess pipeline belongs to a different graphics session than the queue",
            ));
        }
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_material_scene_postprocessed_and_present(
            token,
            records,
            shadow.as_ref(),
            overlay.unwrap_or(&[]),
            postprocess_pipeline.lease.id,
            targets.lease.id,
            clear,
            depth_clear,
        )
    }

    /// Validates a shadow prepass's handles, record shape, cascade agreement, and
    /// uniform/layout agreement.
    fn validate_shadow_pass(
        &self,
        frame_session: u64,
        shadow: Option<&ShadowPrepass<'_>>,
    ) -> Result<(), GraphicsError> {
        let Some(shadow) = shadow else {
            return Ok(());
        };
        let target_session = match shadow {
            ShadowPrepass::Single(pass) => {
                if pass.records.is_empty() {
                    return Err(GraphicsError::invalid_request(
                        "shadow pass must contain at least one record",
                    ));
                }
                ("shadow map", pass.map.lease.session)
            }
            ShadowPrepass::Cascaded(pass) => {
                let layers = usize::try_from(pass.map.layers()).expect("u32 layers fit usize");
                if pass.cascades.len() != layers {
                    return Err(GraphicsError::invalid_request(format!(
                        "cascaded shadow pass supplies {} cascade record lists but its map has \
                         {layers} layers",
                        pass.cascades.len()
                    )));
                }
                ("shadow map array", pass.map.lease.session)
            }
        };
        for (label, session) in shadow
            .records()
            .flat_map(|record| {
                [
                    ("shadow pipeline", record.pipeline.lease.session),
                    ("mesh", record.mesh.lease.session),
                ]
                .into_iter()
                .chain(
                    record
                        .textures
                        .iter()
                        .map(|texture| ("texture", texture.lease.session)),
                )
            })
            .chain([target_session])
        {
            if session != self.shared.id || session != frame_session {
                return Err(GraphicsError::invalid_request(format!(
                    "{label} belongs to a different graphics session than the queue and frame"
                )));
            }
        }
        for record in shadow.records() {
            let expected =
                usize::try_from(record.pipeline.uniform_size).expect("u32 size fits usize");
            if record.uniform.len() != expected {
                return Err(GraphicsError::invalid_request(format!(
                    "shadow record supplies {} uniform bytes but its pipeline declares {expected}",
                    record.uniform.len()
                )));
            }
            let expected_storage =
                usize::try_from(record.pipeline.storage_size).expect("u32 size fits usize");
            if record.storage.len() != expected_storage {
                return Err(GraphicsError::invalid_request(format!(
                    "shadow record supplies {} storage bytes but its pipeline declares \
                     {expected_storage}",
                    record.storage.len()
                )));
            }
            if record.mesh.layout != record.pipeline.layout {
                return Err(GraphicsError::invalid_request(
                    "shadow record's mesh vertex layout does not match its pipeline's declared \
                     layout",
                ));
            }
            if record.textures.len() != record.pipeline.texture_count {
                return Err(GraphicsError::invalid_request(format!(
                    "shadow record supplies {} textures but its pipeline declares {} texture \
                     slots",
                    record.textures.len(),
                    record.pipeline.texture_count
                )));
            }
            validate_instance_supply(
                "shadow record",
                record.instances,
                record.pipeline.instance_stride,
            )?;
        }
        Ok(())
    }

    fn validate_material_scene(
        &self,
        frame_session: u64,
        frame_info: SurfaceInfo,
        records: &[MaterialRecord<'_>],
        targets: &impl SceneTargets,
    ) -> Result<(), GraphicsError> {
        if records.is_empty() {
            return Err(GraphicsError::invalid_request(
                "material scene must contain at least one record",
            ));
        }
        self.validate_targets(frame_session, frame_info, targets)?;
        self.validate_material_records(frame_session, records)
    }

    /// Validates the overlay pass: non-empty records whose pipelines fit the presentable pass,
    /// which carries no depth target and samples no shadow map.
    fn validate_overlay_records(
        &self,
        frame_session: u64,
        overlay: &[MaterialRecord<'_>],
    ) -> Result<(), GraphicsError> {
        if overlay.is_empty() {
            return Err(GraphicsError::invalid_request(
                "overlay pass must contain at least one record",
            ));
        }
        self.validate_material_records(frame_session, overlay)?;
        for record in overlay {
            if record.pipeline.depth != DepthMode::Off {
                return Err(GraphicsError::invalid_request(
                    "overlay records draw into the presentable target, which carries no depth \
                     target; their pipelines must declare DepthMode::Off",
                ));
            }
            if record.pipeline.shadow_slot.is_some() {
                return Err(GraphicsError::invalid_request(
                    "overlay records draw after the scene pass and may not sample a shadow map; \
                     their pipelines must declare no depth-texture slot",
                ));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn validate_material_records(
        &self,
        frame_session: u64,
        records: &[MaterialRecord<'_>],
    ) -> Result<(), GraphicsError> {
        for record in records {
            let mesh = match record.geometry {
                GeometrySource::Mesh(mesh) => Some(mesh),
                GeometrySource::Transient(_) => None,
            };
            let handles = [("material pipeline", record.pipeline.lease.session)]
                .into_iter()
                .chain(mesh.map(|mesh| ("mesh", mesh.lease.session)))
                .chain(
                    record
                        .textures
                        .iter()
                        .map(|texture| ("texture", texture.lease.session)),
                )
                .chain(record.shadow_map.iter().map(|source| match source {
                    ShadowSource::Map(map) => ("shadow map", map.lease.session),
                    ShadowSource::Array(array) => ("shadow map array", array.lease.session),
                }));
            for (label, session) in handles {
                if session != self.shared.id || session != frame_session {
                    return Err(GraphicsError::invalid_request(format!(
                        "{label} belongs to a different graphics session than the queue and frame"
                    )));
                }
            }
            let supplied = record.shadow_map.map(|source| match source {
                ShadowSource::Map(_) => ShadowSlotKind::Map,
                ShadowSource::Array(_) => ShadowSlotKind::Array,
            });
            if supplied != record.pipeline.shadow_slot {
                return Err(GraphicsError::invalid_request(
                    match (supplied, record.pipeline.shadow_slot) {
                        (Some(_), None) => {
                            "material record supplies a shadow map but its pipeline declares no \
                             depth-texture slot"
                        }
                        (None, Some(_)) => {
                            "material record supplies no shadow map but its pipeline declares a \
                             depth-texture slot"
                        }
                        (Some(ShadowSlotKind::Map), _) => {
                            "material record supplies a single shadow map but its pipeline \
                             declares a depth-texture-array slot"
                        }
                        _ => {
                            "material record supplies a shadow map array but its pipeline \
                             declares a plain depth-texture slot"
                        }
                    },
                ));
            }
            if record.textures.len() != record.pipeline.texture_count {
                return Err(GraphicsError::invalid_request(format!(
                    "material record supplies {} textures but its pipeline declares {} texture \
                     slots",
                    record.textures.len(),
                    record.pipeline.texture_count
                )));
            }
            let expected =
                usize::try_from(record.pipeline.uniform_size).expect("u32 size fits usize");
            if record.uniform.len() != expected {
                return Err(GraphicsError::invalid_request(format!(
                    "material record supplies {} uniform bytes but its pipeline declares {}",
                    record.uniform.len(),
                    expected
                )));
            }
            let expected_storage =
                usize::try_from(record.pipeline.storage_size).expect("u32 size fits usize");
            if record.storage.len() != expected_storage {
                return Err(GraphicsError::invalid_request(format!(
                    "material record supplies {} storage bytes but its pipeline declares \
                     {expected_storage}",
                    record.storage.len()
                )));
            }
            validate_instance_supply(
                "material record",
                record.instances,
                record.pipeline.instance_stride,
            )?;
            match record.geometry {
                GeometrySource::Mesh(mesh) => {
                    if mesh.layout != record.pipeline.layout {
                        return Err(GraphicsError::invalid_request(
                            "material record's mesh vertex layout does not match its pipeline's \
                             declared layout",
                        ));
                    }
                }
                GeometrySource::Transient(geometry) => {
                    let stride = usize::try_from(record.pipeline.layout.stride)
                        .expect("validated stride fits usize");
                    if geometry.vertices.is_empty()
                        || !geometry.vertices.len().is_multiple_of(stride)
                    {
                        return Err(GraphicsError::invalid_request(
                            "material record's transient vertex bytes must be a non-zero \
                             multiple of its pipeline's declared layout stride",
                        ));
                    }
                    if geometry.indices.is_empty() {
                        return Err(GraphicsError::invalid_request(
                            "material record's transient geometry must supply at least one index",
                        ));
                    }
                    if geometry
                        .indices
                        .out_of_range(geometry.vertices.len() / stride)
                    {
                        return Err(GraphicsError::invalid_request(
                            "material record's transient geometry contains an out-of-range index",
                        ));
                    }
                    let supplied = geometry
                        .vertices
                        .len()
                        .checked_add(geometry.indices.byte_len())
                        .filter(|&supplied| {
                            supplied
                                <= usize::try_from(TRANSIENT_GEOMETRY_SIZE_LIMIT)
                                    .expect("u32 limit fits usize")
                        });
                    if supplied.is_none() {
                        return Err(GraphicsError::invalid_request(format!(
                            "material record's transient geometry exceeds the \
                             {TRANSIENT_GEOMETRY_SIZE_LIMIT}-byte supply limit",
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Rejects scene targets from another session as an invalid request, and same-session targets
    /// whose surface information no longer matches the frame as stale, so the two failures keep
    /// their distinct corrections: fix the caller versus rebuild the targets.
    fn validate_targets(
        &self,
        frame_session: u64,
        frame_info: SurfaceInfo,
        targets: &impl SceneTargets,
    ) -> Result<(), GraphicsError> {
        let label = targets.label();
        if targets.session() != self.shared.id || targets.session() != frame_session {
            return Err(GraphicsError::invalid_request(format!(
                "{label} belong to a different graphics session than the queue and frame"
            )));
        }
        if targets.info() != frame_info {
            return Err(GraphicsError::stale_resource(format!(
                "{label} are stale for the frame's surface information; recreate them from the \
                 frame's surface info"
            )));
        }
        Ok(())
    }

    fn validate_draw_handle_sessions(
        queue_session: u64,
        frame_session: u64,
        mesh_session: u64,
        texture_session: u64,
        pipeline_session: u64,
    ) -> Result<(), GraphicsError> {
        for (label, session) in [
            ("mesh", mesh_session),
            ("texture", texture_session),
            ("pipeline", pipeline_session),
        ] {
            if session != queue_session || session != frame_session {
                return Err(GraphicsError::invalid_request(format!(
                    "{label} belongs to a different graphics session than the queue and frame"
                )));
            }
        }
        Ok(())
    }

    fn validate_scene(
        &self,
        frame_session: u64,
        frame_info: SurfaceInfo,
        draws: &[TexturedSceneDraw<'_>],
        targets: &impl SceneTargets,
    ) -> Result<(), GraphicsError> {
        if draws.is_empty() {
            return Err(GraphicsError::invalid_request(
                "textured scene must contain at least one draw",
            ));
        }
        self.validate_targets(frame_session, frame_info, targets)?;
        for draw in draws {
            Self::validate_draw_handle_sessions(
                self.shared.id,
                frame_session,
                draw.mesh.lease.session,
                draw.texture.lease.session,
                draw.pipeline.lease.session,
            )?;
            if !draw
                .model_view_projection
                .iter()
                .flatten()
                .all(|component| component.is_finite())
            {
                return Err(GraphicsError::invalid_request(
                    "draw transform must contain only finite values",
                ));
            }
        }
        Ok(())
    }

    fn validate_instanced_scene(
        &self,
        frame_session: u64,
        frame_info: SurfaceInfo,
        batches: &[TexturedInstanceBatch<'_>],
        targets: &impl SceneTargets,
    ) -> Result<(), GraphicsError> {
        if batches.is_empty() {
            return Err(GraphicsError::invalid_request(
                "instanced textured scene must contain at least one batch",
            ));
        }
        self.validate_targets(frame_session, frame_info, targets)?;
        for batch in batches {
            if batch.model_view_projections.is_empty() {
                return Err(GraphicsError::invalid_request(
                    "instanced textured scene batches must contain at least one transform",
                ));
            }
            Self::validate_draw_handle_sessions(
                self.shared.id,
                frame_session,
                batch.mesh.lease.session,
                batch.texture.lease.session,
                batch.pipeline.lease.session,
            )?;
            if !batch
                .model_view_projections
                .iter()
                .flatten()
                .flatten()
                .all(|component| component.is_finite())
            {
                return Err(GraphicsError::invalid_request(
                    "instance transforms must contain only finite values",
                ));
            }
        }
        Ok(())
    }
}

/// Presentation owner for one native session.
pub struct Surface<'window> {
    shared: Shared<'window>,
}

impl<'window> Surface<'window> {
    /// Current graphics-owned surface generation.
    ///
    /// # Errors
    ///
    /// Returns an error after session shutdown.
    pub fn info(&self) -> Result<SurfaceInfo, GraphicsError> {
        Ok(session_ref(&self.shared)?.info())
    }

    /// Acquires one owned native frame token for current window metrics.
    ///
    /// Reconfiguration for changed metrics happens inside acquisition: a ready frame always
    /// matches the requested metrics, and its surface information reports the generation that
    /// render targets must match.
    ///
    /// # Errors
    ///
    /// Returns fatal native acquisition, deferred abandonment, validation, or device failures.
    pub fn acquire(
        &mut self,
        metrics: WindowMetrics,
    ) -> Result<FrameAcquire<Frame<'window>>, GraphicsError> {
        reclaim_lazy_resources(&self.shared)?;
        let acquisition = session_mut(&self.shared)?.acquire(metrics)?;
        Ok(acquisition.map_ready(|token| Frame {
            info: token.info(),
            token: Some(token),
            shared: self.shared.clone(),
        }))
    }

    /// Drains presentation feedback reported by the native backend since the previous drain.
    ///
    /// Feedback is diagnostic and never blocks. Undrained samples are kept in a bounded queue, so
    /// skipping this call costs a fixed amount of memory and no correctness. A backend without
    /// native presentation feedback reports [`PresentFeedback::Unsupported`] on every drain so
    /// estimation fallbacks remain observable rather than silent.
    ///
    /// # Errors
    ///
    /// Returns an error after session shutdown.
    pub fn take_present_feedback(&mut self) -> Result<PresentFeedback, GraphicsError> {
        Ok(session_mut(&self.shared)?.take_present_feedback())
    }
}

/// One frame whose presentation the native system reported complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentedFrame {
    index: u64,
    presented_at: Option<Instant>,
}

impl PresentedFrame {
    /// Constructed only by backends with native presentation feedback: Metal drawable presented
    /// handlers and the Vulkan `VK_EXT_present_timing` drain.
    pub(crate) const fn new(index: u64, presented_at: Option<Instant>) -> Self {
        Self {
            index,
            presented_at,
        }
    }

    /// Zero-based position of this frame among the session's presented frames.
    #[must_use]
    pub const fn index(&self) -> u64 {
        self.index
    }

    /// The moment the frame reached the display.
    ///
    /// `None` means the native system reported presentation handling without a display time, such
    /// as while the window is off screen.
    #[must_use]
    pub const fn presented_at(&self) -> Option<Instant> {
        self.presented_at
    }
}

/// Native presentation feedback drained from a [`Surface`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PresentFeedback {
    /// Frames whose native presentation completed since the previous drain, in presentation
    /// order. The list is empty when no new completions have been reported yet.
    Reported(Vec<PresentedFrame>),
    /// This session's backend exposes no native presentation feedback; cadence must be estimated.
    Unsupported,
}

/// One backend-defined region in a submitted frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GpuTimingScope {
    /// Complete GPU command-buffer work submitted for the frame.
    Frame,
    /// Optional depth-only shadow work.
    Shadow,
    /// Main scene rendering.
    Scene,
    /// Fullscreen post-processing and any overlay encoded into that pass.
    Postprocess,
}

/// Duration of one GPU diagnostic region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GpuScopeTiming {
    scope: GpuTimingScope,
    duration: Duration,
}

impl GpuScopeTiming {
    pub(crate) const fn new(scope: GpuTimingScope, duration: Duration) -> Self {
        Self { scope, duration }
    }

    /// Region measured by this sample.
    #[must_use]
    pub const fn scope(&self) -> GpuTimingScope {
        self.scope
    }

    /// Elapsed time in the backend's GPU timestamp domain.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }
}

/// Completed GPU duration data for one submitted and presented frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GpuFrameTiming {
    frame_index: u64,
    scopes: Vec<GpuScopeTiming>,
}

impl GpuFrameTiming {
    pub(crate) const fn new(frame_index: u64, scopes: Vec<GpuScopeTiming>) -> Self {
        Self {
            frame_index,
            scopes,
        }
    }

    /// Zero-based session index shared with [`PresentedFrame::index`].
    #[must_use]
    pub const fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Backend-supported regions in recording order.
    ///
    /// Metal currently reports only [`GpuTimingScope::Frame`]. Vulkan reports the complete frame
    /// plus the fixed shadow, scene, and postprocess regions that were present in the submission.
    #[must_use]
    pub fn scopes(&self) -> &[GpuScopeTiming] {
        &self.scopes
    }
}

/// GPU duration feedback drained from a [`Queue`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GpuTimingFeedback {
    /// Completed samples in submission order; empty when no new sample is ready.
    Reported(Vec<GpuFrameTiming>),
    /// Collection was not requested when the graphics session was opened.
    Disabled,
    /// Collection was requested, but the selected queue exposes no usable timestamp facility.
    Unsupported,
}

/// Fixed vertex layout for the first textured slice.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    /// Object-space position.
    pub position: [f32; 3],
    /// Linear vertex color multiplier.
    pub color: [f32; 3],
    /// Texture coordinate.
    pub uv: [f32; 2],
}

/// Data format of one vertex attribute: 32-bit float, unsigned, and signed families as scalars
/// through four components, matching what `mulciber-shader` records for vertex-stage inputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum VertexFormat {
    /// One 32-bit float (`f32`).
    Float32,
    /// Two 32-bit floats (`vec2<f32>`).
    Float32x2,
    /// Three 32-bit floats (`vec3<f32>`).
    Float32x3,
    /// Four 32-bit floats (`vec4<f32>`).
    Float32x4,
    /// One unsigned 32-bit integer (`u32`).
    Uint32,
    /// Two unsigned 32-bit integers (`vec2<u32>`).
    Uint32x2,
    /// Three unsigned 32-bit integers (`vec3<u32>`).
    Uint32x3,
    /// Four unsigned 32-bit integers (`vec4<u32>`).
    Uint32x4,
    /// One signed 32-bit integer (`i32`).
    Sint32,
    /// Two signed 32-bit integers (`vec2<i32>`).
    Sint32x2,
    /// Three signed 32-bit integers (`vec3<i32>`).
    Sint32x3,
    /// Four signed 32-bit integers (`vec4<i32>`).
    Sint32x4,
}

impl VertexFormat {
    /// Tightly packed byte size of one attribute value.
    #[must_use]
    pub const fn byte_len(self) -> u32 {
        4 * (self.components() as u32)
    }

    const fn components(self) -> u8 {
        match self {
            Self::Float32 | Self::Uint32 | Self::Sint32 => 1,
            Self::Float32x2 | Self::Uint32x2 | Self::Sint32x2 => 2,
            Self::Float32x3 | Self::Uint32x3 | Self::Sint32x3 => 3,
            Self::Float32x4 | Self::Uint32x4 | Self::Sint32x4 => 4,
        }
    }

    /// The `mulciber-shader` interface format code this format satisfies.
    pub(crate) const fn interface_code(self) -> u8 {
        match self {
            Self::Float32 => 0,
            Self::Float32x2 => 1,
            Self::Float32x3 => 2,
            Self::Float32x4 => 3,
            Self::Uint32 => 4,
            Self::Uint32x2 => 5,
            Self::Uint32x3 => 6,
            Self::Uint32x4 => 7,
            Self::Sint32 => 8,
            Self::Sint32x2 => 9,
            Self::Sint32x3 => 10,
            Self::Sint32x4 => 11,
        }
    }

    /// WGSL spelling used in diagnostics.
    pub(crate) const fn wgsl_name(self) -> &'static str {
        match self {
            Self::Float32 => "f32",
            Self::Float32x2 => "vec2<f32>",
            Self::Float32x3 => "vec3<f32>",
            Self::Float32x4 => "vec4<f32>",
            Self::Uint32 => "u32",
            Self::Uint32x2 => "vec2<u32>",
            Self::Uint32x3 => "vec3<u32>",
            Self::Uint32x4 => "vec4<u32>",
            Self::Sint32 => "i32",
            Self::Sint32x2 => "vec2<i32>",
            Self::Sint32x3 => "vec3<i32>",
            Self::Sint32x4 => "vec4<i32>",
        }
    }

    pub(crate) const fn from_interface_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Float32,
            1 => Self::Float32x2,
            2 => Self::Float32x3,
            3 => Self::Float32x4,
            4 => Self::Uint32,
            5 => Self::Uint32x2,
            6 => Self::Uint32x3,
            7 => Self::Uint32x4,
            8 => Self::Sint32,
            9 => Self::Sint32x2,
            10 => Self::Sint32x3,
            11 => Self::Sint32x4,
            _ => return None,
        })
    }
}

/// One attribute inside an application-described vertex layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VertexAttribute {
    /// Shader input location this attribute feeds.
    pub location: u32,
    /// Data format at `offset`.
    pub format: VertexFormat,
    /// Byte offset from the start of one vertex.
    pub offset: u32,
}

/// An application-described per-vertex data layout.
///
/// The layout is declared once at material pipeline creation and once per mesh uploaded from raw
/// vertex bytes; a draw whose mesh and pipeline layouts differ is rejected. Meshes uploaded
/// through the fixed [`Vertex`] path carry [`VertexLayout::VERTEX`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VertexLayout<'attributes> {
    /// Byte distance between consecutive vertices.
    pub stride: u32,
    /// Attributes consumed from each vertex.
    pub attributes: &'attributes [VertexAttribute],
}

impl VertexLayout<'_> {
    /// The fixed layout of [`Vertex`]: position, color, and texture coordinate at locations 0
    /// through 2.
    pub const VERTEX: VertexLayout<'static> = VertexLayout {
        stride: 32,
        attributes: &[
            VertexAttribute {
                location: 0,
                format: VertexFormat::Float32x3,
                offset: 0,
            },
            VertexAttribute {
                location: 1,
                format: VertexFormat::Float32x3,
                offset: 12,
            },
            VertexAttribute {
                location: 2,
                format: VertexFormat::Float32x2,
                offset: 24,
            },
        ],
    };

    fn to_owned_layout(self) -> OwnedVertexLayout {
        let mut attributes: Vec<VertexAttribute> = self.attributes.to_vec();
        attributes.sort_unstable_by_key(|attribute| attribute.location);
        OwnedVertexLayout {
            stride: self.stride,
            attributes,
        }
    }
}

/// A location-sorted owned copy of a declared vertex layout, kept on meshes and material
/// pipelines so submission can check their compatibility.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OwnedVertexLayout {
    pub(crate) stride: u32,
    pub(crate) attributes: Vec<VertexAttribute>,
}

/// Index data for mesh creation.
///
/// `U16` covers meshes whose vertex count fits sixteen bits; `U32` removes that bound for
/// workloads such as chunked or merged geometry.
#[derive(Clone, Copy, Debug)]
pub enum MeshIndices<'indices> {
    /// 16-bit indices.
    U16(&'indices [u16]),
    /// 32-bit indices.
    U32(&'indices [u32]),
}

impl MeshIndices<'_> {
    /// Number of indices.
    #[must_use]
    pub const fn len(&self) -> usize {
        match self {
            Self::U16(indices) => indices.len(),
            Self::U32(indices) => indices.len(),
        }
    }

    /// Whether no indices are present.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn out_of_range(&self, vertex_count: usize) -> bool {
        match *self {
            Self::U16(indices) => indices
                .iter()
                .any(|&index| usize::from(index) >= vertex_count),
            Self::U32(indices) => indices
                .iter()
                .any(|&index| usize::try_from(index).map_or(true, |index| index >= vertex_count)),
        }
    }

    pub(crate) const fn byte_len(&self) -> usize {
        match *self {
            Self::U16(indices) => indices.len() * 2,
            Self::U32(indices) => indices.len() * 4,
        }
    }
}

/// Uploaded indexed geometry.
#[derive(Debug, Eq, PartialEq)]
pub struct Mesh {
    lease: ResourceLease,
    layout: OwnedVertexLayout,
}

impl Mesh {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// Frame-transient indexed geometry supplied inline with one material record.
///
/// The bytes are copied into the session's frame-transient geometry region at submission, so the
/// application rebuilds them freely every frame — HUD text, gauges, debug lines, and other
/// per-frame-authored geometry — without creating or destroying [`Mesh`] resources. The vertex
/// bytes follow the record's pipeline-declared vertex layout; the application owns that layout
/// correctness exactly as it owns uniform memory layout.
#[derive(Clone, Copy)]
pub struct TransientGeometry<'resources> {
    /// Raw vertex bytes, a non-zero multiple of the pipeline's declared layout stride.
    pub vertices: &'resources [u8],
    /// Non-empty indices into the supplied vertices.
    pub indices: MeshIndices<'resources>,
}

/// Geometry supply for one material record.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum GeometrySource<'resources> {
    /// Uploaded geometry whose retained vertex layout must match the pipeline's declaration.
    Mesh(&'resources Mesh),
    /// Frame-transient geometry staged with this submission against the pipeline's declaration.
    Transient(TransientGeometry<'resources>),
}

/// Uploaded RGBA8 sRGB texture.
#[derive(Debug, Eq, PartialEq)]
pub struct Texture {
    lease: ResourceLease,
}

impl Texture {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// Native textured depth-tested graphics pipeline.
#[derive(Debug, Eq, PartialEq)]
pub struct TexturedPipeline {
    lease: ResourceLease,
}

/// Native textured depth-tested graphics pipeline with per-instance matrix input.
#[derive(Debug, Eq, PartialEq)]
pub struct InstancedTexturedPipeline {
    lease: ResourceLease,
}

impl InstancedTexturedPipeline {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

impl TexturedPipeline {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// Single-sample fullscreen pipeline that samples resolved scene color.
#[derive(Debug, Eq, PartialEq)]
pub struct PostprocessPipeline {
    lease: ResourceLease,
}

/// Largest supported material uniform declaration in bytes.
///
/// Material uniform data flows through the session's per-draw uniform region, whose stride caps
/// one declaration at this size.
pub const MATERIAL_UNIFORM_SIZE_LIMIT: u32 = 256;

/// Largest supported read-only storage declaration in bytes.
///
/// Sixty-four kibibytes holds a thousand and twenty-four `mat4x4<f32>` bone matrices, well past
/// any palette the skinned-record slice needs, while keeping the frame-transient storage region
/// bounded.
pub const MATERIAL_STORAGE_SIZE_LIMIT: u32 = 65536;

/// Largest supported frame-transient geometry supply in bytes, vertices and indices combined.
///
/// Four mebibytes stages past a hundred thousand fixed-layout vertices — far beyond any
/// practical per-record HUD or debug overlay — while keeping the frame-transient geometry
/// region bounded, and it caps the index count well inside the native draw-call range.
pub const TRANSIENT_GEOMETRY_SIZE_LIMIT: u32 = 4_194_304;

/// Largest supported per-record instance supply in bytes.
///
/// Four mebibytes carries 65,536 four-by-four float matrices in one record — far past any
/// practical single-submission scatter — while keeping the frame-transient instance region
/// bounded.
pub const INSTANCE_SUPPLY_SIZE_LIMIT: u32 = 4_194_304;

/// Largest supported material binding slot and vertex attribute location.
///
/// The range 0 through 15 fits inside every native binding namespace both backends guarantee,
/// including Metal's sixteen sampler-state slots.
pub const MATERIAL_SLOT_LIMIT: u32 = 15;

/// Largest supported shadow map extent along either axis.
pub const SHADOW_MAP_SIZE_LIMIT: u32 = 8192;

/// Largest supported shadow map array layer count.
///
/// Eight layers covers every practical cascade scheme while keeping the per-frame layered
/// pre-pass bounded; cascade policy itself (split distances, per-cascade matrices, selection)
/// stays application-owned.
pub const SHADOW_MAP_LAYER_LIMIT: u32 = 8;

/// Minification and magnification filtering for one material sampler slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamplerFilter {
    /// Nearest-texel sampling, keeping texel edges crisp (pixel art, texture atlases).
    Nearest,
    /// Linear interpolation between adjacent texels.
    Linear,
}

/// Texture-coordinate addressing for one material sampler slot, applied on both axes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamplerAddress {
    /// Coordinates wrap, tiling the texture.
    Repeat,
    /// Coordinates clamp to the edge texel.
    ClampToEdge,
}

/// How a material pipeline's fragment output combines with the color target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlendMode {
    /// Fragment color replaces the target color; fragment alpha is ignored.
    Opaque,
    /// Fragment alpha drives multisample coverage (alpha-to-coverage), keeping depth writes
    /// order-independent for hard-edged transparency such as foliage cutouts. At one sample
    /// this degrades to a hard alpha threshold.
    Cutout,
    /// Premultiplied source-over blending: `target = source + (1 - source.a) * target`.
    ///
    /// Translucent records blend against whatever the target already holds, so the application
    /// orders them after the opaque records they should composite over.
    PremultipliedTranslucent,
}

/// How a material pipeline interacts with the scene depth target.
///
/// The testing modes come in two compare directions. `TestWrite` and `TestOnly` use the
/// conventional less-than compare against a depth target cleared to the far plane (1.0).
/// `TestWriteGreater` and `TestOnlyGreater` use a greater-than compare against a depth target
/// cleared to 0.0, for reversed-Z projections that map the near plane to depth 1.0 — the
/// standard fix for far-field precision collapse on the float depth target. The projection
/// matrix that produces reversed-Z clip depth stays application-owned.
///
/// A scene submission derives its depth-clear value from its records' declared modes, so one
/// submission may not mix less-compare and greater-compare testing modes; `Off` composes with
/// either direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DepthMode {
    /// Test against the depth target and write surviving fragment depth (opaque geometry).
    TestWrite,
    /// Test against the depth target without writing (translucents occluded by opaque geometry).
    TestOnly,
    /// Test with a greater-than compare and write surviving fragment depth (opaque geometry
    /// under a reversed-Z projection).
    TestWriteGreater,
    /// Test with a greater-than compare without writing (translucents under a reversed-Z
    /// projection).
    TestOnlyGreater,
    /// Neither test nor write (skyboxes drawn first, overlays drawn last).
    Off,
}

impl DepthMode {
    /// Whether this mode tests with the conventional less-than compare.
    pub(crate) const fn tests_less(self) -> bool {
        matches!(self, Self::TestWrite | Self::TestOnly)
    }

    /// Whether this mode tests with the reversed-Z greater-than compare.
    pub(crate) const fn tests_greater(self) -> bool {
        matches!(self, Self::TestWriteGreater | Self::TestOnlyGreater)
    }
}

/// One declared sampler slot handed to the native backends.
#[derive(Clone, Copy)]
pub(crate) struct SamplerSlot {
    pub(crate) binding: u32,
    pub(crate) filter: SamplerFilter,
    pub(crate) address: SamplerAddress,
}

/// One resource slot declared by a material pipeline, identified by its WGSL binding number in
/// group 0.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MaterialBinding {
    /// Application-defined uniform data supplied as bytes with each draw record.
    ///
    /// At most one uniform slot may be declared, `size` must match the WGSL struct size recorded
    /// in the shader artifact, and it may not exceed [`MATERIAL_UNIFORM_SIZE_LIMIT`].
    Uniform {
        /// WGSL binding number.
        binding: u32,
        /// Byte length of the uniform data supplied with each record.
        size: u32,
    },
    /// Application-defined read-only storage data (`var<storage, read>`) supplied as bytes with
    /// each draw record, sized by its creation-fixed WGSL type (typically a bone-matrix array).
    ///
    /// At most one storage slot may be declared, `size` must match the WGSL type size recorded
    /// in the shader artifact, and it may not exceed [`MATERIAL_STORAGE_SIZE_LIMIT`].
    Storage {
        /// WGSL binding number.
        binding: u32,
        /// Byte length of the storage data supplied with each record.
        size: u32,
    },
    /// One sampled 2D color texture supplied with each draw record.
    Texture {
        /// WGSL binding number.
        binding: u32,
    },
    /// A pipeline-owned sampler with the declared filter and address modes.
    Sampler {
        /// WGSL binding number.
        binding: u32,
        /// Minification and magnification filtering.
        filter: SamplerFilter,
        /// Texture-coordinate addressing on both axes.
        address: SamplerAddress,
    },
    /// One sampled `texture_depth_2d` supplied per draw record from a [`ShadowMap`].
    ///
    /// At most one depth-texture slot — plain or arrayed — may be declared per material
    /// pipeline.
    DepthTexture {
        /// WGSL binding number.
        binding: u32,
    },
    /// One sampled `texture_depth_2d_array` supplied per draw record from a
    /// [`ShadowMapArray`], typically holding one shadow cascade per layer.
    ///
    /// At most one depth-texture slot — plain or arrayed — may be declared per material
    /// pipeline.
    DepthTextureArray {
        /// WGSL binding number.
        binding: u32,
    },
    /// A pipeline-owned `sampler_comparison` with fixed shadow-recipe state: linear filtering,
    /// clamp-to-edge addressing, and a less-or-equal comparison, so
    /// `textureSampleCompare(map, sampler, uv, reference)` returns one where the reference depth
    /// is at most the stored depth. Depth bias stays application-owned in the authored shader.
    ///
    /// At most one comparison-sampler slot may be declared per material pipeline.
    ComparisonSampler {
        /// WGSL binding number.
        binding: u32,
    },
}

/// Everything needed to create one application-authored material pipeline.
#[derive(Clone, Copy)]
pub struct MaterialPipelineDescriptor<'inputs> {
    /// Offline-compiled shader module containing both entry points.
    pub shader: ShaderArtifact<'inputs>,
    /// Vertex entry point name.
    pub vertex_entry: &'inputs str,
    /// Fragment entry point name.
    pub fragment_entry: &'inputs str,
    /// Per-vertex input layout; together with any instance layout it must match the vertex
    /// entry point's recorded inputs.
    pub vertex_layout: VertexLayout<'inputs>,
    /// Optional per-instance input layout fed from each record's instance supply at
    /// instance-stepping rate.
    ///
    /// A location may appear in the vertex layout or the instance layout but not both, and the
    /// two layouts together must match the vertex entry point's recorded inputs exactly. A
    /// pipeline declaring an instance layout draws each record once per supplied instance
    /// (typically a column-major model or model-view-projection matrix as four `vec4<f32>`
    /// locations), indexed implicitly by the instance-rate attributes.
    pub instance_layout: Option<VertexLayout<'inputs>>,
    /// Declared resource slots; must match the module's recorded bindings.
    pub bindings: &'inputs [MaterialBinding],
    /// How fragment output combines with the color target.
    pub blend: BlendMode,
    /// How the pipeline interacts with the scene depth target.
    pub depth: DepthMode,
}

/// Application-authored material pipeline with declared blend and depth modes.
#[derive(Debug, Eq, PartialEq)]
pub struct MaterialPipeline {
    lease: ResourceLease,
    layout: OwnedVertexLayout,
    /// Zero when no uniform slot is declared.
    uniform_size: u32,
    /// Zero when no storage slot is declared.
    storage_size: u32,
    /// Zero when no instance layout is declared.
    instance_stride: u32,
    texture_count: usize,
    /// The kind of depth-texture slot the pipeline declares, fed per record.
    shadow_slot: Option<ShadowSlotKind>,
    /// Declared depth mode, retained so a scene submission can derive its depth-clear value
    /// and reject mixed compare directions.
    depth: DepthMode,
}

/// Which depth-texture slot kind a material pipeline declares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShadowSlotKind {
    /// A `texture_depth_2d` slot fed from a [`ShadowMap`].
    Map,
    /// A `texture_depth_2d_array` slot fed from a [`ShadowMapArray`].
    Array,
}

impl MaterialPipeline {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// A square sampleable depth target rendered by a scene submission's shadow pass.
#[derive(Debug, Eq, PartialEq)]
pub struct ShadowMap {
    lease: ResourceLease,
    size: u32,
}

impl ShadowMap {
    /// Extent of the map along both axes.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.size
    }

    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// A square layered sampleable depth target rendered by a scene submission's cascaded shadow
/// pass, one cascade per layer.
///
/// All layers share one extent; per-cascade fitting happens entirely in the application's
/// light matrices.
#[derive(Debug, Eq, PartialEq)]
pub struct ShadowMapArray {
    lease: ResourceLease,
    size: u32,
    layers: u32,
}

impl ShadowMapArray {
    /// Extent of every layer along both axes.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// Number of layers, each holding one cascade.
    #[must_use]
    pub const fn layers(&self) -> u32 {
        self.layers
    }

    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// The rendered depth resource feeding a material record's declared depth-texture slot.
#[derive(Clone, Copy)]
pub enum ShadowSource<'resources> {
    /// A single square map for a pipeline declaring [`MaterialBinding::DepthTexture`].
    Map(&'resources ShadowMap),
    /// A layered map for a pipeline declaring [`MaterialBinding::DepthTextureArray`].
    Array(&'resources ShadowMapArray),
}

/// Everything needed to create one application-authored depth-only shadow pipeline.
#[derive(Clone, Copy)]
pub struct ShadowPipelineDescriptor<'inputs> {
    /// Offline-compiled shader module containing the vertex entry point.
    pub shader: ShaderArtifact<'inputs>,
    /// Vertex entry point name.
    pub vertex_entry: &'inputs str,
    /// Optional fragment entry point name for casters that carve fragments out of the depth
    /// result, typically an alpha test that `discard`s below a cutout threshold; the pipeline
    /// runs no fragment stage when absent.
    ///
    /// The fragment stage rasterizes into no color target, so its only observable effect is
    /// discarding fragments; texture and sampler bindings become available so the test can
    /// sample the same base-color texture the caster's material pass uses.
    pub fragment_entry: Option<&'inputs str>,
    /// Per-vertex input layout; together with any instance layout it must cover the vertex
    /// entry point's recorded inputs.
    pub vertex_layout: VertexLayout<'inputs>,
    /// Optional per-instance input layout fed from each record's instance supply at
    /// instance-stepping rate, mirroring the caster's material pipeline.
    pub instance_layout: Option<VertexLayout<'inputs>>,
    /// Declared resource slots; shadow pipelines support at most one uniform slot and one
    /// read-only storage slot, plus texture and sampler slots when a fragment entry point is
    /// declared. The module must record no other bindings.
    pub bindings: &'inputs [MaterialBinding],
}

/// Application-authored depth-only pipeline drawn by a shadow pass.
#[derive(Debug, Eq, PartialEq)]
pub struct ShadowPipeline {
    lease: ResourceLease,
    layout: OwnedVertexLayout,
    /// Zero when no uniform slot is declared.
    uniform_size: u32,
    /// Zero when no storage slot is declared.
    storage_size: u32,
    /// Zero when no instance layout is declared.
    instance_stride: u32,
    texture_count: usize,
}

impl ShadowPipeline {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// One depth-only draw inside a shadow pass.
#[derive(Clone, Copy)]
pub struct ShadowRecord<'resources> {
    /// Shadow pipeline whose declared layout matches the mesh.
    pub pipeline: &'resources ShadowPipeline,
    /// Geometry to render into the shadow map.
    pub mesh: &'resources Mesh,
    /// Uniform data matching the pipeline's declared uniform size (typically the light's
    /// view-projection times the record's model transform); empty when no uniform is declared.
    pub uniform: &'resources [u8],
    /// Read-only storage data matching the pipeline's declared storage size (typically the same
    /// bone-matrix palette as the caster's material record); empty when no storage is declared.
    pub storage: &'resources [u8],
    /// Textures for the pipeline's declared texture slots in ascending binding order, typically
    /// the same base-color texture the caster's material record samples for its alpha test;
    /// empty when the pipeline declares no texture slots.
    pub textures: &'resources [&'resources Texture],
    /// Per-instance data laid out per the pipeline's declared instance layout, mirroring the
    /// caster's material record: a non-empty multiple of the instance stride when the pipeline
    /// declares an instance layout, and empty when it declares none. Bounded by
    /// [`INSTANCE_SUPPLY_SIZE_LIMIT`].
    pub instances: &'resources [u8],
}

/// One depth-only pre-pass rendered into a shadow map before the scene pass samples it.
#[derive(Clone, Copy)]
pub struct ShadowPass<'resources> {
    /// Destination map, cleared to the far plane before the first record.
    pub map: &'resources ShadowMap,
    /// Non-empty depth-only records encoded in slice order.
    pub records: &'resources [ShadowRecord<'resources>],
}

/// One depth-only pre-pass per cascade layer, rendered into a shadow map array before the
/// scene pass samples it.
///
/// Each cascade renders with its own record list because every cascade carries its own light
/// matrix in its record uniforms, and the application may cull casters per cascade.
#[derive(Clone, Copy)]
pub struct CascadedShadowPass<'resources> {
    /// Destination layered map; every layer is cleared to the far plane before its records.
    pub map: &'resources ShadowMapArray,
    /// One depth-only record list per layer in layer order; the list count must equal the
    /// map's layer count. A cascade with no records still clears its layer, leaving that
    /// cascade fully lit.
    pub cascades: &'resources [&'resources [ShadowRecord<'resources>]],
}

/// Depth-only pre-pass work submitted ahead of one material scene pass.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum ShadowPrepass<'resources> {
    /// One pass into a single square map.
    Single(ShadowPass<'resources>),
    /// One pass per cascade layer into a layered map.
    Cascaded(CascadedShadowPass<'resources>),
}

impl<'resources> ShadowPrepass<'resources> {
    /// Every record in encode order: the single pass's list, or each cascade's list in layer
    /// order.
    pub(crate) fn records(&self) -> impl Iterator<Item = &'resources ShadowRecord<'resources>> {
        let (single, cascaded): (
            &'resources [ShadowRecord<'resources>],
            &'resources [&'resources [ShadowRecord<'resources>]],
        ) = match *self {
            Self::Single(pass) => (pass.records, &[]),
            Self::Cascaded(pass) => (&[], pass.cascades),
        };
        single.iter().chain(cascaded.iter().copied().flatten())
    }
}

/// Validated creation inputs handed to the native backends.
pub(crate) struct MaterialPipelineConfig<'inputs> {
    pub(crate) vertex_entry: &'inputs str,
    pub(crate) fragment_entry: &'inputs str,
    pub(crate) stride: u32,
    pub(crate) attributes: &'inputs [VertexAttribute],
    /// Declared per-instance stride in bytes; zero when no instance layout is declared.
    pub(crate) instance_stride: u32,
    /// Declared instance-rate attributes; empty when no instance layout is declared.
    pub(crate) instance_attributes: &'inputs [VertexAttribute],
    /// Declared uniform slot as (binding, size).
    pub(crate) uniform: Option<(u32, u32)>,
    /// Declared read-only storage slot as (binding, size).
    pub(crate) storage: Option<(u32, u32)>,
    /// Declared texture binding numbers in ascending order.
    pub(crate) texture_bindings: &'inputs [u32],
    /// Declared sampler slots with their filter and address modes.
    pub(crate) sampler_bindings: &'inputs [SamplerSlot],
    /// Declared depth-texture slot fed from a shadow map per record.
    pub(crate) depth_texture_binding: Option<u32>,
    /// Declared depth-texture-array slot fed from a shadow map array per record.
    pub(crate) depth_texture_array_binding: Option<u32>,
    /// Declared fixed-recipe comparison-sampler slot.
    pub(crate) comparison_sampler_binding: Option<u32>,
    pub(crate) blend: BlendMode,
    pub(crate) depth: DepthMode,
}

/// Validated shadow pipeline creation inputs handed to the native backends.
pub(crate) struct ShadowPipelineConfig<'inputs> {
    pub(crate) vertex_entry: &'inputs str,
    /// Declared fragment entry point for depth-carving casters; absent for the depth-only form.
    pub(crate) fragment_entry: Option<&'inputs str>,
    pub(crate) stride: u32,
    pub(crate) attributes: &'inputs [VertexAttribute],
    /// Declared per-instance stride in bytes; zero when no instance layout is declared.
    pub(crate) instance_stride: u32,
    /// Declared instance-rate attributes consumed by the entry points.
    pub(crate) instance_attributes: &'inputs [VertexAttribute],
    /// Declared uniform slot as (binding, size).
    pub(crate) uniform: Option<(u32, u32)>,
    /// Declared read-only storage slot as (binding, size).
    pub(crate) storage: Option<(u32, u32)>,
    /// Declared texture binding numbers in ascending order.
    pub(crate) texture_bindings: &'inputs [u32],
    /// Declared sampler slots with their filter and address modes.
    pub(crate) sampler_bindings: &'inputs [SamplerSlot],
}

/// Extent- and generation-dependent color/depth targets.
#[derive(Debug, Eq, PartialEq)]
pub struct RenderTargets {
    lease: ResourceLease,
    info: SurfaceInfo,
}

/// Scale applied to the offscreen scene extent of postprocess targets, in percent of the
/// presentable extent.
///
/// The scale is a property of created targets rather than a per-frame toggle: changing it
/// means creating replacement targets, exactly like reacting to a surface reconfiguration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderScale {
    percent: u32,
}

impl RenderScale {
    /// Native 1:1 rendering.
    pub const NATIVE: Self = Self { percent: 100 };

    /// Smallest supported scale in percent.
    pub const MIN_PERCENT: u32 = 25;

    /// Largest supported scale in percent; values above one hundred supersample.
    pub const MAX_PERCENT: u32 = 200;

    /// Selects a scale in percent of the presentable extent.
    ///
    /// # Errors
    ///
    /// Returns an error for a value outside [`RenderScale::MIN_PERCENT`] through
    /// [`RenderScale::MAX_PERCENT`].
    pub fn percent(percent: u32) -> Result<Self, GraphicsError> {
        if !(Self::MIN_PERCENT..=Self::MAX_PERCENT).contains(&percent) {
            return Err(GraphicsError::invalid_request(format!(
                "render scale {percent} percent is outside the supported {} through {}",
                Self::MIN_PERCENT,
                Self::MAX_PERCENT
            )));
        }
        Ok(Self { percent })
    }

    /// Value in percent of the presentable extent.
    #[must_use]
    pub const fn as_percent(self) -> u32 {
        self.percent
    }

    /// The offscreen scene extent this scale selects for one presentable extent, flooring
    /// each dimension at one texel.
    pub(crate) fn scene_extent(self, extent: SurfaceExtent) -> SurfaceExtent {
        let scale = |axis: u32| -> u32 {
            let scaled = u64::from(axis) * u64::from(self.percent) / 100;
            u32::try_from(scaled).unwrap_or(u32::MAX).max(1)
        };
        SurfaceExtent::new(scale(extent.width()), scale(extent.height()))
    }
}

/// Extent- and generation-dependent two-pass color/depth targets.
#[derive(Debug, Eq, PartialEq)]
pub struct PostprocessTargets {
    lease: ResourceLease,
    info: SurfaceInfo,
    scale: RenderScale,
}

impl PostprocessTargets {
    /// Surface information these targets were created for.
    ///
    /// Recreate the targets when an acquired frame reports different surface information.
    #[must_use]
    pub const fn info(&self) -> SurfaceInfo {
        self.info
    }

    /// Scale applied to the offscreen scene extent relative to the presentable extent.
    #[must_use]
    pub const fn render_scale(&self) -> RenderScale {
        self.scale
    }
}

trait SceneTargets {
    fn session(&self) -> u64;
    fn info(&self) -> SurfaceInfo;
    fn label(&self) -> &'static str;
}

impl SceneTargets for RenderTargets {
    fn session(&self) -> u64 {
        self.lease.session
    }

    fn info(&self) -> SurfaceInfo {
        self.info
    }

    fn label(&self) -> &'static str {
        ResourceKind::RenderTargets.label()
    }
}

impl SceneTargets for PostprocessTargets {
    fn session(&self) -> u64 {
        self.lease.session
    }

    fn info(&self) -> SurfaceInfo {
        self.info
    }

    fn label(&self) -> &'static str {
        ResourceKind::PostprocessTargets.label()
    }
}

impl RenderTargets {
    /// Surface information these targets were created for.
    ///
    /// Recreate the targets when an acquired frame reports different surface information; a draw
    /// into mismatched targets is rejected.
    #[must_use]
    pub const fn info(&self) -> SurfaceInfo {
        self.info
    }
}

/// Resources and dynamic data for one textured indexed draw.
#[derive(Clone, Copy)]
pub struct TexturedDraw<'resources> {
    /// Geometry to draw.
    pub mesh: &'resources Mesh,
    /// Sampled color texture.
    pub texture: &'resources Texture,
    /// Pipeline compatible with the session's selected sample count and surface format.
    pub pipeline: &'resources TexturedPipeline,
    /// Targets matching the acquired surface generation.
    pub targets: &'resources RenderTargets,
    /// Column-major model-view-projection matrix.
    pub model_view_projection: [[f32; 4]; 4],
    /// Linear clear color.
    pub clear: ClearColor,
}

/// Resources and dynamic data for one object in a textured scene pass.
#[derive(Clone, Copy)]
pub struct TexturedSceneDraw<'resources> {
    /// Geometry to draw.
    pub mesh: &'resources Mesh,
    /// Sampled color texture.
    pub texture: &'resources Texture,
    /// Depth-tested pipeline compatible with the scene targets.
    pub pipeline: &'resources TexturedPipeline,
    /// Column-major model-view-projection matrix for this object.
    pub model_view_projection: [[f32; 4]; 4],
}

/// A sequence of textured objects rendered directly into one presentable frame.
#[derive(Clone, Copy)]
pub struct TexturedScene<'resources> {
    /// Non-empty object sequence, encoded in slice order.
    pub draws: &'resources [TexturedSceneDraw<'resources>],
    /// Targets matching the acquired surface generation.
    pub targets: &'resources RenderTargets,
    /// Linear color used to clear the scene before its first object.
    pub clear: ClearColor,
}

/// One homogeneous instance batch inside a textured scene pass.
#[derive(Clone, Copy)]
pub struct TexturedInstanceBatch<'resources> {
    /// Geometry shared by every instance in this batch.
    pub mesh: &'resources Mesh,
    /// Sampled color texture shared by every instance in this batch.
    pub texture: &'resources Texture,
    /// Instanced depth-tested pipeline compatible with the scene targets.
    pub pipeline: &'resources InstancedTexturedPipeline,
    /// Non-empty column-major model-view-projection matrix sequence in instance order.
    pub model_view_projections: &'resources [[[f32; 4]; 4]],
}

/// One application-authored material draw inside a scene pass.
#[derive(Clone, Copy)]
pub struct MaterialRecord<'resources> {
    /// Material pipeline compatible with the scene targets.
    pub pipeline: &'resources MaterialPipeline,
    /// Geometry supply: an uploaded mesh whose vertex layout matches the pipeline's declared
    /// layout, or frame-transient bytes laid out per that declaration.
    pub geometry: GeometrySource<'resources>,
    /// Textures for the pipeline's declared texture slots in ascending binding order.
    pub textures: &'resources [&'resources Texture],
    /// The depth resource feeding the pipeline's depth-texture slot; required exactly when the
    /// pipeline declares one, matching the declared kind (plain map or layered array), and it
    /// must have been rendered by a shadow pass (this frame or earlier).
    pub shadow_map: Option<ShadowSource<'resources>>,
    /// Uniform data matching the pipeline's declared uniform size; empty when the pipeline
    /// declares no uniform slot. The application owns WGSL memory-layout correctness.
    pub uniform: &'resources [u8],
    /// Read-only storage data matching the pipeline's declared storage size (typically a
    /// bone-matrix palette); empty when the pipeline declares no storage slot.
    pub storage: &'resources [u8],
    /// Per-instance data laid out per the pipeline's declared instance layout: a non-empty
    /// multiple of the instance stride when the pipeline declares an instance layout (the
    /// record draws once per instance), and empty when it declares none. Bounded by
    /// [`INSTANCE_SUPPLY_SIZE_LIMIT`]; the bytes are copied into the session's frame-transient
    /// instance region at submission.
    pub instances: &'resources [u8],
}

/// Resources and dynamic data for one offscreen textured draw followed by a fullscreen pass.
#[derive(Clone, Copy)]
pub struct PostprocessedDraw<'resources> {
    /// Geometry to draw into the offscreen scene color.
    pub mesh: &'resources Mesh,
    /// Texture sampled by the scene pass.
    pub texture: &'resources Texture,
    /// Depth-tested scene pipeline compatible with the selected sample count.
    pub scene_pipeline: &'resources TexturedPipeline,
    /// Single-sample fullscreen pipeline that samples the resolved scene color.
    pub postprocess_pipeline: &'resources PostprocessPipeline,
    /// Offscreen, depth, and optional multisample targets matching the acquired frame.
    pub targets: &'resources PostprocessTargets,
    /// Column-major model-view-projection matrix for the scene draw.
    pub model_view_projection: [[f32; 4]; 4],
    /// Linear scene-pass clear color.
    pub clear: ClearColor,
}

/// A sequence of textured objects followed by one fullscreen post-processing pass.
#[derive(Clone, Copy)]
pub struct PostprocessedScene<'resources> {
    /// Non-empty object sequence, encoded in slice order into offscreen scene color.
    pub draws: &'resources [TexturedSceneDraw<'resources>],
    /// Single-sample fullscreen pipeline that samples the resolved scene color.
    pub postprocess_pipeline: &'resources PostprocessPipeline,
    /// Offscreen, depth, and optional multisample targets matching the acquired frame.
    pub targets: &'resources PostprocessTargets,
    /// Linear color used to clear the scene before its first object.
    pub clear: ClearColor,
}

/// One narrow scene recipe accepted by [`Queue::render_and_present`].
#[derive(Clone, Copy)]
pub struct SceneSubmission<'resources> {
    /// Geometry records and their submission grouping.
    pub content: SceneContent<'resources>,
    /// Direct or postprocessed destination for the scene pass.
    pub output: SceneOutput<'resources>,
    /// Optional depth-only pre-pass work — a single map or one pass per cascade layer —
    /// rendered before the scene pass; composes with material content only.
    pub shadow: Option<ShadowPrepass<'resources>>,
    /// Optional non-empty material records drawn into the presentable target after the
    /// fullscreen postprocess draw, at the surface's native extent.
    ///
    /// The overlay keeps record-based text and UI sharp while a below-native [`RenderScale`]
    /// shrinks the scene pass: overlay records never touch the scaled offscreen storage. It
    /// composes with material content and postprocessed output only. The presentable pass
    /// carries no depth target, so every overlay record's pipeline must declare
    /// [`DepthMode::Off`] and no depth-texture slot; painter's order is the record order.
    /// Overlay pipelines rasterize at one sample, so [`BlendMode::Cutout`] degrades to a hard
    /// alpha threshold here.
    pub overlay: Option<&'resources [MaterialRecord<'resources>]>,
    /// Linear color used to clear the scene before its first draw or batch.
    pub clear: ClearColor,
}

/// Geometry content for one [`SceneSubmission`].
#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum SceneContent<'resources> {
    /// Non-empty heterogeneous textured records encoded in slice order.
    Textured(&'resources [TexturedSceneDraw<'resources>]),
    /// Non-empty homogeneous instance batches encoded in slice order.
    Instanced(&'resources [TexturedInstanceBatch<'resources>]),
    /// Non-empty application-authored material records encoded in slice order.
    Material(&'resources [MaterialRecord<'resources>]),
}

/// Output path for one [`SceneSubmission`].
#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum SceneOutput<'resources> {
    /// Render directly into generation-matched presentable targets.
    Direct(&'resources RenderTargets),
    /// Resolve into sampled scene color, then run one fullscreen postprocess pass.
    Postprocessed {
        /// Fullscreen pipeline that samples the resolved scene color.
        pipeline: &'resources PostprocessPipeline,
        /// Generation-matched offscreen, depth, and optional multisample targets.
        targets: &'resources PostprocessTargets,
    },
}

/// One acquired native drawable or swapchain image.
#[must_use = "an acquired frame must be presented or explicitly abandoned"]
pub struct Frame<'window> {
    shared: Shared<'window>,
    token: Option<backend::TexturedFrameToken>,
    info: SurfaceInfo,
}

impl Frame<'_> {
    /// Surface generation owning this frame.
    #[must_use]
    pub const fn surface_info(&self) -> SurfaceInfo {
        self.info
    }

    /// Releases the frame through the backend-specific non-presentation path.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot complete safe abandonment.
    pub fn abandon(mut self) -> Result<FrameDisposition, GraphicsError> {
        let token = self
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.abandon(token)
    }
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        if let Some(token) = self.token.take()
            && let Ok(mut session) = session_mut(&self.shared)
        {
            session.defer_abandon(token);
        }
    }
}

/// Number of levels in a full mip chain from the base extent down to its 1x1 level.
fn full_mip_chain_len(width: u32, height: u32) -> usize {
    let largest = width.max(height);
    usize::try_from(32 - largest.leading_zeros()).expect("level count fits usize")
}

/// Extent of one mip level along one axis, flooring at one texel.
pub(crate) const fn mip_extent(base: u32, level: u32) -> u32 {
    let scaled = base >> level;
    if scaled == 0 { 1 } else { scaled }
}

/// Checks that one mip level's byte count matches its tightly packed RGBA8 extent.
fn validate_mip_level(
    width: u32,
    height: u32,
    level: u32,
    texels: &[u8],
) -> Result<(), GraphicsError> {
    if width == 0 || height == 0 {
        return Err(GraphicsError::invalid_request(
            "texture byte count does not match its dimensions",
        ));
    }
    let level_width = mip_extent(width, level);
    let level_height = mip_extent(height, level);
    let expected = usize::try_from(level_width)
        .ok()
        .and_then(|level_width| {
            usize::try_from(level_height)
                .ok()
                .and_then(|level_height| level_width.checked_mul(level_height))
        })
        .and_then(|texels| texels.checked_mul(4))
        .ok_or_else(|| {
            GraphicsError::invalid_request("texture dimensions overflow address space")
        })?;
    if texels.len() != expected {
        if level == 0 {
            return Err(GraphicsError::invalid_request(
                "texture byte count does not match its dimensions",
            ));
        }
        return Err(GraphicsError::invalid_request(format!(
            "texture mip level {level} supplies {} bytes but its {level_width}x{level_height} \
             extent needs {expected}",
            texels.len()
        )));
    }
    Ok(())
}

/// Derives the scene depth-clear value from the records' declared depth modes.
///
/// Greater-compare (reversed-Z) records select a 0.0 clear and conventional less-compare
/// records select the 1.0 far-plane clear; one depth target cannot serve both conventions,
/// so mixing the directions in a single scene is rejected. A scene of only [`DepthMode::Off`]
/// records keeps the conventional far-plane clear.
fn material_scene_depth_clear(records: &[MaterialRecord<'_>]) -> Result<f32, GraphicsError> {
    let mut less = false;
    let mut greater = false;
    for record in records {
        less |= record.pipeline.depth.tests_less();
        greater |= record.pipeline.depth.tests_greater();
    }
    if less && greater {
        return Err(GraphicsError::invalid_request(
            "material scene mixes less-compare and greater-compare depth modes against one \
             depth target",
        ));
    }
    Ok(if greater { 0.0 } else { 1.0 })
}

/// Checks one record's instance supply against its pipeline's declared instance stride: empty
/// when no instance layout is declared, otherwise a non-empty stride multiple inside the
/// supply limit.
fn validate_instance_supply(
    record_label: &str,
    instances: &[u8],
    instance_stride: u32,
) -> Result<(), GraphicsError> {
    if instance_stride == 0 {
        if !instances.is_empty() {
            return Err(GraphicsError::invalid_request(format!(
                "{record_label} supplies instance bytes but its pipeline declares no instance \
                 layout"
            )));
        }
        return Ok(());
    }
    let stride = usize::try_from(instance_stride).expect("validated stride fits usize");
    if instances.is_empty() || !instances.len().is_multiple_of(stride) {
        return Err(GraphicsError::invalid_request(format!(
            "{record_label}'s instance bytes must be a non-zero multiple of its pipeline's \
             declared {instance_stride}-byte instance stride"
        )));
    }
    if instances.len() > usize::try_from(INSTANCE_SUPPLY_SIZE_LIMIT).expect("u32 limit fits usize")
    {
        return Err(GraphicsError::invalid_request(format!(
            "{record_label}'s instance supply exceeds the {INSTANCE_SUPPLY_SIZE_LIMIT}-byte \
             supply limit"
        )));
    }
    Ok(())
}

/// Checks stride and attribute fit, and returns the location-sorted owned layout.
fn validate_vertex_layout(layout: VertexLayout<'_>) -> Result<OwnedVertexLayout, GraphicsError> {
    if layout.stride == 0 {
        return Err(GraphicsError::invalid_request(
            "vertex layout stride must be non-zero",
        ));
    }
    let owned = layout.to_owned_layout();
    for window in owned.attributes.windows(2) {
        if window[0].location == window[1].location {
            return Err(GraphicsError::invalid_request(format!(
                "vertex layout declares location {} twice",
                window[0].location
            )));
        }
    }
    for attribute in &owned.attributes {
        if attribute.location > MATERIAL_SLOT_LIMIT {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                format!(
                    "vertex attribute location {} exceeds the supported locations 0 through \
                     {MATERIAL_SLOT_LIMIT}",
                    attribute.location
                ),
            ));
        }
    }
    for attribute in &owned.attributes {
        let end = attribute
            .offset
            .checked_add(attribute.format.byte_len())
            .filter(|&end| end <= layout.stride);
        if end.is_none() {
            return Err(GraphicsError::invalid_request(format!(
                "vertex layout attribute at location {} does not fit inside the {}-byte stride",
                attribute.location, layout.stride
            )));
        }
    }
    Ok(owned)
}

fn find_entry_point<'interface>(
    interface: &'interface shader::ShaderInterface,
    name: &str,
    stage: u8,
    stage_label: &str,
) -> Result<&'interface shader::InterfaceEntryPoint, GraphicsError> {
    interface
        .entry_points
        .iter()
        .find(|entry| entry.stage == stage && entry.name == name)
        .ok_or_else(|| {
            GraphicsError::invalid_request(format!(
                "shader artifact records no {stage_label} entry point named `{name}`"
            ))
        })
}

/// Checks an instance layout's stride and attribute fit, and rejects locations the vertex
/// layout already declares, returning the location-sorted owned layout.
fn validate_instance_layout(
    vertex: &OwnedVertexLayout,
    instance: VertexLayout<'_>,
) -> Result<OwnedVertexLayout, GraphicsError> {
    let owned = validate_vertex_layout(instance)?;
    for attribute in &owned.attributes {
        if vertex
            .attributes
            .iter()
            .any(|declared| declared.location == attribute.location)
        {
            return Err(GraphicsError::invalid_request(format!(
                "instance layout declares location {} that the vertex layout already declares",
                attribute.location
            )));
        }
    }
    Ok(owned)
}

/// Finds the declared attribute feeding one recorded input — in the vertex layout or, when
/// declared, the instance layout — and checks its format, naming the first mismatch.
fn find_declared_attribute<'layouts>(
    layout: &'layouts OwnedVertexLayout,
    instance_layout: Option<&'layouts OwnedVertexLayout>,
    input: shader::InterfaceVertexInput,
    entry_name: &str,
) -> Result<(&'layouts VertexAttribute, bool), GraphicsError> {
    let vertex_attribute = layout
        .attributes
        .iter()
        .find(|attribute| attribute.location == input.location)
        .map(|attribute| (attribute, false));
    let attribute = vertex_attribute.or_else(|| {
        instance_layout.and_then(|instance| {
            instance
                .attributes
                .iter()
                .find(|attribute| attribute.location == input.location)
                .map(|attribute| (attribute, true))
        })
    });
    let Some((attribute, from_instance)) = attribute else {
        return Err(GraphicsError::invalid_request(format!(
            "entry point `{entry_name}` consumes location {} that the declared layouts do not \
             supply",
            input.location
        )));
    };
    if input.format != attribute.format.interface_code() {
        let recorded = VertexFormat::from_interface_code(input.format)
            .map_or("an unsupported format", VertexFormat::wgsl_name);
        return Err(GraphicsError::invalid_request(format!(
            "declared layouts supply location {} as {} but the shader artifact records {}",
            attribute.location,
            attribute.format.wgsl_name(),
            recorded
        )));
    }
    Ok((attribute, from_instance))
}

/// Requires the declared vertex and instance attributes together to match the artifact's
/// recorded vertex inputs exactly, naming the first offending location.
fn validate_layouts_against_entry(
    layout: &OwnedVertexLayout,
    instance_layout: Option<&OwnedVertexLayout>,
    entry: &shader::InterfaceEntryPoint,
) -> Result<(), GraphicsError> {
    for &input in &entry.inputs {
        find_declared_attribute(layout, instance_layout, input, &entry.name)?;
    }
    let declared = layout.attributes.iter().chain(
        instance_layout
            .map(|instance| instance.attributes.as_slice())
            .unwrap_or_default(),
    );
    for attribute in declared {
        if !entry
            .inputs
            .iter()
            .any(|input| input.location == attribute.location)
        {
            return Err(GraphicsError::invalid_request(format!(
                "declared layouts supply location {} that entry point `{}` does not consume",
                attribute.location, entry.name
            )));
        }
    }
    Ok(())
}

/// Requires every recorded vertex input to have a matching declared attribute — extra declared
/// attributes are legal and simply not consumed by the depth-only stage — and returns the
/// consumed vertex-rate and instance-rate subsets for native vertex-input construction.
fn validate_layouts_cover_entry(
    layout: &OwnedVertexLayout,
    instance_layout: Option<&OwnedVertexLayout>,
    entry: &shader::InterfaceEntryPoint,
) -> Result<(Vec<VertexAttribute>, Vec<VertexAttribute>), GraphicsError> {
    let mut consumed = Vec::with_capacity(entry.inputs.len());
    let mut consumed_instance = Vec::new();
    for &input in &entry.inputs {
        let (attribute, from_instance) =
            find_declared_attribute(layout, instance_layout, input, &entry.name)?;
        if from_instance {
            consumed_instance.push(*attribute);
        } else {
            consumed.push(*attribute);
        }
    }
    Ok((consumed, consumed_instance))
}

struct BindingDeclaration {
    uniform: Option<(u32, u32)>,
    texture_bindings: Vec<u32>,
    sampler_bindings: Vec<SamplerSlot>,
    depth_texture: Option<u32>,
    depth_texture_array: Option<u32>,
    comparison_sampler: Option<u32>,
    /// Declared read-only storage slot as (binding, size).
    storage: Option<(u32, u32)>,
}

const fn interface_binding_label(kind: u8) -> &'static str {
    match kind {
        shader::INTERFACE_BINDING_UNIFORM => "uniform data",
        shader::INTERFACE_BINDING_SAMPLED_TEXTURE => "a sampled texture",
        shader::INTERFACE_BINDING_SAMPLER => "a sampler",
        shader::INTERFACE_BINDING_STORAGE => "a storage buffer",
        shader::INTERFACE_BINDING_DEPTH_TEXTURE => "a depth texture",
        shader::INTERFACE_BINDING_COMPARISON_SAMPLER => "a comparison sampler",
        shader::INTERFACE_BINDING_DEPTH_TEXTURE_ARRAY => "a depth texture array",
        _ => "an unsupported resource",
    }
}

/// Requires the declared slots and the artifact's recorded bindings to match exactly, naming the
/// first offending slot, and rejects interface constructs outside the material vocabulary.
#[allow(clippy::too_many_lines)]
fn validate_bindings_against_interface(
    bindings: &[MaterialBinding],
    interface: &shader::ShaderInterface,
) -> Result<BindingDeclaration, GraphicsError> {
    for recorded in &interface.bindings {
        if recorded.group != 0 {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                format!(
                    "shader binding {}:{} is outside group 0, which the material vocabulary does not \
                 support",
                    recorded.group, recorded.binding
                ),
            ));
        }
    }
    let mut declaration = BindingDeclaration {
        uniform: None,
        texture_bindings: Vec::new(),
        sampler_bindings: Vec::new(),
        depth_texture: None,
        depth_texture_array: None,
        comparison_sampler: None,
        storage: None,
    };
    let mut declared: Vec<(u32, u8, u32)> = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let (slot, kind, size) = match *binding {
            MaterialBinding::Uniform { binding, size } => {
                if declaration.uniform.is_some() {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        "material pipelines support at most one uniform slot",
                    ));
                }
                if size == 0 || size > MATERIAL_UNIFORM_SIZE_LIMIT {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        format!(
                            "material uniform slot {binding} declares {size} bytes, outside the \
                         supported 1 through {MATERIAL_UNIFORM_SIZE_LIMIT}"
                        ),
                    ));
                }
                declaration.uniform = Some((binding, size));
                (binding, shader::INTERFACE_BINDING_UNIFORM, size)
            }
            MaterialBinding::Storage { binding, size } => {
                if declaration.storage.is_some() {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        "material pipelines support at most one storage slot",
                    ));
                }
                if size == 0 || size > MATERIAL_STORAGE_SIZE_LIMIT {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        format!(
                            "material storage slot {binding} declares {size} bytes, outside the \
                         supported 1 through {MATERIAL_STORAGE_SIZE_LIMIT}"
                        ),
                    ));
                }
                declaration.storage = Some((binding, size));
                (binding, shader::INTERFACE_BINDING_STORAGE, size)
            }
            MaterialBinding::Texture { binding } => {
                declaration.texture_bindings.push(binding);
                (binding, shader::INTERFACE_BINDING_SAMPLED_TEXTURE, 0)
            }
            MaterialBinding::Sampler {
                binding,
                filter,
                address,
            } => {
                declaration.sampler_bindings.push(SamplerSlot {
                    binding,
                    filter,
                    address,
                });
                (binding, shader::INTERFACE_BINDING_SAMPLER, 0)
            }
            MaterialBinding::DepthTexture { binding } => {
                if declaration.depth_texture.is_some() || declaration.depth_texture_array.is_some()
                {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        "material pipelines support at most one depth-texture slot",
                    ));
                }
                declaration.depth_texture = Some(binding);
                (binding, shader::INTERFACE_BINDING_DEPTH_TEXTURE, 0)
            }
            MaterialBinding::DepthTextureArray { binding } => {
                if declaration.depth_texture.is_some() || declaration.depth_texture_array.is_some()
                {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        "material pipelines support at most one depth-texture slot",
                    ));
                }
                declaration.depth_texture_array = Some(binding);
                (binding, shader::INTERFACE_BINDING_DEPTH_TEXTURE_ARRAY, 0)
            }
            MaterialBinding::ComparisonSampler { binding } => {
                if declaration.comparison_sampler.is_some() {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        "material pipelines support at most one comparison-sampler slot",
                    ));
                }
                declaration.comparison_sampler = Some(binding);
                (binding, shader::INTERFACE_BINDING_COMPARISON_SAMPLER, 0)
            }
        };
        if slot > MATERIAL_SLOT_LIMIT {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                format!(
                    "material binding slot {slot} exceeds the supported slots 0 through \
                     {MATERIAL_SLOT_LIMIT}"
                ),
            ));
        }
        if declared.iter().any(|&(previous, _, _)| previous == slot) {
            return Err(GraphicsError::invalid_request(format!(
                "material bindings declare slot {slot} twice"
            )));
        }
        declared.push((slot, kind, size));
    }
    for &(slot, kind, size) in &declared {
        let Some(recorded) = interface
            .bindings
            .iter()
            .find(|recorded| recorded.binding == slot)
        else {
            return Err(GraphicsError::invalid_request(format!(
                "material bindings declare slot {slot} that the shader artifact does not record"
            )));
        };
        if recorded.kind != kind {
            return Err(GraphicsError::invalid_request(format!(
                "material bindings declare slot {slot} as {} but the shader artifact records {}",
                interface_binding_label(kind),
                interface_binding_label(recorded.kind)
            )));
        }
        if (kind == shader::INTERFACE_BINDING_UNIFORM || kind == shader::INTERFACE_BINDING_STORAGE)
            && recorded.size != size
        {
            return Err(GraphicsError::invalid_request(format!(
                "material {} slot {slot} declares {size} bytes but the shader artifact \
                 records {}",
                if kind == shader::INTERFACE_BINDING_UNIFORM {
                    "uniform"
                } else {
                    "storage"
                },
                recorded.size
            )));
        }
    }
    for recorded in &interface.bindings {
        if !declared
            .iter()
            .any(|&(slot, _, _)| slot == recorded.binding)
        {
            return Err(GraphicsError::invalid_request(format!(
                "the shader artifact records binding slot {} that the material bindings do not \
                 declare",
                recorded.binding
            )));
        }
    }
    declaration.texture_bindings.sort_unstable();
    declaration
        .sampler_bindings
        .sort_unstable_by_key(|slot| slot.binding);
    Ok(declaration)
}

fn session_ref<'a, 'window>(
    shared: &'a Shared<'window>,
) -> Result<core::cell::Ref<'a, backend::TexturedSession<'window>>, GraphicsError> {
    core::cell::Ref::filter_map(shared.inner.borrow(), Option::as_ref)
        .map_err(|_| GraphicsError::lifecycle("graphics session is shut down"))
}

fn session_mut<'a, 'window>(
    shared: &'a Shared<'window>,
) -> Result<core::cell::RefMut<'a, backend::TexturedSession<'window>>, GraphicsError> {
    // Lazy drops deliberately do not run here: resource creation and diagnostic drains may call
    // this several times in one frame. Surface acquisition owns the single bounded reclamation
    // boundary instead.
    core::cell::RefMut::filter_map(shared.inner.borrow_mut(), Option::as_mut)
        .map_err(|_| GraphicsError::lifecycle("graphics session is shut down"))
}

fn reclaim_lazy_resources(shared: &Shared<'_>) -> Result<(), GraphicsError> {
    let pending = shared.drops.take_bounded(LAZY_RECLAIM_BUDGET);
    if pending.is_empty() {
        return Ok(());
    }
    let mut session = match session_mut(shared) {
        Ok(session) => session,
        Err(error) => {
            shared.drops.restore_front(pending);
            return Err(error);
        }
    };
    if let Err(error) = session.reclaim_resources(&pending) {
        shared.drops.restore_front(pending);
        return Err(error);
    }
    Ok(())
}
