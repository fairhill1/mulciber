use std::env;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::mem;

use crate::vk;

#[path = "wayland.rs"]
mod wayland;
#[path = "x11.rs"]
mod x11;

const RTLD_NOW: c_int = 2;

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
}

pub struct VulkanLibrary(*mut c_void);

impl VulkanLibrary {
    pub fn open() -> Result<Self, &'static str> {
        // SAFETY: The library name is static and NUL-terminated.
        let library = unsafe { dlopen(c"libvulkan.so.1".as_ptr(), RTLD_NOW) };
        if library.is_null() {
            Err("could not load libvulkan.so.1; install a Vulkan loader and driver")
        } else {
            Ok(Self(library))
        }
    }

    pub unsafe fn symbol(&self, name: &CStr) -> *mut c_void {
        // SAFETY: The library is live and the symbol name is NUL-terminated.
        unsafe { dlsym(self.0, name.as_ptr()) }
    }
}

impl Drop for VulkanLibrary {
    fn drop(&mut self) {
        // SAFETY: All Vulkan children are destroyed before the owning library is dropped.
        unsafe { dlclose(self.0) };
    }
}

pub enum Window {
    Wayland(wayland::Window),
    X11(x11::Window),
}

pub fn create_window(
    title: &str,
    width: u32,
    height: u32,
    visible: bool,
    requested_platform: Option<&str>,
) -> Result<Window, String> {
    let platform = match requested_platform {
        Some("wayland") => "wayland",
        Some("x11") => "x11",
        Some(other) => {
            return Err(format!(
                "unsupported Linux platform {other:?}; expected x11 or wayland"
            ));
        }
        None if environment_is_set("WAYLAND_DISPLAY") => "wayland",
        None if environment_is_set("DISPLAY") => "x11",
        None => {
            return Err(
                "no Wayland or X11 display is available; set WAYLAND_DISPLAY/DISPLAY or pass --platform"
                    .into(),
            );
        }
    };
    match platform {
        "wayland" => wayland::Window::new(title, width, height, visible)
            .map(Window::Wayland)
            .map_err(|error| error.to_string()),
        "x11" => x11::Window::new(title, width, height, visible)
            .map(Window::X11)
            .map_err(|error| error.to_string()),
        _ => unreachable!("platform was matched above"),
    }
}

pub const fn json_name(window: &Window) -> &'static str {
    match window {
        Window::Wayland(_) => "linux-wayland",
        Window::X11(_) => "linux-x11",
    }
}

pub const fn display_name(window: &Window) -> &'static str {
    match window {
        Window::Wayland(_) => "Wayland",
        Window::X11(_) => "X11",
    }
}

pub const fn surface_extension(window: &Window) -> &'static CStr {
    match window {
        Window::Wayland(_) => c"VK_KHR_wayland_surface",
        Window::X11(_) => c"VK_KHR_xlib_surface",
    }
}

pub const fn create_surface_name(window: &Window) -> &'static CStr {
    match window {
        Window::Wayland(_) => c"vkCreateWaylandSurfaceKHR",
        Window::X11(_) => c"vkCreateXlibSurfaceKHR",
    }
}

pub unsafe fn create_surface(
    function: vk::PFN_vkVoidFunction,
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
