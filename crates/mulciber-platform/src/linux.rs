//! Runtime-selected Linux window ownership with peer Wayland and X11 implementations.

mod wayland;
mod x11;

use std::cell::Cell;
use std::env;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

use crate::{
    PhysicalExtent, PlatformError, PumpStatus, WindowDescriptor, WindowEvent, WindowMetrics,
    WindowRevision,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Platform {
    Wayland,
    X11,
}

/// A Linux display selection and native event pump.
pub struct Application {
    platform: Platform,
    window_slot: WindowSlot,
    _creating_thread: PhantomData<Rc<()>>,
}

impl Application {
    /// Selects Wayland when `WAYLAND_DISPLAY` is set, then X11 when `DISPLAY` is set.
    ///
    /// # Errors
    ///
    /// Returns an error when neither display environment variable names a platform to connect.
    pub fn new() -> Result<Self, PlatformError> {
        let platform = if environment_is_set("WAYLAND_DISPLAY") {
            Platform::Wayland
        } else if environment_is_set("DISPLAY") {
            Platform::X11
        } else {
            return Err(PlatformError::new(
                "no Wayland or X11 display is available; set WAYLAND_DISPLAY or DISPLAY",
            ));
        };
        Ok(Self::for_platform(platform))
    }

    fn for_platform(platform: Platform) -> Self {
        Self {
            platform,
            window_slot: WindowSlot::new(),
            _creating_thread: PhantomData,
        }
    }

    /// Creates and shows one native window.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty extent, while another window is alive, or when the selected
    /// display server cannot create its native window and shell resources.
    pub fn create_window(&self, descriptor: &WindowDescriptor) -> Result<Window, PlatformError> {
        if descriptor.logical_size().is_empty() {
            return Err(PlatformError::new(
                "window creation requires a non-empty logical extent",
            ));
        }
        let window_lease = self.window_slot.claim()?;
        let size = descriptor.logical_size();
        let native = match self.platform {
            Platform::Wayland => {
                wayland::Window::new(descriptor.title(), size.width(), size.height(), true)
                    .map(NativeWindow::Wayland)?
            }
            Platform::X11 => {
                x11::Window::new(descriptor.title(), size.width(), size.height(), true)
                    .map(NativeWindow::X11)?
            }
        };
        let window = Window {
            native,
            revision: Cell::new(WindowRevision::INITIAL),
            last_extent: Cell::new(PhysicalExtent::default()),
            last_metrics: Cell::new(None),
            close_reported: Cell::new(false),
            _window_lease: window_lease,
            _creating_thread: PhantomData,
        };
        window.last_metrics.set(window.current_window_metrics());
        Ok(window)
    }

    /// Dispatches queued display events and reports game-facing lifecycle events for `window`.
    ///
    /// # Errors
    ///
    /// Returns an error when the display connection or native event pump fails.
    pub fn pump_events(
        &mut self,
        window: &Window,
        mut handler: impl FnMut(WindowEvent),
    ) -> Result<PumpStatus, PlatformError> {
        let open = match &window.native {
            NativeWindow::Wayland(native) => native.pump_events()?,
            NativeWindow::X11(native) => native.pump_events()?,
        };
        if !open {
            if !window.close_reported.replace(true) {
                handler(WindowEvent::CloseRequested);
            }
            window.last_metrics.set(None);
            return Ok(PumpStatus::Exit);
        }

        let previous = window.last_metrics.get();
        let current = window.current_window_metrics();
        if let Some(event) = metrics_transition(previous, current) {
            handler(event);
        }
        if let Some(metrics) = current {
            handler(WindowEvent::RedrawRequested(metrics));
        }
        window.last_metrics.set(current);
        Ok(PumpStatus::Continue)
    }
}

enum NativeWindow {
    Wayland(wayland::Window),
    X11(x11::Window),
}

/// An owned Wayland or X11 window confined to its creating thread.
pub struct Window {
    native: NativeWindow,
    revision: Cell<WindowRevision>,
    last_extent: Cell<PhysicalExtent>,
    last_metrics: Cell<Option<WindowMetrics>>,
    close_reported: Cell<bool>,
    _window_lease: WindowLease,
    _creating_thread: PhantomData<Rc<()>>,
}

impl Window {
    /// Returns current drawable metrics.
    ///
    /// Linux scale-factor and display-change evidence is still pending, so these native probes
    /// report the drawable client extent at scale factor `1.0`.
    #[must_use]
    pub fn rendering_metrics(&self) -> Option<WindowMetrics> {
        self.current_window_metrics()
    }

    /// Returns a borrowed opaque target accepted by Mulciber's graphics surface creation.
    #[must_use]
    pub fn surface_target(&self) -> SurfaceTarget<'_> {
        let native = match &self.native {
            NativeWindow::Wayland(window) => NativeSurfaceTarget::Wayland {
                // SAFETY: Wayland window construction rejects null display and surface handles.
                display: unsafe { NonNull::new_unchecked(window.display()) },
                // SAFETY: Wayland window construction rejects null display and surface handles.
                surface: unsafe { NonNull::new_unchecked(window.surface()) },
            },
            NativeWindow::X11(window) => NativeSurfaceTarget::X11 {
                // SAFETY: X11 window construction rejects a null display connection.
                display: unsafe { NonNull::new_unchecked(window.display()) },
                window: window.handle(),
            },
        };
        SurfaceTarget {
            native,
            _window: PhantomData,
        }
    }

    fn current_window_metrics(&self) -> Option<WindowMetrics> {
        let (width, height) = match &self.native {
            NativeWindow::Wayland(window) => window.client_extent(),
            NativeWindow::X11(window) => window.client_extent(),
        };
        let extent = PhysicalExtent::new(width, height);
        if extent.is_empty() {
            return None;
        }
        let revision = if self.last_extent.get() == PhysicalExtent::default() {
            self.revision.get()
        } else if self.last_extent.get() != extent {
            let next = self.revision.get().next();
            self.revision.set(next);
            next
        } else {
            self.revision.get()
        };
        self.last_extent.set(extent);
        Some(WindowMetrics::new(extent, 1.0, revision))
    }
}

