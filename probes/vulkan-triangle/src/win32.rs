//! Vulkan integration adapter over `mulciber-platform`'s Win32 backend.

use std::ffi::CStr;
use std::fmt;
use std::ptr;
use std::time::Duration;

use mulciber_platform::integration::{Win32SurfaceTarget, in_live_resize, native_surface_target};
use mulciber_platform::{
    Application as PlatformApplication, LogicalSize, PumpStatus, Window as PlatformWindow,
    WindowDescriptor, WindowEvent,
};

use crate::vk;

pub(crate) type SurfaceFunction = vk::PFN_vkCreateWin32SurfaceKHR;

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
        if requested_platform.is_some_and(|platform| platform != "windows") {
            return Err(WindowError(
                "Windows supports only --platform windows".into(),
            ));
        }
        PlatformApplication::new()
            .map(|platform| Self { platform })
            .map_err(|error| WindowError(error.to_string()))
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
                "the Vulkan probe requires a visible Win32 window".into(),
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
        live_resize: &mut F,
    ) -> Result<bool, WindowError>
    where
        F: FnMut(),
    {
        self.platform
            .pump_events(&window.0, |event| {
                if matches!(event, WindowEvent::RedrawRequested(_)) && in_live_resize(&window.0) {
                    live_resize();
                }
            })
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

pub(crate) const fn surface_extension(_window: &Window) -> &'static CStr {
    c"VK_KHR_win32_surface"
}

pub(crate) const fn surface_description(_window: &Window) -> &'static str {
    "Win32 surface extension"
}

pub(crate) const fn create_surface_name(_window: &Window) -> &'static CStr {
    c"vkCreateWin32SurfaceKHR"
}

pub(crate) const fn acquire_timeout(_window: &Window) -> u64 {
    u64::MAX
}

pub(crate) const fn resize_commit_interval(_window: &Window) -> Duration {
    Duration::ZERO
}

pub(crate) unsafe fn create_surface(
    function: SurfaceFunction,
    instance: vk::VkInstance,
    window: &Window,
    surface: *mut vk::VkSurfaceKHR,
) -> vk::VkResult {
    let target = native_target(window);
    let info = vk::VkWin32SurfaceCreateInfoKHR {
        sType: vk::VK_STRUCTURE_TYPE_WIN32_SURFACE_CREATE_INFO_KHR,
        hinstance: target.instance.as_ptr(),
        hwnd: target.window.as_ptr(),
        ..Default::default()
    };
    // SAFETY: Native handles and instance are live, output is writable, and the function matches.
    unsafe { function.expect("loaded function")(instance, &raw const info, ptr::null(), surface) }
}

fn native_target(window: &Window) -> Win32SurfaceTarget {
    let target = window.0.surface_target();
    // SAFETY: The handles are copied and used only while `window` remains borrowed and alive.
    unsafe { native_surface_target(&target) }
}
