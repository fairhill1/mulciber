use core::fmt;

use mulciber_platform::{SurfaceTarget, WindowMetrics};

use crate::backend;
use crate::{FrameAcquire, FrameDisposition, SurfaceInfo};

/// A normalized linear RGBA color used by the clear-only extraction slice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClearColor {
    components: [f32; 4],
}

impl ClearColor {
    /// Opaque black.
    pub const BLACK: Self = Self {
        components: [0.0, 0.0, 0.0, 1.0],
    };

    /// Creates an opaque clear color from finite components in the inclusive `0.0..=1.0` range.
    ///
    /// Intended for literal colors: evaluated in const context, an invalid component fails at
    /// compile time instead of forcing a runtime unwrap.
    ///
    /// # Panics
    ///
    /// Panics when a component is not finite or lies outside the inclusive `0.0..=1.0` range.
    #[must_use]
    pub const fn opaque(red: f32, green: f32, blue: f32) -> Self {
        #[allow(clippy::manual_range_contains)]
        const fn normalized(component: f32) -> f32 {
            assert!(
                component >= 0.0 && component <= 1.0,
                "clear color components must be finite and within 0.0..=1.0",
            );
            component
        }
        Self {
            components: [normalized(red), normalized(green), normalized(blue), 1.0],
        }
    }

    /// Creates a clear color when every component is finite and in the inclusive `0.0..=1.0`
    /// range.
    #[must_use]
    pub fn new(red: f32, green: f32, blue: f32, alpha: f32) -> Option<Self> {
        let components = [red, green, blue, alpha];
        components
            .iter()
            .all(|component| component.is_finite() && (0.0..=1.0).contains(component))
            .then_some(Self { components })
    }

    pub(crate) const fn components(self) -> [f32; 4] {
        self.components
    }
}

/// The recovery-relevant category of a [`GraphicsError`].
///
/// Temporary surface unavailability is intentionally represented by [`crate::FrameAcquire`], not
/// by this enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GraphicsErrorKind {
    /// Application-provided values are invalid and must be corrected before retrying.
    InvalidRequest,
    /// The requested capability is unavailable; choose a fallback or another device.
    Unsupported,
    /// The operation is invalid in the owner's current lifecycle state.
    Lifecycle,
    /// A resource or surface-generation handle is no longer valid and must be recreated.
    StaleResource,
    /// Native presentation failed and the surface may need to be recreated.
    SurfaceFailure,
    /// The native device can no longer execute work and the graphics session must be recreated.
    DeviceFailure,
    /// Host or device memory allocation failed; release resources, reduce demand, or terminate.
    OutOfMemory,
    /// Native validation reported an application or Mulciber contract violation.
    Validation,
    /// A native operation failed without stronger recovery evidence.
    NativeFailure,
    /// A Mulciber invariant or identity space failed internally.
    Internal,
}

/// A graphics creation, frame, presentation, validation, or shutdown failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicsError {
    kind: GraphicsErrorKind,
    message: std::string::String,
}

impl GraphicsError {
    pub(crate) fn new(message: impl Into<std::string::String>) -> Self {
        Self::with_kind(GraphicsErrorKind::NativeFailure, message)
    }

    pub(crate) fn with_kind(
        kind: GraphicsErrorKind,
        message: impl Into<std::string::String>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn invalid_request(message: impl Into<std::string::String>) -> Self {
        Self::with_kind(GraphicsErrorKind::InvalidRequest, message)
    }

    pub(crate) fn lifecycle(message: impl Into<std::string::String>) -> Self {
        Self::with_kind(GraphicsErrorKind::Lifecycle, message)
    }

    pub(crate) fn stale_resource(message: impl Into<std::string::String>) -> Self {
        Self::with_kind(GraphicsErrorKind::StaleResource, message)
    }

    pub(crate) fn internal(message: impl Into<std::string::String>) -> Self {
        Self::with_kind(GraphicsErrorKind::Internal, message)
    }

    /// Returns the recovery-relevant category while preserving native detail in [`Self::message`].
    #[must_use]
    pub const fn kind(&self) -> GraphicsErrorKind {
        self.kind
    }

    /// Returns the contextual diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for GraphicsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GraphicsError {}

/// A deliberately narrow presentation surface for the clear-only Gate 2 experiment.
///
/// This type temporarily keeps native device, queue, command, and presentation objects behind one
/// owner. The textured slice will decide whether those objects earn separate public types.
pub struct ClearSurface<'window> {
    inner: backend::ClearSurface<'window>,
}

impl<'window> ClearSurface<'window> {
    /// Opens the target-selected native graphics backend for a window surface.
    ///
    /// # Errors
    ///
    /// Returns an error when the initial metrics are empty, native validation is unavailable, no
    /// compatible device can present to the target, or presentation setup fails.
    pub fn new(
        target: SurfaceTarget<'window>,
        initial_metrics: WindowMetrics,
    ) -> Result<Self, GraphicsError> {
        backend::ClearSurface::new(target, initial_metrics).map(|inner| Self { inner })
    }

