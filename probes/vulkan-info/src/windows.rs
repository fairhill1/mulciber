use std::ffi::{CStr, c_char, c_void};
use std::mem;

use mulciber_platform::integration::{
    Win32SurfaceTarget, create_window as create_platform_window, native_surface_target,
};
use mulciber_platform::{Application, LogicalSize, Window as PlatformWindow, WindowDescriptor};

use crate::vk;

pub struct Window {
    window: PlatformWindow,
    _application: Application,
}

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
    let application = Application::new().map_err(|error| error.to_string())?;
    let descriptor = WindowDescriptor::new(title, LogicalSize::new(width, height));
    let window = create_platform_window(&application, &descriptor, visible)
        .map_err(|error| error.to_string())?;
    Ok(Window {
        window,
        _application: application,
    })
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
    let target = native_target(window);
    let info = vk::VkWin32SurfaceCreateInfoKHR {
        sType: vk::VK_STRUCTURE_TYPE_WIN32_SURFACE_CREATE_INFO_KHR,
        hinstance: target.instance.as_ptr(),
        hwnd: target.window.as_ptr(),
        ..Default::default()
    };
    // SAFETY: The window/instance are live, output is writable, and the function type matches.
    unsafe {
        function.expect("loaded function")(instance, &raw const info, std::ptr::null(), surface)
    }
}

fn native_target(window: &Window) -> Win32SurfaceTarget {
    let target = window.window.surface_target();
    // SAFETY: The handles are used only while the source window remains alive.
    unsafe { native_surface_target(&target) }
}