/// A borrowed native target whose ownership remains with its [`Window`].
pub struct SurfaceTarget<'window> {
    native: NativeSurfaceTarget,
    _window: PhantomData<&'window Window>,
}

#[derive(Clone, Copy)]
enum NativeSurfaceTarget {
    Wayland {
        display: NonNull<c_void>,
        surface: NonNull<c_void>,
    },
    X11 {
        display: NonNull<c_void>,
        window: u64,
    },
}

struct WindowSlot {
    live: Rc<Cell<bool>>,
}

impl WindowSlot {
    fn new() -> Self {
        Self {
            live: Rc::new(Cell::new(false)),
        }
    }

    fn claim(&self) -> Result<WindowLease, PlatformError> {
        if self.live.get() {
            return Err(PlatformError::new(
                "the initial Linux extraction supports one live window per application",
            ));
        }
        self.live.set(true);
        Ok(WindowLease {
            live: Rc::clone(&self.live),
        })
    }
}

struct WindowLease {
    live: Rc<Cell<bool>>,
}

impl Drop for WindowLease {
    fn drop(&mut self) {
        self.live.set(false);
    }
}

fn metrics_transition(
    previous: Option<WindowMetrics>,
    current: Option<WindowMetrics>,
) -> Option<WindowEvent> {
    match (previous, current) {
        (Some(old), Some(new)) if old.revision() != new.revision() => {
            Some(WindowEvent::MetricsChanged(new))
        }
        (Some(_), None) => Some(WindowEvent::RenderingSuspended),
        (None, Some(metrics)) => Some(WindowEvent::RenderingResumed(metrics)),
        _ => None,
    }
}

fn environment_is_set(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Backend integration details for Mulciber's native Vulkan implementation.
#[doc(hidden)]
pub mod integration {
    use super::{Application, NativeSurfaceTarget, Platform, SurfaceTarget};
    use crate::PlatformError;
    use std::ffi::c_void;
    use std::ptr::NonNull;

    /// An explicit Linux display-server selection used by validation probes.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum LinuxPlatform {
        /// Select the native Wayland path.
        Wayland,
        /// Select the Xlib path.
        X11,
    }

    /// Borrowed native handles used to create the matching Vulkan surface.
    #[derive(Clone, Copy)]
    pub enum LinuxSurfaceTarget {
        /// A live `wl_display` and `wl_surface` pair.
        Wayland {
            /// The native `wl_display` pointer.
            display: NonNull<c_void>,
            /// The native `wl_surface` pointer.
            surface: NonNull<c_void>,
        },
        /// A live Xlib display and window pair.
        X11 {
            /// The native Xlib `Display` pointer.
            display: NonNull<c_void>,
            /// The Xlib `Window` resource identifier.
            window: u64,
        },
    }

    /// Creates an application for an explicit display-server validation path.
    ///
    /// # Errors
    ///
    /// This constructor currently cannot fail; it returns a result to match [`Application::new`]
    /// and allow connection ownership to move into the application in a future evidence-backed
    /// revision without changing the integration call site.
    pub fn application(platform: LinuxPlatform) -> Result<Application, PlatformError> {
        let platform = match platform {
            LinuxPlatform::Wayland => Platform::Wayland,
            LinuxPlatform::X11 => Platform::X11,
        };
        Ok(Application::for_platform(platform))
    }

    /// Exposes native handles while `target` and its source window remain alive.
    ///
    /// # Safety
    ///
    /// The returned handles must not be retained beyond the borrowed target or used with a Vulkan
    /// surface extension that does not match the returned variant.
    #[must_use]
    pub unsafe fn native_surface_target(target: &SurfaceTarget<'_>) -> LinuxSurfaceTarget {
        match target.native {
            NativeSurfaceTarget::Wayland { display, surface } => {
                LinuxSurfaceTarget::Wayland { display, surface }
            }
            NativeSurfaceTarget::X11 { display, window } => {
                LinuxSurfaceTarget::X11 { display, window }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WindowSlot, metrics_transition};
    use crate::{PhysicalExtent, WindowEvent, WindowMetrics, WindowRevision};

    fn metrics(revision: WindowRevision) -> WindowMetrics {
        WindowMetrics::new(PhysicalExtent::new(960, 540), 1.0, revision)
    }

    #[test]
    fn lifecycle_transitions_preserve_revision_and_suspend_semantics() {
        let first = metrics(WindowRevision::INITIAL);
        let resized = metrics(WindowRevision::INITIAL.next());
        assert_eq!(
            metrics_transition(Some(first), Some(resized)),
            Some(WindowEvent::MetricsChanged(resized))
        );
        assert_eq!(
            metrics_transition(Some(resized), None),
            Some(WindowEvent::RenderingSuspended)
        );
        assert_eq!(
            metrics_transition(None, Some(resized)),
            Some(WindowEvent::RenderingResumed(resized))
        );
    }

    #[test]
    fn window_slot_releases_when_the_window_lease_drops() {
        let slot = WindowSlot::new();
        let lease = slot.claim().expect("first window should claim the slot");
        assert!(slot.claim().is_err());
        drop(lease);
        assert!(slot.claim().is_ok());
    }
}
