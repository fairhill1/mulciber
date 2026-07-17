use core::ffi::CStr;
use core::{mem, ptr};

use mulciber_platform::SurfaceTarget;

use super::vk;

pub(super) fn surface_extension(target: &SurfaceTarget<'_>) -> &'static CStr {
    #[cfg(target_os = "windows")]
    {
        let _ = target;
        c"VK_KHR_win32_surface"
    }
    #[cfg(target_os = "linux")]
    {
        match native_target(target) {
            mulciber_platform::integration::LinuxSurfaceTarget::Wayland { .. } => {
                c"VK_KHR_wayland_surface"
            }
            mulciber_platform::integration::LinuxSurfaceTarget::X11 { .. } => {
                c"VK_KHR_xlib_surface"
            }
        }
    }
}

pub(super) fn create_surface_name(target: &SurfaceTarget<'_>) -> &'static CStr {
    #[cfg(target_os = "windows")]
    {
        let _ = target;
        c"vkCreateWin32SurfaceKHR"
    }
    #[cfg(target_os = "linux")]
    {
        match native_target(target) {
            mulciber_platform::integration::LinuxSurfaceTarget::Wayland { .. } => {
                c"vkCreateWaylandSurfaceKHR"
            }
            mulciber_platform::integration::LinuxSurfaceTarget::X11 { .. } => {
                c"vkCreateXlibSurfaceKHR"
            }
        }
    }
}

pub(super) const fn acquire_timeout() -> u64 {
    #[cfg(target_os = "windows")]
    {
        u64::MAX
    }
    #[cfg(target_os = "linux")]
    {
        0
    }
}

pub(super) unsafe fn create_surface(
    function: vk::PFN_vkVoidFunction,
    instance: vk::VkInstance,
    target: &SurfaceTarget<'_>,
    surface: *mut vk::VkSurfaceKHR,
) -> vk::VkResult {
    #[cfg(target_os = "windows")]
    {
        // SAFETY: The copied handles remain borrowed from `target` for this call.
        let target = unsafe { mulciber_platform::integration::native_surface_target(target) };
        // SAFETY: The function was loaded from vkCreateWin32SurfaceKHR.
        let function: vk::PFN_vkCreateWin32SurfaceKHR = unsafe { cast_function(function) };
        let info = vk::VkWin32SurfaceCreateInfoKHR {
            sType: vk::VK_STRUCTURE_TYPE_WIN32_SURFACE_CREATE_INFO_KHR,
            hinstance: target.instance.as_ptr(),
            hwnd: target.window.as_ptr(),
            ..Default::default()
        };
        // SAFETY: Native handles and instance are live and output storage is writable.
        unsafe {
            function.expect("loaded function")(instance, &raw const info, ptr::null(), surface)
        }
    }
    #[cfg(target_os = "linux")]
    {
        match native_target(target) {
            mulciber_platform::integration::LinuxSurfaceTarget::Wayland {
                display,
                surface: wayland_surface,
            } => {
                // SAFETY: The function was loaded from vkCreateWaylandSurfaceKHR.
                let function: vk::PFN_vkCreateWaylandSurfaceKHR =
                    unsafe { cast_function(function) };
                let info = vk::VkWaylandSurfaceCreateInfoKHR {
                    sType: vk::VK_STRUCTURE_TYPE_WAYLAND_SURFACE_CREATE_INFO_KHR,
                    display: display.as_ptr().cast(),
                    surface: wayland_surface.as_ptr().cast(),
                    ..Default::default()
                };
                // SAFETY: Native handles and instance are live and output storage is writable.
                unsafe {
                    function.expect("loaded function")(
                        instance,
                        &raw const info,
                        ptr::null(),
                        surface,
                    )
                }
            }
            mulciber_platform::integration::LinuxSurfaceTarget::X11 { display, window } => {
                // SAFETY: The function was loaded from vkCreateXlibSurfaceKHR.
                let function: vk::PFN_vkCreateXlibSurfaceKHR = unsafe { cast_function(function) };
                let info = vk::VkXlibSurfaceCreateInfoKHR {
                    sType: vk::VK_STRUCTURE_TYPE_XLIB_SURFACE_CREATE_INFO_KHR,
                    dpy: display.as_ptr().cast(),
                    window,
                    ..Default::default()
                };
                // SAFETY: Native handles and instance are live and output storage is writable.
                unsafe {
                    function.expect("loaded function")(
                        instance,
                        &raw const info,
                        ptr::null(),
                        surface,
                    )
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn native_target(target: &SurfaceTarget<'_>) -> mulciber_platform::integration::LinuxSurfaceTarget {
    // SAFETY: Handles are copied only for immediate Vulkan integration while the target is live.
    unsafe { mulciber_platform::integration::native_surface_target(target) }
}

unsafe fn cast_function<T: Copy>(function: vk::PFN_vkVoidFunction) -> T {
    assert_eq!(mem::size_of::<T>(), mem::size_of_val(&function));
    // SAFETY: The caller pairs the type with the exact symbol used to load this pointer.
    unsafe { mem::transmute_copy(&function) }
}
