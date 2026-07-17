//! Vulkan integration adapter over `mulciber-platform`'s peer Wayland and X11 backends.

use std::ffi::CStr;
use std::fmt;
use std::mem;
use std::ptr;
use std::time::Duration;

use mulciber_platform::integration::{
    LinuxPlatform, LinuxSurfaceTarget, application, native_surface_target,
};
use mulciber_platform::{
    Application as PlatformApplication, LogicalSize, PlatformError, PumpStatus,
    Window as PlatformWindow, WindowDescriptor,
};

use crate::vk;

pub(crate) type SurfaceFunction = vk::PFN_vkVoidFunction;
#[derive(Debug)]
pub struct WindowError(String);

impl fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WindowError {}

pub struct Application {
    platform: PlatformApplication,
}

impl Application {
    pub fn new(requested_platform: Option<&str>) -> Result<Self, WindowError> {
        let platform = match requested_platform {
            Some("wayland") => application(LinuxPlatform::Wayland)
                .map_err(|error| WindowError(error.to_string()))?,
            Some("x11") => {
                application(LinuxPlatform::X11).map_err(|error| WindowError(error.to_string()))?
            }
            Some(other) => {
                return Err(WindowError(format!(
                    "unsupported Linux platform {other:?}; expected x11 or wayland"
                )));
            }
            None => PlatformApplication::new().map_err(|error| WindowError(error.to_string()))?,
        };
        Ok(Self { platform })
    }

    pub fn create_window(
        &self,
        title: &str,
        width: u32,
        height: u32,
        visible: bool,
    ) -> Result<Window, WindowError> {
        if !visible {
            return Err(WindowError(
                "the Vulkan probe requires a visible Linux window".into(),
            ));
        }
        let descriptor = WindowDescriptor::new(title, LogicalSize::new(width, height));
        self.platform
            .create_window(&descriptor)
            .map(Window)
            .map_err(|error| WindowError(error.to_string()))
    }

    pub fn pump_events<F>(
        &mut self,
        window: &Window,
        _live_resize: &mut F,
    ) -> Result<bool, WindowError>
    where
        F: FnMut(),
    {
        self.platform
            .pump_events(&window.0, |_| Ok::<(), PlatformError>(()))
            .map(|status| status == PumpStatus::Continue)
            .map_err(|error| WindowError(error.to_string()))
    }
}

pub struct Window(PlatformWindow);

impl Window {
    #[allow(clippy::unnecessary_wraps)]
    pub fn client_extent(&self) -> Result<(u32, u32), WindowError> {
        Ok(self.0.rendering_metrics().map_or((0, 0), |metrics| {
            let extent = metrics.extent();
            (extent.width(), extent.height())
        }))
    }
}

pub(crate) fn surface_extension(window: &Window) -> &'static CStr {
    match native_target(window) {
        LinuxSurfaceTarget::Wayland { .. } => c"VK_KHR_wayland_surface",
        LinuxSurfaceTarget::X11 { .. } => c"VK_KHR_xlib_surface",
    }
}

pub(crate) fn surface_description(window: &Window) -> &'static str {
    match native_target(window) {
        LinuxSurfaceTarget::Wayland { .. } => "Wayland surface extension",
        LinuxSurfaceTarget::X11 { .. } => "Xlib surface extension",
    }
}

pub(crate) fn create_surface_name(window: &Window) -> &'static CStr {
    match native_target(window) {
        LinuxSurfaceTarget::Wayland { .. } => c"vkCreateWaylandSurfaceKHR",
        LinuxSurfaceTarget::X11 { .. } => c"vkCreateXlibSurfaceKHR",
    }
}

pub(crate) fn acquire_timeout(_window: &Window) -> u64 {
    // Both Linux paths acquire without blocking: compositor-driven presentation may withhold
    // images indefinitely, and the render loop already treats VK_NOT_READY as a paced retry.
    0
}

pub(crate) fn resize_commit_interval(window: &Window) -> Duration {
    // Wayland swapchain recreation bypasses FIFO acquisition backpressure and needs frame pacing.
    // X11's `_NET_WM_SYNC_REQUEST` counter already gates each interactive resize step.
    match native_target(window) {
        LinuxSurfaceTarget::Wayland { .. } => Duration::from_millis(16),
        LinuxSurfaceTarget::X11 { .. } => Duration::ZERO,
    }
}

pub(crate) unsafe fn create_surface(
    function: SurfaceFunction,
    instance: vk::VkInstance,
    window: &Window,
    surface: *mut vk::VkSurfaceKHR,
) -> vk::VkResult {
    match native_target(window) {
        LinuxSurfaceTarget::Wayland {
            display,
            surface: wayland_surface,
        } => {
            // SAFETY: The function was loaded by the Wayland surface-creation name.
            let function: vk::PFN_vkCreateWaylandSurfaceKHR = unsafe { cast_function(function) };
            let info = vk::VkWaylandSurfaceCreateInfoKHR {
                sType: vk::VK_STRUCTURE_TYPE_WAYLAND_SURFACE_CREATE_INFO_KHR,
                display: display.as_ptr().cast(),
                surface: wayland_surface.as_ptr().cast(),
                ..Default::default()
            };
            // SAFETY: Native objects and instance are live, and the output pointer is writable.
            unsafe {
                function.expect("loaded function")(instance, &raw const info, ptr::null(), surface)
            }
        }
        LinuxSurfaceTarget::X11 { display, window } => {
            // SAFETY: The function was loaded by the Xlib surface-creation name.
            let function: vk::PFN_vkCreateXlibSurfaceKHR = unsafe { cast_function(function) };
            let info = vk::VkXlibSurfaceCreateInfoKHR {
                sType: vk::VK_STRUCTURE_TYPE_XLIB_SURFACE_CREATE_INFO_KHR,
                dpy: display.as_ptr().cast(),
                window,
                ..Default::default()
            };
            // SAFETY: Native objects and instance are live, and the output pointer is writable.
            unsafe {
                function.expect("loaded function")(instance, &raw const info, ptr::null(), surface)
            }
        }
    }
}

fn native_target(window: &Window) -> LinuxSurfaceTarget {
    let target = window.0.surface_target();
    // SAFETY: The handles are copied and used only while `window` remains borrowed and alive.
    unsafe { native_surface_target(&target) }
}

unsafe fn cast_function<T: Copy>(function: vk::PFN_vkVoidFunction) -> T {
    assert_eq!(
        mem::size_of::<T>(),
        mem::size_of::<vk::PFN_vkVoidFunction>()
    );
    // SAFETY: The caller selects the type paired with the Vulkan symbol used to load this pointer.
    unsafe { mem::transmute_copy(&function) }
}
