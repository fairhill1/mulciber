use std::ffi::{CStr, c_char, c_void};
use std::mem;

use crate::vk;

#[path = "../../vulkan-triangle/src/win32.rs"]
#[allow(dead_code)]
mod window;

pub use window::Window;

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

pub fn create_window(
    title: &str,
    width: u32,
    height: u32,
    visible: bool,
    requested_platform: Option<&str>,
) -> Result<Window, String> {
    if requested_platform.is_some_and(|platform| platform != "windows") {
        return Err("Windows supports only --platform windows".into());
    }
    Window::new(title, width, height, visible).map_err(|error| error.to_string())
}

pub const fn json_name(_window: &Window) -> &'static str {
    "windows"
}

pub const fn display_name(_window: &Window) -> &'static str {
    "Win32"
}

pub const fn surface_extension(_window: &Window) -> &'static CStr {
    c"VK_KHR_win32_surface"
}

pub const fn create_surface_name(_window: &Window) -> &'static CStr {
    c"vkCreateWin32SurfaceKHR"
}

pub unsafe fn create_surface(
    function: vk::PFN_vkVoidFunction,
    instance: vk::VkInstance,
    window: &Window,
    surface: *mut vk::VkSurfaceKHR,
) -> vk::VkResult {
    assert_eq!(
        mem::size_of::<vk::PFN_vkCreateWin32SurfaceKHR>(),
        mem::size_of::<vk::PFN_vkVoidFunction>()
    );
    // SAFETY: The function was loaded by the Win32 Vulkan surface-creation name.
    let function: vk::PFN_vkCreateWin32SurfaceKHR = unsafe { mem::transmute_copy(&function) };
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
