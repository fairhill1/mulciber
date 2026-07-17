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

/// A graphics creation, frame, presentation, validation, or shutdown failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicsError(pub(crate) std::string::String);

impl GraphicsError {
    pub(crate) fn new(message: impl Into<std::string::String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for GraphicsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
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
    use super::ClearColor;

    #[test]
    fn clear_color_requires_finite_normalized_components() {
        assert!(ClearColor::new(0.0, 0.5, 1.0, 1.0).is_some());
        assert!(ClearColor::new(-0.01, 0.5, 1.0, 1.0).is_none());
        assert!(ClearColor::new(0.0, 1.01, 1.0, 1.0).is_none());
        assert!(ClearColor::new(0.0, f32::NAN, 1.0, 1.0).is_none());
    }
}
