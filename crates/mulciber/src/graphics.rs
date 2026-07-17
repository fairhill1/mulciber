use core::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use mulciber_platform::{SurfaceTarget, WindowMetrics};

use crate::backend;
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
}

impl Clone for Shared<'_> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            inner: Rc::clone(&self.inner),
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
            return Err(GraphicsError::new(
                "graphics session identity space is exhausted",
            ));
        }
        let (session, sample_count) = backend::TexturedSession::new(target, metrics, request)?;
        let shared = Shared {
            id,
            inner: Rc::new(RefCell::new(Some(session))),
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
            return Err(GraphicsError::new(
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
            .ok_or_else(|| GraphicsError::new("graphics session is already shut down"))?;
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
            return Err(GraphicsError::new(
                "mesh vertices and indices must be non-empty",
            ));
        }
        if indices
            .iter()
            .any(|&index| usize::from(index) >= vertices.len())
        {
            return Err(GraphicsError::new("mesh contains an out-of-range index"));
        }
        let id = session_mut(&self.shared)?.create_mesh(vertices, indices)?;
        Ok(Mesh {
            session: self.shared.id,
            id,
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
            .ok_or_else(|| GraphicsError::new("texture dimensions overflow address space"))?;
        if expected == 0 || texels.len() != expected {
            return Err(GraphicsError::new(
                "texture byte count does not match its dimensions",
            ));
        }
        let id = session_mut(&self.shared)?.create_texture(width, height, texels)?;
        Ok(Texture {
            session: self.shared.id,
            id,
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
            session: self.shared.id,
            id,
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
            session: self.shared.id,
            id,
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
            session: self.shared.id,
            id,
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
            session: self.shared.id,
            id,
            info,
        })
    }
}

/// Submission owner for one native session.
pub struct Queue<'window> {
    shared: Shared<'window>,
}

impl Queue<'_> {
    /// Draws one indexed textured mesh, presents the frame, and consumes it.
    ///
    /// # Errors
    ///
    /// Returns an error for mixed-session or stale handles, a non-finite transform, or native
    /// encoding, submission, validation, or presentation failure.
    pub fn draw_textured_and_present(
        &mut self,
        mut frame: Frame<'_>,
        draw: TexturedDraw<'_>,
    ) -> Result<FrameDisposition, GraphicsError> {
        for session in [
            draw.mesh.session,
            draw.texture.session,
            draw.pipeline.session,
            draw.targets.session,
        ] {
            if session != self.shared.id || session != frame.shared.id {
                return Err(GraphicsError::new(
                    "graphics handles belong to different sessions",
                ));
            }
        }
        if draw.targets.info != frame.info {
            return Err(GraphicsError::new(
                "render targets are stale for the acquired frame",
            ));
        }
        if !draw
            .model_view_projection
            .iter()
            .flatten()
            .all(|component| component.is_finite())
        {
            return Err(GraphicsError::new(
                "draw transform must contain only finite values",
            ));
        }
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::new("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_and_present(
            token,
            draw.mesh.id,
            draw.texture.id,
            draw.pipeline.id,
            draw.targets.id,
            draw.model_view_projection,
            draw.clear,
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
        mut frame: Frame<'_>,
        draw: PostprocessedDraw<'_>,
    ) -> Result<FrameDisposition, GraphicsError> {
        for session in [
            draw.mesh.session,
            draw.texture.session,
            draw.scene_pipeline.session,
            draw.postprocess_pipeline.session,
            draw.targets.session,
        ] {
            if session != self.shared.id || session != frame.shared.id {
                return Err(GraphicsError::new(
                    "graphics handles belong to different sessions",
                ));
            }
        }
        if draw.targets.info != frame.info {
            return Err(GraphicsError::new(
                "postprocess targets are stale for the acquired frame",
            ));
        }
        if !draw
            .model_view_projection
            .iter()
            .flatten()
            .all(|component| component.is_finite())
        {
            return Err(GraphicsError::new(
                "draw transform must contain only finite values",
            ));
        }
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::new("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_postprocessed_and_present(
            token,
            draw.mesh.id,
            draw.texture.id,
            draw.scene_pipeline.id,
            draw.postprocess_pipeline.id,
            draw.targets.id,
            draw.model_view_projection,
            draw.clear,
        )
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mesh {
    session: u64,
    id: u32,
}

/// Uploaded RGBA8 sRGB texture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Texture {
    session: u64,
    id: u32,
}

/// Native textured depth-tested graphics pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TexturedPipeline {
    session: u64,
    id: u32,
}

/// Single-sample fullscreen pipeline that samples resolved scene color.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PostprocessPipeline {
    session: u64,
    id: u32,
}

/// Extent- and generation-dependent color/depth targets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderTargets {
    session: u64,
    id: u32,
    info: SurfaceInfo,
}

/// Extent- and generation-dependent two-pass color/depth targets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PostprocessTargets {
    session: u64,
    id: u32,
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
            .ok_or_else(|| GraphicsError::new("frame is already disposed"))?;
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
        .map_err(|_| GraphicsError::new("graphics session is shut down"))
}

fn session_mut<'a, 'window>(
    shared: &'a Shared<'window>,
) -> Result<core::cell::RefMut<'a, backend::TexturedSession<'window>>, GraphicsError> {
    core::cell::RefMut::filter_map(shared.inner.borrow_mut(), Option::as_mut)
        .map_err(|_| GraphicsError::new("graphics session is shut down"))
}
