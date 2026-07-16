use std::ffi::{CStr, c_char, c_void};

use crate::vk;

#[path = "../../vulkan-win32-triangle/src/win32.rs"]
#[allow(dead_code)]
mod window;

pub use window::Window;

pub const JSON_NAME: &str = "windows";
pub const DISPLAY_NAME: &str = "Win32";
pub const SURFACE_EXTENSION: &CStr = c"VK_KHR_win32_surface";
pub const CREATE_SURFACE_NAME: &CStr = c"vkCreateWin32SurfaceKHR";
pub type CreateSurface = vk::PFN_vkCreateWin32SurfaceKHR;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
}

pub struct VulkanLibrary(*mut c_void);

impl VulkanLibrary {
    pub fn open() -> Result<Self, &'static str> {
        let name: Vec<u16> = "vulkan-1.dll".encode_utf16().chain(Some(0)).collect();
        // SAFETY: The UTF-16 library name is NUL-terminated.
        let library = unsafe { LoadLibraryW(name.as_ptr()) };
        if library.is_null() {
            Err("could not load vulkan-1.dll; install a Vulkan driver")
        } else {
            Ok(Self(library))
        }
    }

    pub unsafe fn symbol(&self, name: &CStr) -> *mut c_void {
        // SAFETY: The library is live and the symbol name is NUL-terminated.
        unsafe { GetProcAddress(self.0, name.as_ptr()) }
    }
}

impl Drop for VulkanLibrary {
    fn drop(&mut self) {
        // SAFETY: All Vulkan children are destroyed before the owning library is dropped.
        unsafe { FreeLibrary(self.0) };
    }
}

pub unsafe fn create_surface(
    function: CreateSurface,
    instance: vk::VkInstance,
    window: &Window,
    surface: *mut vk::VkSurfaceKHR,
) -> vk::VkResult {
    let info = vk::VkWin32SurfaceCreateInfoKHR {
        sType: vk::VK_STRUCTURE_TYPE_WIN32_SURFACE_CREATE_INFO_KHR,
        hinstance: window.instance(),
        hwnd: window.handle(),
        ..Default::default()
    };
    // SAFETY: The window/instance are live, output is writable, and the function type matches.
    unsafe {
        function.expect("loaded function")(instance, &raw const info, std::ptr::null(), surface)
    }
}
