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
        indices: &[u16],
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
        if indices
            .iter()
            .any(|&index| usize::from(index) >= vertex_count)
        {
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
        let expected = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|texels| texels.checked_mul(4))
            .ok_or_else(|| {
                GraphicsError::invalid_request("texture dimensions overflow address space")
            })?;
        if expected == 0 || texels.len() != expected {
            return Err(GraphicsError::invalid_request(
                "texture byte count does not match its dimensions",
            ));
        }
        let id = session_mut(&self.shared)?.create_texture(width, height, texels)?;
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
    /// session's selected sample count, opaque color output, and the standard depth test.
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
            texture_bindings: &declaration.texture_bindings,
            sampler_bindings: &declaration.sampler_bindings,
        };
        let id = session_mut(&self.shared)?.create_material_pipeline(descriptor.shader, &config)?;
        Ok(MaterialPipeline {
            lease: self.lease(id, ResourceKind::MaterialPipeline),
            layout,
            uniform_size: declaration.uniform.map_or(0, |(_, size)| size),
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
            (SceneContent::Material(records), SceneOutput::Direct(targets)) => {
                self.draw_material_scene_and_present(frame, records, targets, submission.clear)
            }
            (SceneContent::Material(records), SceneOutput::Postprocessed { pipeline, targets }) => {
                self.draw_material_scene_postprocessed_and_present(
                    frame,
                    records,
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

    /// Draws a non-empty sequence of application-authored material records in one depth-tested
    /// render pass, presents the frame, and consumes it.
    fn draw_material_scene_and_present(
        &mut self,
        mut frame: Frame<'_>,
        records: &[MaterialRecord<'_>],
        targets: &RenderTargets,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_material_scene(frame.shared.id, frame.info, records, targets)?;
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_material_scene_and_present(
            token,
            records,
            targets.lease.id,
            clear,
        )
    }

    /// Draws a non-empty sequence of application-authored material records into resolved scene
    /// color, runs one fullscreen post-processing pass, presents the frame, and consumes it.
    fn draw_material_scene_postprocessed_and_present(
        &mut self,
        mut frame: Frame<'_>,
        records: &[MaterialRecord<'_>],
        postprocess_pipeline: &PostprocessPipeline,
        targets: &PostprocessTargets,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_material_scene(frame.shared.id, frame.info, records, targets)?;
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
            postprocess_pipeline.lease.id,
            targets.lease.id,
            clear,
        )
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
            );
            for (label, session) in handles {
                if session != self.shared.id || session != frame_session {
                    return Err(GraphicsError::invalid_request(format!(
                        "{label} belongs to a different graphics session than the queue and frame"
                    )));
                }
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

/// Largest supported material binding slot and vertex attribute location.
///
/// The range 0 through 15 fits inside every native binding namespace both backends guarantee,
/// including Metal's sixteen sampler-state slots.
pub const MATERIAL_SLOT_LIMIT: u32 = 15;

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
    /// One sampled 2D color texture supplied with each draw record.
    Texture {
        /// WGSL binding number.
        binding: u32,
    },
    /// A crate-owned linear repeat sampler.
    Sampler {
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
}

/// Depth-tested application-authored material pipeline.
#[derive(Debug, Eq, PartialEq)]
pub struct MaterialPipeline {
    lease: ResourceLease,
    layout: OwnedVertexLayout,
    /// Zero when no uniform slot is declared.
    uniform_size: u32,
    texture_count: usize,
}

impl MaterialPipeline {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// Validated creation inputs handed to the native backends.
pub(crate) struct MaterialPipelineConfig<'inputs> {
    pub(crate) vertex_entry: &'inputs str,
    pub(crate) fragment_entry: &'inputs str,
    pub(crate) stride: u32,
    pub(crate) attributes: &'inputs [VertexAttribute],
    /// Declared uniform slot as (binding, size).
    pub(crate) uniform: Option<(u32, u32)>,
    /// Declared texture binding numbers in ascending order.
    pub(crate) texture_bindings: &'inputs [u32],
    /// Declared sampler binding numbers.
    pub(crate) sampler_bindings: &'inputs [u32],
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
    /// Uniform data matching the pipeline's declared uniform size; empty when the pipeline
    /// declares no uniform slot. The application owns WGSL memory-layout correctness.
    pub uniform: &'resources [u8],
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

struct BindingDeclaration {
    uniform: Option<(u32, u32)>,
    texture_bindings: Vec<u32>,
    sampler_bindings: Vec<u32>,
}

const fn interface_binding_label(kind: u8) -> &'static str {
    match kind {
        shader::INTERFACE_BINDING_UNIFORM => "uniform data",
        shader::INTERFACE_BINDING_SAMPLED_TEXTURE => "a sampled texture",
        shader::INTERFACE_BINDING_SAMPLER => "a sampler",
        shader::INTERFACE_BINDING_STORAGE => "a storage buffer",
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
        if recorded.kind == shader::INTERFACE_BINDING_STORAGE {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                format!(
                    "shader binding {} is a storage buffer, which the material vocabulary does not \
                 support",
                    recorded.binding
                ),
            ));
        }
    }
    let mut declaration = BindingDeclaration {
        uniform: None,
        texture_bindings: Vec::new(),
        sampler_bindings: Vec::new(),
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
            MaterialBinding::Texture { binding } => {
                declaration.texture_bindings.push(binding);
                (binding, shader::INTERFACE_BINDING_SAMPLED_TEXTURE, 0)
            }
            MaterialBinding::Sampler { binding } => {
                declaration.sampler_bindings.push(binding);
                (binding, shader::INTERFACE_BINDING_SAMPLER, 0)
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
        if kind == shader::INTERFACE_BINDING_UNIFORM && recorded.size != size {
            return Err(GraphicsError::invalid_request(format!(
                "material uniform slot {slot} declares {size} bytes but the shader artifact \
                 records {}",
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
    declaration.sampler_bindings.sort_unstable();
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