    /// Returns the current graphics-owned presentation generation.
    #[must_use]
    pub fn info(&self) -> SurfaceInfo {
        self.inner.info()
    }

    /// Acquires a scoped frame for the supplied current window metrics.
    ///
    /// Reconfiguration for changed metrics happens inside acquisition, so a ready frame always
    /// matches the supplied metrics.
    ///
    /// # Errors
    ///
    /// Returns an error for fatal native acquisition, deferred abandonment, validation, or device
    /// failures. Temporary unavailability is a nonfatal outcome.
    pub fn acquire(
        &mut self,
        metrics: WindowMetrics,
    ) -> Result<FrameAcquire<ClearFrame<'_, 'window>>, GraphicsError> {
        self.inner
            .acquire(metrics)
            .map(|acquisition| acquisition.map_ready(|inner| ClearFrame { inner }))
    }

    /// Drains presentation and GPU ownership and destroys the native graphics surface.
    ///
    /// # Errors
    ///
    /// Returns the first native completion, validation, or destruction failure observed while all
    /// remaining owned work is still given a cleanup attempt.
    pub fn shutdown(self) -> Result<(), GraphicsError> {
        self.inner.shutdown()
    }
}

/// One scoped presentable frame acquired from a [`ClearSurface`].
#[must_use = "an acquired frame must be presented or explicitly abandoned"]
pub struct ClearFrame<'surface, 'window> {
    inner: backend::ClearFrame<'surface, 'window>,
}

impl ClearFrame<'_, '_> {
    /// Returns the surface information whose generation owns this frame.
    #[must_use]
    pub fn surface_info(&self) -> SurfaceInfo {
        self.inner.surface_info()
    }

    /// Encodes a full-surface clear, submits it, and queues this frame for presentation.
    ///
    /// # Errors
    ///
    /// Returns an error when command encoding, submission, presentation, or native validation fails.
    pub fn clear_and_present(self, color: ClearColor) -> Result<FrameDisposition, GraphicsError> {
        self.inner.clear_and_present(color)
    }

    /// Safely releases this frame without submission or presentation.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot complete its native non-presentation path. Vulkan
    /// may replace the complete surface generation while Metal releases the drawable.
    pub fn abandon(self) -> Result<FrameDisposition, GraphicsError> {
        self.inner.abandon()
    }
}

#[cfg(test)]
mod tests {
    use std::string::ToString;

    use super::{ClearColor, GraphicsError, GraphicsErrorKind};

    #[test]
    fn graphics_errors_preserve_kind_and_message() {
        let error = GraphicsError::invalid_request("bad graphics request");
        assert_eq!(error.kind(), GraphicsErrorKind::InvalidRequest);
        assert_eq!(error.message(), "bad graphics request");
        assert_eq!(error.to_string(), "bad graphics request");
    }

    #[test]
    fn clear_color_requires_finite_normalized_components() {
        assert!(ClearColor::new(0.0, 0.5, 1.0, 1.0).is_some());
        assert!(ClearColor::new(-0.01, 0.5, 1.0, 1.0).is_none());
        assert!(ClearColor::new(0.0, 1.01, 1.0, 1.0).is_none());
        assert!(ClearColor::new(0.0, f32::NAN, 1.0, 1.0).is_none());
    }

    #[test]
    fn opaque_constructor_matches_the_fallible_path() {
        const COLOR: ClearColor = ClearColor::opaque(0.2, 0.4, 0.6);
        assert_eq!(Some(COLOR), ClearColor::new(0.2, 0.4, 0.6, 1.0));
    }

    #[test]
    #[should_panic(expected = "clear color components must be finite")]
    fn opaque_constructor_rejects_out_of_range_components() {
        let _ = ClearColor::opaque(1.5, 0.0, 0.0);
    }
}
