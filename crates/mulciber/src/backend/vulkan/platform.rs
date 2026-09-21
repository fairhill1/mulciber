use core::ffi::CStr;
use core::time::Duration;
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

pub(super) fn resize_commit_interval(target: &SurfaceTarget<'_>) -> Duration {
    #[cfg(target_os = "windows")]
    {
        // Win32's nested sizing loop already gates each interactive resize step.
        let _ = target;
        Duration::ZERO
    }
    #[cfg(target_os = "linux")]
    {
        // Wayland swapchain recreation bypasses FIFO acquisition backpressure: every replacement
        // swapchain provides images immediately, so an unpaced client commits one new-size buffer
        // per configure and the compositor's FIFO presentation backlog grows without bound during
        // a drag. X11's `_NET_WM_SYNC_REQUEST` counter already gates each interactive resize step.
        match native_target(target) {
            mulciber_platform::integration::LinuxSurfaceTarget::Wayland { .. } => {
                Duration::from_millis(16)
            }
            mulciber_platform::integration::LinuxSurfaceTarget::X11 { .. } => Duration::ZERO,
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

/// Keep Windows presentation from accumulating stale completed frames. Mailbox
/// remains synchronized to vertical blank; FIFO is the guaranteed fallback.
/// Linux keeps its existing compositor-paced FIFO policy.
pub(super) fn choose_present_mode(modes: &[vk::VkPresentModeKHR]) -> Option<vk::VkPresentModeKHR> {
    if cfg!(target_os = "windows") && modes.contains(&vk::VK_PRESENT_MODE_MAILBOX_KHR) {
        Some(vk::VK_PRESENT_MODE_MAILBOX_KHR)
    } else if modes.contains(&vk::VK_PRESENT_MODE_FIFO_KHR) {
        Some(vk::VK_PRESENT_MODE_FIFO_KHR)
    } else {
        None
    }
}

/// Never silently substitute plain synchronized output for a request that asked
/// not to be stalled by synchronization.
///
/// [`crate::PresentationMode::Adaptive`] has two native spellings and takes
/// whichever the driver offers. Relaxed FIFO lets a late frame through
/// immediately, tearing rather than holding the previous image for another
/// whole interval. Latest-ready instead keeps every present on a vertical
/// blank and discards the images that went stale waiting for one, so a frame
/// finished during the interval is shown at the next blank rather than behind
/// the ones queued before it. They differ in whether the screen may tear and
/// agree on the thing the policy is asked for, which is that the queue never
/// costs the player latency. Neither is offered in place of the other's
/// availability: an adapter with both takes relaxed FIFO, and an adapter with
/// neither reports the policy unsupported rather than falling back to FIFO.
///
/// `latest_ready` is whether the device enabled
/// `VK_KHR_present_mode_fifo_latest_ready` and its feature, not merely whether
/// the surface lists the mode. A surface advertises it to any query, and
/// presenting in a mode the device did not enable the feature for is invalid
/// usage.
pub(super) fn choose_present_mode_for_policy(
    modes: &[vk::VkPresentModeKHR],
    policy: crate::PresentationMode,
    latest_ready: bool,
) -> Option<vk::VkPresentModeKHR> {
    match policy {
        crate::PresentationMode::Synchronized => choose_present_mode(modes),
        crate::PresentationMode::HalfRefresh | crate::PresentationMode::Strict => modes
            .contains(&vk::VK_PRESENT_MODE_FIFO_KHR)
            .then_some(vk::VK_PRESENT_MODE_FIFO_KHR),
        crate::PresentationMode::Immediate => modes
            .contains(&vk::VK_PRESENT_MODE_IMMEDIATE_KHR)
            .then_some(vk::VK_PRESENT_MODE_IMMEDIATE_KHR),
        crate::PresentationMode::Adaptive => {
            if modes.contains(&vk::VK_PRESENT_MODE_FIFO_RELAXED_KHR) {
                Some(vk::VK_PRESENT_MODE_FIFO_RELAXED_KHR)
            } else if latest_ready && modes.contains(&vk::VK_PRESENT_MODE_FIFO_LATEST_READY_KHR) {
                Some(vk::VK_PRESENT_MODE_FIFO_LATEST_READY_KHR)
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;

    #[test]
    fn adaptive_requires_one_of_the_two_native_spellings() {
        assert_eq!(
            choose_present_mode_for_policy(
                &[
                    vk::VK_PRESENT_MODE_FIFO_KHR,
                    vk::VK_PRESENT_MODE_IMMEDIATE_KHR
                ],
                crate::PresentationMode::Adaptive,
                true
            ),
            None
        );
        assert_eq!(
            choose_present_mode_for_policy(
                &[
                    vk::VK_PRESENT_MODE_FIFO_KHR,
                    vk::VK_PRESENT_MODE_FIFO_RELAXED_KHR
                ],
                crate::PresentationMode::Adaptive,
                false
            ),
            Some(vk::VK_PRESENT_MODE_FIFO_RELAXED_KHR)
        );
        assert_eq!(
            choose_present_mode_for_policy(
                &[
                    vk::VK_PRESENT_MODE_FIFO_KHR,
                    vk::VK_PRESENT_MODE_FIFO_LATEST_READY_KHR
                ],
                crate::PresentationMode::Adaptive,
                true
            ),
            Some(vk::VK_PRESENT_MODE_FIFO_LATEST_READY_KHR)
        );
    }

    /// Relaxed FIFO is what the policy was written for, so an adapter offering
    /// both keeps it rather than changing behaviour under machines it already
    /// worked on.
    #[test]
    fn relaxed_fifo_wins_where_both_spellings_exist() {
        assert_eq!(
            choose_present_mode_for_policy(
                &[
                    vk::VK_PRESENT_MODE_FIFO_KHR,
                    vk::VK_PRESENT_MODE_FIFO_LATEST_READY_KHR,
                    vk::VK_PRESENT_MODE_FIFO_RELAXED_KHR
                ],
                crate::PresentationMode::Adaptive,
                true
            ),
            Some(vk::VK_PRESENT_MODE_FIFO_RELAXED_KHR)
        );
    }

    /// A surface lists latest-ready whether or not the device enabled the
    /// feature that makes presenting in it legal.
    #[test]
    fn latest_ready_is_refused_until_the_device_enables_it() {
        assert_eq!(
            choose_present_mode_for_policy(
                &[
                    vk::VK_PRESENT_MODE_FIFO_KHR,
                    vk::VK_PRESENT_MODE_FIFO_LATEST_READY_KHR
                ],
                crate::PresentationMode::Adaptive,
                false
            ),
            None
        );
    }

    #[test]
    fn immediate_is_explicit_and_never_silently_falls_back() {
        assert_eq!(
            choose_present_mode_for_policy(
                &[vk::VK_PRESENT_MODE_FIFO_KHR],
                crate::PresentationMode::Immediate,
                true
            ),
            None
        );
        assert_eq!(
            choose_present_mode_for_policy(
                &[
                    vk::VK_PRESENT_MODE_FIFO_KHR,
                    vk::VK_PRESENT_MODE_IMMEDIATE_KHR
                ],
                crate::PresentationMode::Immediate,
                true
            ),
            Some(vk::VK_PRESENT_MODE_IMMEDIATE_KHR)
        );
        assert_eq!(
            choose_present_mode_for_policy(
                &[
                    vk::VK_PRESENT_MODE_FIFO_KHR,
                    vk::VK_PRESENT_MODE_IMMEDIATE_KHR
                ],
                crate::PresentationMode::Synchronized,
                true
            ),
            Some(vk::VK_PRESENT_MODE_FIFO_KHR)
        );
    }

    /// Latest-ready is the adaptive policy's second spelling and must never
    /// become a quieter answer to a plain synchronized request, which promises
    /// every rendered frame reaches the screen.
    #[test]
    fn synchronized_never_takes_the_latest_ready_shortcut() {
        assert_eq!(
            choose_present_mode_for_policy(
                &[
                    vk::VK_PRESENT_MODE_FIFO_KHR,
                    vk::VK_PRESENT_MODE_FIFO_LATEST_READY_KHR
                ],
                crate::PresentationMode::Synchronized,
                true
            ),
            Some(vk::VK_PRESENT_MODE_FIFO_KHR)
        );
    }

    #[test]
    fn fifo_only_surfaces_remain_supported() {
        assert_eq!(
            choose_present_mode(&[vk::VK_PRESENT_MODE_FIFO_KHR]),
            Some(vk::VK_PRESENT_MODE_FIFO_KHR)
        );
        assert_eq!(choose_present_mode(&[]), None);
        assert_eq!(
            choose_present_mode(&[vk::VK_PRESENT_MODE_IMMEDIATE_KHR]),
            None
        );
    }

    #[test]
    fn mailbox_preference_is_windows_only() {
        let modes = [
            vk::VK_PRESENT_MODE_FIFO_KHR,
            vk::VK_PRESENT_MODE_MAILBOX_KHR,
        ];
        let expected = if cfg!(target_os = "windows") {
            vk::VK_PRESENT_MODE_MAILBOX_KHR
        } else {
            vk::VK_PRESENT_MODE_FIFO_KHR
        };
        assert_eq!(choose_present_mode(&modes), Some(expected));
    }
}
