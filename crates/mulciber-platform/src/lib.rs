//! Native window, event, display, and lifecycle support for Mulciber.
//!
//! This API is experimental. Its first implementation extracts the `AppKit` ownership and window
//! lifecycle proven by the native Metal probe. Peer Win32, Wayland, and X11 implementations will
//! follow the same game-facing contract without sharing native machinery.

use core::fmt;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
#[doc(hidden)]
pub use macos::integration;
#[cfg(target_os = "macos")]
pub use macos::{Application, SurfaceTarget, Window};

/// A two-dimensional extent in logical window coordinates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LogicalSize {
    width: u32,
    height: u32,
}

impl LogicalSize {
    /// Creates a logical size from its width and height.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Returns the width in logical window coordinates.
    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Returns the height in logical window coordinates.
    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Returns whether either dimension is zero.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// A two-dimensional drawable extent in physical pixels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PhysicalExtent {
    width: u32,
    height: u32,
}

impl PhysicalExtent {
    /// Creates a physical extent from its pixel width and height.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Returns the width in physical pixels.
    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Returns the height in physical pixels.
    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Returns whether either dimension is zero.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Identifies one revision of a window's drawable metrics.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct WindowRevision(u64);

impl WindowRevision {
    // Only the AppKit backend constructs revisions today; peer platform modules will follow.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) const INITIAL: Self = Self(1);

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Returns the monotonically increasing revision number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The current drawable metrics of a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowMetrics {
    extent: PhysicalExtent,
    scale_factor: f64,
    revision: WindowRevision,
}

impl WindowMetrics {
    // Only the AppKit backend constructs metrics today; peer platform modules will follow.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) const fn new(
        extent: PhysicalExtent,
        scale_factor: f64,
        revision: WindowRevision,
    ) -> Self {
        Self {
            extent,
            scale_factor,
            revision,
        }
    }

    /// Returns the drawable extent in physical pixels.
    #[must_use]
    pub const fn extent(self) -> PhysicalExtent {
        self.extent
    }

    /// Returns physical pixels per logical window coordinate.
    #[must_use]
    pub const fn scale_factor(self) -> f64 {
        self.scale_factor
    }

    /// Returns the revision of these window metrics.
    #[must_use]
    pub const fn revision(self) -> WindowRevision {
        self.revision
    }
}

/// Describes a native window before creation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowDescriptor {
    title: String,
    logical_size: LogicalSize,
}

impl WindowDescriptor {
    /// Creates a window descriptor with a logical client extent.
    pub fn new(title: impl Into<String>, logical_size: LogicalSize) -> Self {
        Self {
            title: title.into(),
            logical_size,
        }
    }

    /// Returns the requested title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the requested logical client extent.
    #[must_use]
    pub const fn logical_size(&self) -> LogicalSize {
        self.logical_size
    }
}

/// A window lifecycle event delivered while pumping the native event queue.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum WindowEvent {
    /// The drawable extent or backing scale changed.
    MetricsChanged(WindowMetrics),
    /// Rendering should pause, such as while the window is minimized or fully occluded.
    RenderingSuspended,
    /// A paused window became drawable again.
    RenderingResumed(WindowMetrics),
    /// The window can render a frame with the supplied current metrics.
    RedrawRequested(WindowMetrics),
    /// Native window closure requested termination of this window's loop.
    CloseRequested,
}

/// Whether the native event pump should continue or exit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PumpStatus {
    /// Continue processing the window and rendering future frames.
    Continue,
    /// Exit because the window has closed.
    Exit,
}

/// A platform creation or lifecycle error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformError(String);

impl PlatformError {
    // Only the AppKit backend constructs platform errors today; peer platform modules will follow.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for PlatformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PlatformError {}

#[cfg(test)]
mod tests {
    use super::{LogicalSize, PhysicalExtent, WindowDescriptor, WindowRevision};

    #[test]
    fn empty_extent_requires_a_zero_dimension() {
        assert!(PhysicalExtent::new(0, 9).is_empty());
        assert!(PhysicalExtent::new(7, 0).is_empty());
        assert!(!PhysicalExtent::new(7, 9).is_empty());
    }

    #[test]
    fn window_revisions_are_monotonic() {
        let initial = WindowRevision::INITIAL;
        assert_eq!(initial.get(), 1);
        assert_eq!(initial.next().get(), 2);
    }

    #[test]
    fn window_descriptor_preserves_game_intent() {
        let descriptor = WindowDescriptor::new("Mulciber", LogicalSize::new(960, 540));
        assert_eq!(descriptor.title(), "Mulciber");
        assert_eq!(descriptor.logical_size(), LogicalSize::new(960, 540));
    }
}
