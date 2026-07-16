//! Runtime-selected Linux presentation platform with peer Wayland and X11 modules.

use std::env;
use std::ffi::CStr;
use std::fmt;
use std::mem;
use std::time::Duration;

use crate::vk;

#[path = "wayland.rs"]
mod wayland;
#[path = "x11.rs"]
mod x11;

pub(crate) type SurfaceFunction = vk::PFN_vkVoidFunction;

#[derive(Debug)]
pub struct WindowError(String);

impl fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WindowError {}

pub enum Window {
    Wayland(wayland::Window),
    X11(x11::Window),
}

pub(crate) fn create_window(
    title: &str,
    width: u32,
    height: u32,
    visible: bool,
    requested_platform: Option<&str>,
) -> Result<Window, WindowError> {
    let platform = match requested_platform {
        Some("wayland") => "wayland",
        Some("x11") => "x11",
        Some(other) => {
            return Err(WindowError(format!(
                "unsupported Linux platform {other:?}; expected x11 or wayland"
            )));
        }
        None if environment_is_set("WAYLAND_DISPLAY") => "wayland",
        None if environment_is_set("DISPLAY") => "x11",
        None => {
            return Err(WindowError(
                "no Wayland or X11 display is available; set WAYLAND_DISPLAY/DISPLAY or pass --platform"
                    .into(),
            ));
        }
    };
    match platform {
        "wayland" => wayland::Window::new(title, width, height, visible)
            .map(Window::Wayland)
            .map_err(|error| WindowError(error.to_string())),
        "x11" => x11::Window::new(title, width, height, visible)
            .map(Window::X11)
            .map_err(|error| WindowError(error.to_string())),
        _ => unreachable!("platform was matched above"),
    }
}

impl Window {
    pub fn client_extent(&self) -> Result<(u32, u32), WindowError> {
        match self {
            Self::Wayland(window) => window
                .client_extent()
                .map_err(|error| WindowError(error.to_string())),
            Self::X11(window) => window
                .client_extent()
                .map_err(|error| WindowError(error.to_string())),
        }
    }

    pub fn pump_events<F>(&self, live_resize: &mut F) -> Result<bool, WindowError>
    where
        F: FnMut(),
    {
        match self {
            Self::Wayland(window) => window
                .pump_events(live_resize)
                .map_err(|error| WindowError(error.to_string())),
            Self::X11(window) => window
                .pump_events(live_resize)
                .map_err(|error| WindowError(error.to_string())),
        }
    }
}

pub(crate) const fn surface_extension(window: &Window) -> &'static CStr {
    match window {
        Window::Wayland(_) => c"VK_KHR_wayland_surface",
        Window::X11(_) => c"VK_KHR_xlib_surface",
    }
}

pub(crate) const fn surface_description(window: &Window) -> &'static str {
    match window {
        Window::Wayland(_) => "Wayland surface extension",
        Window::X11(_) => "Xlib surface extension",
    }
}

pub(crate) const fn create_surface_name(window: &Window) -> &'static CStr {
    match window {
        Window::Wayland(_) => c"vkCreateWaylandSurfaceKHR",
        Window::X11(_) => c"vkCreateXlibSurfaceKHR",
    }
}

pub(crate) const fn acquire_timeout(window: &Window) -> u64 {
    // Both Linux paths acquire without blocking: compositor-driven presentation may withhold
    // images indefinitely (X11 presentation ultimately reaches a compositor under XWayland), and
    // the render loop already treats VK_NOT_READY as a paced retry.
    match window {
        Window::Wayland(_) | Window::X11(_) => 0,
    }
}

pub(crate) const fn resize_commit_interval(window: &Window) -> Duration {
    // Swapchain recreation supplies fresh images that bypass FIFO acquisition backpressure, so
    // resize commits are paced on both Linux paths; see the Wayland resize investigation in
    // docs/linux-validation.md. Native Xorg pacing behavior has not been separately measured.
    match window {
        Window::Wayland(_) | Window::X11(_) => Duration::from_millis(16),
    }
}

pub(crate) unsafe fn create_surface(
    function: SurfaceFunction,
    instance: vk::VkInstance,
    window: &Window,
    surface: *mut vk::VkSurfaceKHR,
) -> vk::VkResult {
    match window {
        Window::Wayland(window) => {
            // SAFETY: The function was loaded by the Wayland Vulkan surface-creation name.
            let function = unsafe { cast_function(function) };
            // SAFETY: The typed function and native window variant match.
            unsafe { wayland::create_surface(function, instance, window, surface) }
        }
        Window::X11(window) => {
            // SAFETY: The function was loaded by the Xlib Vulkan surface-creation name.
            let function = unsafe { cast_function(function) };
            // SAFETY: The typed function and native window variant match.
            unsafe { x11::create_surface(function, instance, window, surface) }
        }
    }
}

fn environment_is_set(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

unsafe fn cast_function<T: Copy>(function: vk::PFN_vkVoidFunction) -> T {
    assert_eq!(
        mem::size_of::<T>(),
        mem::size_of::<vk::PFN_vkVoidFunction>()
    );
    // SAFETY: The caller selects the type paired with the Vulkan symbol used to load this pointer.
    unsafe { mem::transmute_copy(&function) }
}
