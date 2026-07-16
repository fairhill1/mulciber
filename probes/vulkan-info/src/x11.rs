use std::ffi::{CString, c_char, c_int, c_uint, c_ulong};
use std::fmt;
use std::ptr;

use crate::vk;

#[link(name = "X11")]
unsafe extern "C" {
    fn XOpenDisplay(display_name: *const c_char) -> *mut vk::Display;
    fn XDefaultScreen(display: *mut vk::Display) -> c_int;
    fn XRootWindow(display: *mut vk::Display, screen_number: c_int) -> vk::Window;
    fn XCreateSimpleWindow(
        display: *mut vk::Display,
        parent: vk::Window,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        border_width: c_uint,
        border: c_ulong,
        background: c_ulong,
    ) -> vk::Window;
    fn XStoreName(
        display: *mut vk::Display,
        window: vk::Window,
        window_name: *const c_char,
    ) -> c_int;
    fn XMapWindow(display: *mut vk::Display, window: vk::Window) -> c_int;
    fn XFlush(display: *mut vk::Display) -> c_int;
    fn XDestroyWindow(display: *mut vk::Display, window: vk::Window) -> c_int;
    fn XCloseDisplay(display: *mut vk::Display) -> c_int;
}

#[derive(Debug)]
pub struct X11Error(String);

impl fmt::Display for X11Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for X11Error {}

pub(crate) struct Window {
    display: *mut vk::Display,
    handle: vk::Window,
}

impl Window {
    pub(super) fn new(
        title: &str,
        width: u32,
        height: u32,
        visible: bool,
    ) -> Result<Self, X11Error> {
        let title = CString::new(title)
            .map_err(|_| X11Error("X11 window title contains an interior NUL".into()))?;
        // SAFETY: A null display name asks Xlib to use the DISPLAY environment variable.
        let display = unsafe { XOpenDisplay(ptr::null()) };
        if display.is_null() {
            return Err(X11Error(
                "XOpenDisplay failed; ensure DISPLAY names a reachable X11 server".into(),
            ));
        }
        // SAFETY: The display connection is live for these Xlib calls.
        let screen = unsafe { XDefaultScreen(display) };
        // SAFETY: The display and screen index originate from Xlib.
        let root = unsafe { XRootWindow(display, screen) };
        // SAFETY: The parent window and display are live; dimensions are nonzero probe constants.
        let handle = unsafe { XCreateSimpleWindow(display, root, 0, 0, width, height, 0, 0, 0) };
        if handle == 0 {
            // SAFETY: The display was opened above and no window was created.
            unsafe { XCloseDisplay(display) };
            return Err(X11Error("XCreateSimpleWindow returned no window".into()));
        }
        // SAFETY: The display/window are live and the title is NUL-terminated.
        unsafe {
            XStoreName(display, handle, title.as_ptr());
            if visible {
                XMapWindow(display, handle);
            }
            XFlush(display);
        }
        Ok(Self { display, handle })
    }

    pub(super) const fn display(&self) -> *mut vk::Display {
        self.display
    }

    pub(super) const fn handle(&self) -> vk::Window {
        self.handle
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: This value owns both handles and Vulkan destroys its surface before this drop.
        unsafe {
            if self.handle != 0 {
                XDestroyWindow(self.display, self.handle);
            }
            if !self.display.is_null() {
                XCloseDisplay(self.display);
            }
        }
    }
}

pub(super) unsafe fn create_surface(
    function: vk::PFN_vkCreateXlibSurfaceKHR,
    instance: vk::VkInstance,
    window: &Window,
    surface: *mut vk::VkSurfaceKHR,
) -> vk::VkResult {
    let info = vk::VkXlibSurfaceCreateInfoKHR {
        sType: vk::VK_STRUCTURE_TYPE_XLIB_SURFACE_CREATE_INFO_KHR,
        dpy: window.display(),
        window: window.handle(),
        ..Default::default()
    };
    // SAFETY: The X11 window/instance are live, output is writable, and the function type matches.
    unsafe {
        function.expect("loaded function")(instance, &raw const info, std::ptr::null(), surface)
    }
}
