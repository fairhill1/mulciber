use core::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use mulciber_platform::{SurfaceTarget, WindowMetrics};

use crate::backend;
use crate::resource::{DestroyRequest, DropQueue, ResourceId, ResourceKind, ResourceLease};
use crate::{
    ClearColor, FrameAcquire, FrameDisposition, GraphicsError, ShaderArtifact, SurfaceInfo,
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
            return Err(GraphicsError::invalid_request(
                "graphics handles belong to different sessions",
            ));
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
                "graphics handles belong to different sessions",
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
                "graphics handles belong to different sessions",
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
        if targets.session() != self.shared.id
            || targets.session() != frame_session
            || targets.info() != frame_info
        {
            return Err(GraphicsError::stale_resource(
                "graphics handles belong to different sessions or stale surface targets",
            ));
        }
        for draw in draws {
            for session in [
                draw.mesh.lease.session,
                draw.texture.lease.session,
                draw.pipeline.lease.session,
            ] {
                if session != self.shared.id || session != frame_session {
                    return Err(GraphicsError::invalid_request(
                        "graphics handles belong to different sessions",
                    ));
                }
            }
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
        if targets.session() != self.shared.id
            || targets.session() != frame_session
            || targets.info() != frame_info
        {
            return Err(GraphicsError::stale_resource(
                "graphics handles belong to different sessions or stale surface targets",
            ));
        }
        for batch in batches {
            if batch.model_view_projections.is_empty() {
                return Err(GraphicsError::invalid_request(
                    "instanced textured scene batches must contain at least one transform",
                ));
            }
            for session in [
                batch.mesh.lease.session,
                batch.texture.lease.session,
                batch.pipeline.lease.session,
            ] {
                if session != self.shared.id || session != frame_session {
                    return Err(GraphicsError::invalid_request(
                        "graphics handles belong to different sessions",
                    ));
                }
            }
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

/// Uploaded indexed geometry.
#[derive(Debug, Eq, PartialEq)]
pub struct Mesh {
    lease: ResourceLease,
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
}

impl SceneTargets for RenderTargets {
    fn session(&self) -> u64 {
        self.lease.session
    }

    fn info(&self) -> SurfaceInfo {
        self.info
    }
}

impl SceneTargets for PostprocessTargets {
    fn session(&self) -> u64 {
        self.lease.session
    }

    fn info(&self) -> SurfaceInfo {
        self.info
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
