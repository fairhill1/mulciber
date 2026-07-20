use core::cell::RefCell;
use std::format;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use std::vec::Vec;

use mulciber_platform::{SurfaceTarget, WindowMetrics};

use crate::backend;
use crate::resource::{DestroyRequest, DropQueue, ResourceId, ResourceKind, ResourceLease};
use crate::shader;
use crate::{
    ClearColor, FrameAcquire, FrameDisposition, GraphicsError, GraphicsErrorKind, ShaderArtifact,
    SurfaceInfo,
};

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

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

/// Native choices made while opening graphics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceSelection {
    backend: &'static str,
    sample_count: SampleCount,
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
        let session = surface
            .shared
            .inner
            .borrow_mut()
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("graphics session is already shut down"))?;
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
        validate_layout_against_entry(&layout, vertex_entry)?;
        let declaration = validate_bindings_against_interface(descriptor.bindings, &interface)?;
        let config = MaterialPipelineConfig {
            vertex_entry: descriptor.vertex_entry,
            fragment_entry: descriptor.fragment_entry,
            stride: layout.stride,
            attributes: &layout.attributes,
            uniform: declaration.uniform,
            storage: declaration.storage,
            texture_bindings: &declaration.texture_bindings,
            sampler_bindings: &declaration.sampler_bindings,
            depth_texture_binding: declaration.depth_texture,
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
            texture_count: declaration.texture_bindings.len(),
            samples_shadow: declaration.depth_texture.is_some(),
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

    /// Creates a depth-only pipeline from an application-authored shader module for shadow
    /// passes.
    ///
    /// The pipeline runs the named vertex entry point with no fragment stage into a
    /// [`ShadowMap`]'s depth target, testing and writing depth. Shadow pipelines support at
    /// most one uniform binding and one read-only storage binding (so skinned casters shadow
    /// with the same bone palette as their material records); the module must record no other
    /// bindings.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid layout, a missing vertex entry point, a declaration that
    /// does not match the artifact's recorded interface, a binding outside the uniform and
    /// storage kinds, or native shader loading and pipeline creation failure.
    pub fn create_shadow_pipeline(
        &self,
        descriptor: ShadowPipelineDescriptor<'_>,
    ) -> Result<ShadowPipeline, GraphicsError> {
        let layout = validate_vertex_layout(descriptor.vertex_layout)?;
        let interface = descriptor.shader.parse_interface();
        let vertex_entry = find_entry_point(
            &interface,
            descriptor.vertex_entry,
            shader::INTERFACE_STAGE_VERTEX,
            "vertex",
        )?;
        let consumed = validate_layout_covers_entry(&layout, vertex_entry)?;
        let declaration = validate_bindings_against_interface(descriptor.bindings, &interface)?;
        if !declaration.texture_bindings.is_empty()
            || !declaration.sampler_bindings.is_empty()
            || declaration.depth_texture.is_some()
            || declaration.comparison_sampler.is_some()
        {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                "shadow pipelines support only uniform and storage bindings",
            ));
        }
        let config = ShadowPipelineConfig {
            vertex_entry: descriptor.vertex_entry,
            stride: layout.stride,
            attributes: &consumed,
            uniform: declaration.uniform,
            storage: declaration.storage,
        };
        let id = session_mut(&self.shared)?.create_shadow_pipeline(descriptor.shader, &config)?;
        Ok(ShadowPipeline {
            lease: self.lease(id, ResourceKind::ShadowPipeline),
            layout,
            uniform_size: declaration.uniform.map_or(0, |(_, size)| size),
            storage_size: declaration.storage.map_or(0, |(_, size)| size),
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
    /// generation.
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
        let id = session_mut(&self.shared)?.create_postprocess_targets(info)?;
        Ok(PostprocessTargets {
            lease: self.lease(id, ResourceKind::PostprocessTargets),
            info,
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
        shadow: Option<ShadowPass<'_>>,
        targets: &RenderTargets,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_material_scene(frame.shared.id, frame.info, records, targets)?;
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
        )
    }

    /// Draws an optional depth-only shadow pass, then a non-empty sequence of
    /// application-authored material records into resolved scene color, runs one fullscreen
    /// post-processing pass, presents the frame, and consumes it.
    fn draw_material_scene_postprocessed_and_present(
        &mut self,
        mut frame: Frame<'_>,
        records: &[MaterialRecord<'_>],
        shadow: Option<ShadowPass<'_>>,
        postprocess_pipeline: &PostprocessPipeline,
        targets: &PostprocessTargets,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_material_scene(frame.shared.id, frame.info, records, targets)?;
        self.validate_shadow_pass(frame.shared.id, shadow.as_ref())?;
        session_ref(&self.shared)?.validate_shadow_sampling(records, shadow.as_ref())?;
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
            postprocess_pipeline.lease.id,
            targets.lease.id,
            clear,
        )
    }

    /// Validates a shadow pass's handles, record shape, and uniform/layout agreement.
    fn validate_shadow_pass(
        &self,
        frame_session: u64,
        shadow: Option<&ShadowPass<'_>>,
    ) -> Result<(), GraphicsError> {
        let Some(shadow) = shadow else {
            return Ok(());
        };
        if shadow.records.is_empty() {
            return Err(GraphicsError::invalid_request(
                "shadow pass must contain at least one record",
            ));
        }
        for (label, session) in shadow
            .records
            .iter()
            .flat_map(|record| {
                [
                    ("shadow pipeline", record.pipeline.lease.session),
                    ("mesh", record.mesh.lease.session),
                ]
            })
            .chain([("shadow map", shadow.map.lease.session)])
        {
            if session != self.shared.id || session != frame_session {
                return Err(GraphicsError::invalid_request(format!(
                    "{label} belongs to a different graphics session than the queue and frame"
                )));
            }
        }
        for record in shadow.records {
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
        for record in records {
            let handles = [
                ("material pipeline", record.pipeline.lease.session),
                ("mesh", record.mesh.lease.session),
            ]
            .into_iter()
            .chain(
                record
                    .textures
                    .iter()
                    .map(|texture| ("texture", texture.lease.session)),
            )
            .chain(
                record
                    .shadow_map
                    .iter()
                    .map(|map| ("shadow map", map.lease.session)),
            );
            for (label, session) in handles {
                if session != self.shared.id || session != frame_session {
                    return Err(GraphicsError::invalid_request(format!(
                        "{label} belongs to a different graphics session than the queue and frame"
                    )));
                }
            }
            if record.shadow_map.is_some() != record.pipeline.samples_shadow {
                return Err(GraphicsError::invalid_request(
                    if record.shadow_map.is_some() {
                        "material record supplies a shadow map but its pipeline declares no \
                     depth-texture slot"
                    } else {
                        "material record supplies no shadow map but its pipeline declares a \
                     depth-texture slot"
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
            if record.mesh.layout != record.pipeline.layout {
                return Err(GraphicsError::invalid_request(
                    "material record's mesh vertex layout does not match its pipeline's declared \
                     layout",
                ));
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

/// Largest supported material binding slot and vertex attribute location.
///
/// The range 0 through 15 fits inside every native binding namespace both backends guarantee,
/// including Metal's sixteen sampler-state slots.
pub const MATERIAL_SLOT_LIMIT: u32 = 15;

/// Largest supported shadow map extent along either axis.
pub const SHADOW_MAP_SIZE_LIMIT: u32 = 8192;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DepthMode {
    /// Test against the depth target and write surviving fragment depth (opaque geometry).
    TestWrite,
    /// Test against the depth target without writing (translucents occluded by opaque geometry).
    TestOnly,
    /// Neither test nor write (skyboxes drawn first, overlays drawn last).
    Off,
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
    /// At most one depth-texture slot may be declared per material pipeline.
    DepthTexture {
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
    /// Per-vertex input layout; must match the vertex entry point's recorded inputs.
    pub vertex_layout: VertexLayout<'inputs>,
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
    texture_count: usize,
    /// Whether the pipeline declares a depth-texture slot fed from a shadow map per record.
    samples_shadow: bool,
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

/// Everything needed to create one application-authored depth-only shadow pipeline.
#[derive(Clone, Copy)]
pub struct ShadowPipelineDescriptor<'inputs> {
    /// Offline-compiled shader module containing the vertex entry point.
    pub shader: ShaderArtifact<'inputs>,
    /// Vertex entry point name; the pipeline runs no fragment stage.
    pub vertex_entry: &'inputs str,
    /// Per-vertex input layout; must match the vertex entry point's recorded inputs.
    pub vertex_layout: VertexLayout<'inputs>,
    /// Declared resource slots; shadow pipelines support at most one uniform slot, and the
    /// module must record no other bindings.
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
}

/// One depth-only pre-pass rendered into a shadow map before the scene pass samples it.
#[derive(Clone, Copy)]
pub struct ShadowPass<'resources> {
    /// Destination map, cleared to the far plane before the first record.
    pub map: &'resources ShadowMap,
    /// Non-empty depth-only records encoded in slice order.
    pub records: &'resources [ShadowRecord<'resources>],
}

/// Validated creation inputs handed to the native backends.
pub(crate) struct MaterialPipelineConfig<'inputs> {
    pub(crate) vertex_entry: &'inputs str,
    pub(crate) fragment_entry: &'inputs str,
    pub(crate) stride: u32,
    pub(crate) attributes: &'inputs [VertexAttribute],
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
    /// Declared fixed-recipe comparison-sampler slot.
    pub(crate) comparison_sampler_binding: Option<u32>,
    pub(crate) blend: BlendMode,
    pub(crate) depth: DepthMode,
}

/// Validated shadow pipeline creation inputs handed to the native backends.
pub(crate) struct ShadowPipelineConfig<'inputs> {
    pub(crate) vertex_entry: &'inputs str,
    pub(crate) stride: u32,
    pub(crate) attributes: &'inputs [VertexAttribute],
    /// Declared uniform slot as (binding, size).
    pub(crate) uniform: Option<(u32, u32)>,
    /// Declared read-only storage slot as (binding, size).
    pub(crate) storage: Option<(u32, u32)>,
}

/// Extent- and generation-dependent color/depth targets.
#[derive(Debug, Eq, PartialEq)]
pub struct RenderTargets {
    lease: ResourceLease,
    info: SurfaceInfo,
}

/// Extent- and generation-dependent two-pass color/depth targets.
#[derive(Debug, Eq, PartialEq)]
pub struct PostprocessTargets {
    lease: ResourceLease,
    info: SurfaceInfo,
}

impl PostprocessTargets {
    /// Surface information these targets were created for.
    ///
    /// Recreate the targets when an acquired frame reports different surface information.
    #[must_use]
    pub const fn info(&self) -> SurfaceInfo {
        self.info
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
    /// Geometry whose vertex layout matches the pipeline's declared layout.
    pub mesh: &'resources Mesh,
    /// Textures for the pipeline's declared texture slots in ascending binding order.
    pub textures: &'resources [&'resources Texture],
    /// The map feeding the pipeline's depth-texture slot; required exactly when the pipeline
    /// declares one, and it must have been rendered by a shadow pass (this frame or earlier).
    pub shadow_map: Option<&'resources ShadowMap>,
    /// Uniform data matching the pipeline's declared uniform size; empty when the pipeline
    /// declares no uniform slot. The application owns WGSL memory-layout correctness.
    pub uniform: &'resources [u8],
    /// Read-only storage data matching the pipeline's declared storage size (typically a
    /// bone-matrix palette); empty when the pipeline declares no storage slot.
    pub storage: &'resources [u8],
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
    /// Optional depth-only pre-pass rendered into a shadow map before the scene pass; composes
    /// with material content only.
    pub shadow: Option<ShadowPass<'resources>>,
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

/// Requires the declared attributes and the artifact's recorded vertex inputs to match exactly,
/// naming the first offending location.
fn validate_layout_against_entry(
    layout: &OwnedVertexLayout,
    entry: &shader::InterfaceEntryPoint,
) -> Result<(), GraphicsError> {
    for attribute in &layout.attributes {
        let Some(input) = entry
            .inputs
            .iter()
            .find(|input| input.location == attribute.location)
        else {
            return Err(GraphicsError::invalid_request(format!(
                "vertex layout declares location {} that entry point `{}` does not consume",
                attribute.location, entry.name
            )));
        };
        if input.format != attribute.format.interface_code() {
            let recorded = VertexFormat::from_interface_code(input.format)
                .map_or("an unsupported format", VertexFormat::wgsl_name);
            return Err(GraphicsError::invalid_request(format!(
                "vertex layout declares location {} as {} but the shader artifact records {}",
                attribute.location,
                attribute.format.wgsl_name(),
                recorded
            )));
        }
    }
    for input in &entry.inputs {
        if !layout
            .attributes
            .iter()
            .any(|attribute| attribute.location == input.location)
        {
            return Err(GraphicsError::invalid_request(format!(
                "entry point `{}` consumes location {} that the vertex layout does not declare",
                entry.name, input.location
            )));
        }
    }
    Ok(())
}

/// Requires every recorded vertex input to have a matching declared attribute — extra declared
/// attributes are legal and simply not consumed by the depth-only stage — and returns the
/// consumed subset for native vertex-input construction.
fn validate_layout_covers_entry(
    layout: &OwnedVertexLayout,
    entry: &shader::InterfaceEntryPoint,
) -> Result<Vec<VertexAttribute>, GraphicsError> {
    let mut consumed = Vec::with_capacity(entry.inputs.len());
    for input in &entry.inputs {
        let Some(attribute) = layout
            .attributes
            .iter()
            .find(|attribute| attribute.location == input.location)
        else {
            return Err(GraphicsError::invalid_request(format!(
                "entry point `{}` consumes location {} that the vertex layout does not declare",
                entry.name, input.location
            )));
        };
        if input.format != attribute.format.interface_code() {
            let recorded = VertexFormat::from_interface_code(input.format)
                .map_or("an unsupported format", VertexFormat::wgsl_name);
            return Err(GraphicsError::invalid_request(format!(
                "vertex layout declares location {} as {} but the shader artifact records {}",
                attribute.location,
                attribute.format.wgsl_name(),
                recorded
            )));
        }
        consumed.push(*attribute);
    }
    Ok(consumed)
}

struct BindingDeclaration {
    uniform: Option<(u32, u32)>,
    texture_bindings: Vec<u32>,
    sampler_bindings: Vec<SamplerSlot>,
    depth_texture: Option<u32>,
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
                if declaration.depth_texture.is_some() {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        "material pipelines support at most one depth-texture slot",
                    ));
                }
                declaration.depth_texture = Some(binding);
                (binding, shader::INTERFACE_BINDING_DEPTH_TEXTURE, 0)
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
    let pending = shared.drops.take();
    let mut session = core::cell::RefMut::filter_map(shared.inner.borrow_mut(), Option::as_mut)
        .map_err(|_| GraphicsError::lifecycle("graphics session is shut down"))?;
    if let Err(error) = session.reclaim_resources(&pending) {
        shared.drops.restore(pending);
        return Err(error);
    }
    Ok(session)
}
