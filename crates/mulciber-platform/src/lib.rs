//! Native window, event, display, and lifecycle support for Mulciber.
//!
//! This API is experimental. Its native `AppKit`, Win32, Wayland, and X11 implementations preserve
//! their backend-specific ownership and lifecycle machinery behind the same game-facing contract.

use core::fmt;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
#[doc(hidden)]
pub use macos::integration;
#[cfg(target_os = "macos")]
pub use macos::{Application, SurfaceTarget, Window};

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
#[doc(hidden)]
pub use linux::integration;
#[cfg(target_os = "linux")]
pub use linux::{Application, SurfaceTarget, Window};

#[cfg(target_os = "windows")]
mod win32;

#[cfg(target_os = "windows")]
#[doc(hidden)]
pub use win32::integration;
#[cfg(target_os = "windows")]
pub use win32::{Application, SurfaceTarget, Window};

/// A two-dimensional extent in logical window coordinates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LogicalSize {
    width: u32,
    height: u32,
}

/// A position in logical window coordinates with a top-left origin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LogicalPosition {
    x: f64,
    y: f64,
}

impl LogicalPosition {
    /// Creates a logical position.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Returns the horizontal logical coordinate.
    #[must_use]
    pub const fn x(self) -> f64 {
        self.x
    }

    /// Returns the vertical logical coordinate.
    #[must_use]
    pub const fn y(self) -> f64 {
        self.y
    }
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
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
        allow(dead_code)
    )]
    pub(crate) const INITIAL: Self = Self(1);

    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
        allow(dead_code)
    )]
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
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
        allow(dead_code)
    )]
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

/// Whether a keyboard key or pointer button was pressed or released.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonState {
    /// The control became pressed.
    Pressed,
    /// The control became released.
    Released,
}

/// A physical, position-oriented keyboard key.
///
/// Text entry and input-method composition are deliberately separate from this gameplay-oriented
/// identity and are not part of the first input evidence slice.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum KeyCode {
    KeyA,
    KeyB,
    KeyC,
    KeyD,
    KeyE,
    KeyF,
    KeyG,
    KeyH,
    KeyI,
    KeyJ,
    KeyK,
    KeyL,
    KeyM,
    KeyN,
    KeyO,
    KeyP,
    KeyQ,
    KeyR,
    KeyS,
    KeyT,
    KeyU,
    KeyV,
    KeyW,
    KeyX,
    KeyY,
    KeyZ,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Escape,
    Space,
    Enter,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Minus,
    Equal,
    BracketLeft,
    BracketRight,
    Backslash,
    Semicolon,
    Quote,
    Backquote,
    Comma,
    Period,
    Slash,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,
    NumpadDecimal,
    NumpadMultiply,
    NumpadAdd,
    NumpadSubtract,
    NumpadDivide,
    NumpadEnter,
    NumpadEqual,
    NumpadClear,
    /// A physical key whose current backend mapping is not yet represented portably.
    Unidentified(u32),
}

/// The currently active device-independent keyboard modifiers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Modifiers(u8);

impl Modifiers {
    pub(crate) const SHIFT: u8 = 1 << 0;
    pub(crate) const CONTROL: u8 = 1 << 1;
    pub(crate) const ALT: u8 = 1 << 2;
    pub(crate) const SUPER: u8 = 1 << 3;
    pub(crate) const CAPS_LOCK: u8 = 1 << 4;
    pub(crate) const FUNCTION: u8 = 1 << 5;

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Returns whether either Shift key is active.
    #[must_use]
    pub const fn shift(self) -> bool {
        self.0 & Self::SHIFT != 0
    }

    /// Returns whether either Control key is active.
    #[must_use]
    pub const fn control(self) -> bool {
        self.0 & Self::CONTROL != 0
    }

    /// Returns whether either Alt/Option key is active.
    #[must_use]
    pub const fn alt(self) -> bool {
        self.0 & Self::ALT != 0
    }

    /// Returns whether either platform Command/Super key is active.
    #[must_use]
    pub const fn super_key(self) -> bool {
        self.0 & Self::SUPER != 0
    }

    /// Returns whether Caps Lock is active.
    #[must_use]
    pub const fn caps_lock(self) -> bool {
        self.0 & Self::CAPS_LOCK != 0
    }

    /// Returns whether the platform function modifier is active.
    #[must_use]
    pub const fn function(self) -> bool {
        self.0 & Self::FUNCTION != 0
    }
}

/// A pointer button identity independent from handedness preferences.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
    Other(u16),
}

/// A scroll delta preserving whether the platform supplied precise or coarse units.
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(missing_docs)]
pub enum ScrollDelta {
    /// Precise logical-coordinate deltas, typically from a trackpad.
    Precise { x: f64, y: f64 },
    /// Coarse wheel-step deltas.
    Coarse { x: f64, y: f64 },
}

/// One ordered gameplay-oriented input transition from the native event queue.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum InputEvent {
    /// The window gained or lost keyboard focus.
    FocusChanged { focused: bool },
    /// A physical key changed state.
    Keyboard {
        key: KeyCode,
        state: ButtonState,
        repeat: bool,
        modifiers: Modifiers,
    },
    /// The aggregate modifier state changed.
    ModifiersChanged(Modifiers),
    /// The pointer moved in logical client coordinates.
    PointerMoved {
        position: LogicalPosition,
        modifiers: Modifiers,
    },
    /// A pointer button changed state.
    PointerButton {
        button: PointerButton,
        state: ButtonState,
        position: LogicalPosition,
        modifiers: Modifiers,
    },
    /// A wheel or trackpad supplied a scroll delta at the pointer position.
    Scroll {
        delta: ScrollDelta,
        position: LogicalPosition,
        modifiers: Modifiers,
    },
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
    /// An ordered input transition associated with this window.
    Input(InputEvent),
}

/// Whether the native event pump should continue or exit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PumpStatus {
    /// Continue processing the window and rendering future frames.
    Continue,
    /// Exit because the window has closed.
    Exit,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
impl Application {
    /// Pumps native events until `window` first reports drawable metrics.
    ///
    /// Every correct application needs drawable metrics before opening graphics, so this wait is
    /// owned here instead of being restated by each application.
    ///
    /// # Errors
    ///
    /// Returns an error when the native event pump fails or when the window closes before it
    /// becomes drawable.
    pub fn wait_for_first_metrics(
        &mut self,
        window: &Window,
    ) -> Result<WindowMetrics, PlatformError> {
        loop {
            if let Some(metrics) = window.rendering_metrics() {
                return Ok(metrics);
            }
            let mut first = None;
            let status = self.pump_events(window, |event| {
                if let WindowEvent::RenderingResumed(metrics)
                | WindowEvent::RedrawRequested(metrics) = event
                {
                    first = Some(metrics);
                }
                Ok::<(), PlatformError>(())
            })?;
            if let Some(metrics) = first {
                return Ok(metrics);
            }
            if status == PumpStatus::Exit {
                return Err(PlatformError::new(
                    "window closed before drawable metrics became available",
                ));
            }
        }
    }
}

/// A platform creation or lifecycle error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformError(String);

impl PlatformError {
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
        allow(dead_code)
    )]
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
