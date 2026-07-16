use std::cell::Cell;
use std::ffi::{CString, c_char, c_int, c_long, c_uint, c_ulong};
use std::fmt;
use std::ptr;

use crate::vk;

type Atom = c_ulong;
type Status = c_int;

const FALSE: c_int = 0;
const STRUCTURE_NOTIFY_MASK: c_long = 0x0002_0000;
const DESTROY_NOTIFY: c_int = 17;
const MAP_NOTIFY: c_int = 19;
const CONFIGURE_NOTIFY: c_int = 22;
const CLIENT_MESSAGE: c_int = 33;
const XEVENT_PADDING_WORDS: usize = 24;

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
    fn XSelectInput(display: *mut vk::Display, window: vk::Window, event_mask: c_long) -> c_int;
    fn XInternAtom(
        display: *mut vk::Display,
        atom_name: *const c_char,
        only_if_exists: c_int,
    ) -> Atom;
    fn XSetWMProtocols(
        display: *mut vk::Display,
        window: vk::Window,
        protocols: *mut Atom,
        count: c_int,
    ) -> Status;
    fn XMapWindow(display: *mut vk::Display, window: vk::Window) -> c_int;
    fn XFlush(display: *mut vk::Display) -> c_int;
    fn XPending(display: *mut vk::Display) -> c_int;
    fn XNextEvent(display: *mut vk::Display, event: *mut XEvent) -> c_int;
    fn XDestroyWindow(display: *mut vk::Display, window: vk::Window) -> c_int;
    fn XCloseDisplay(display: *mut vk::Display) -> c_int;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XConfigureEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut vk::Display,
    event: vk::Window,
    window: vk::Window,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    border_width: c_int,
    above: vk::Window,
    override_redirect: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XClientMessageEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut vk::Display,
    window: vk::Window,
    message_type: Atom,
    format: c_int,
    data: [c_long; 5],
}

/// The generic Xlib event union, sized by its libX11 padding member.
#[repr(C)]
union XEvent {
    kind: c_int,
    configure: XConfigureEvent,
    client_message: XClientMessageEvent,
    padding: [c_long; XEVENT_PADDING_WORDS],
}

#[derive(Debug)]
pub struct WindowError(String);

impl fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WindowError {}

struct WindowState {
    width: Cell<u32>,
    height: Cell<u32>,
    closed: Cell<bool>,
}

pub struct Window {
    display: *mut vk::Display,
    handle: vk::Window,
    wm_protocols: Atom,
    wm_delete: Atom,
    state: WindowState,
}

impl Window {
    pub fn new(title: &str, width: u32, height: u32, visible: bool) -> Result<Self, WindowError> {
        let title = CString::new(title)
            .map_err(|_| WindowError("X11 window title contains an interior NUL".into()))?;
        // SAFETY: A null display name asks Xlib to use the DISPLAY environment variable.
        let display = unsafe { XOpenDisplay(ptr::null()) };
        if display.is_null() {
            return Err(WindowError(
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
            return Err(WindowError("XCreateSimpleWindow returned no window".into()));
        }
        let mut window = Self {
            display,
            handle,
            wm_protocols: 0,
            wm_delete: 0,
            state: WindowState {
                width: Cell::new(width),
                height: Cell::new(height),
                closed: Cell::new(false),
            },
        };
        // SAFETY: The display/window are live, the title is NUL-terminated, and the event mask
        // requests the structure notifications this module consumes.
        unsafe {
            XStoreName(window.display, window.handle, title.as_ptr());
            XSelectInput(window.display, window.handle, STRUCTURE_NOTIFY_MASK);
        }
        window.register_delete_protocol()?;
        if visible {
            // SAFETY: The display/window are live; mapping requests WM management of the window.
            unsafe {
                XMapWindow(window.display, window.handle);
            }
            window.await_initial_map();
        }
        // SAFETY: The display connection is live and buffered requests must reach the server.
        unsafe {
            XFlush(window.display);
        }
        Ok(window)
    }

    fn register_delete_protocol(&mut self) -> Result<(), WindowError> {
        // SAFETY: The display is live and both atom names are NUL-terminated.
        unsafe {
            self.wm_protocols = XInternAtom(self.display, c"WM_PROTOCOLS".as_ptr(), FALSE);
            self.wm_delete = XInternAtom(self.display, c"WM_DELETE_WINDOW".as_ptr(), FALSE);
        }
        if self.wm_protocols == 0 || self.wm_delete == 0 {
            return Err(WindowError(
                "XInternAtom returned no WM_PROTOCOLS/WM_DELETE_WINDOW atom".into(),
            ));
        }
        let mut protocols = [self.wm_delete];
        // SAFETY: The display/window are live and the protocol array outlives the call.
        if unsafe { XSetWMProtocols(self.display, self.handle, protocols.as_mut_ptr(), 1) } == 0 {
            return Err(WindowError(
                "XSetWMProtocols failed to register WM_DELETE_WINDOW".into(),
            ));
        }
        Ok(())
    }

    /// Blocks until the mapped window's first `MapNotify`, applying any configure sent before it,
    /// so the initial swapchain uses the extent the window manager actually granted.
    fn await_initial_map(&self) {
        loop {
            let mut event = XEvent {
                padding: [0; XEVENT_PADDING_WORDS],
            };
            // SAFETY: The display is live and the event buffer is writable. XNextEvent blocks
            // until the X server delivers the next structure notification.
            unsafe {
                XNextEvent(self.display, &raw mut event);
            }
            if self.handle_event(&event) {
                break;
            }
        }
    }

    /// Applies one structure event to the window state; returns true for `MapNotify`.
    fn handle_event(&self, event: &XEvent) -> bool {
        // SAFETY: Every Xlib event begins with the c_int type discriminant.
        match unsafe { event.kind } {
            CONFIGURE_NOTIFY => {
                // SAFETY: The discriminant identifies the configure member as initialized.
                let configure = unsafe { event.configure };
                if let (Ok(width), Ok(height)) = (
                    u32::try_from(configure.width),
                    u32::try_from(configure.height),
                ) && width != 0
                    && height != 0
                {
                    self.state.width.set(width);
                    self.state.height.set(height);
                }
                false
            }
            CLIENT_MESSAGE => {
                // SAFETY: The discriminant identifies the client-message member as initialized.
                let message = unsafe { event.client_message };
                if message.message_type == self.wm_protocols
                    && message.format == 32
                    && message.data[0] >= 0
                    && message.data[0].unsigned_abs() == self.wm_delete
                {
                    self.state.closed.set(true);
                }
                false
            }
            DESTROY_NOTIFY => {
                self.state.closed.set(true);
                false
            }
            MAP_NOTIFY => true,
            _ => false,
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    pub fn client_extent(&self) -> Result<(u32, u32), WindowError> {
        Ok((self.state.width.get(), self.state.height.get()))
    }

    #[allow(clippy::unnecessary_wraps)]
    pub fn pump_events<F>(&self, _live_resize: &mut F) -> Result<bool, WindowError>
    where
        F: FnMut(),
    {
        // X11 delivers resize as ordinary queued ConfigureNotify events instead of a nested native
        // resize loop, so the live-resize callback contract has nothing to drive here. Xlib pushes
        // fatal connection errors through its process-exiting default I/O handler rather than a
        // recoverable return value, which this probe records as a known Xlib boundary.
        //
        // SAFETY: The display is live; XPending flushes and reports queued events, so each
        // XNextEvent call below consumes an already-delivered event without blocking.
        while unsafe { XPending(self.display) } > 0 {
            let mut event = XEvent {
                padding: [0; XEVENT_PADDING_WORDS],
            };
            // SAFETY: The display is live and the event buffer is writable.
            unsafe {
                XNextEvent(self.display, &raw mut event);
            }
            self.handle_event(&event);
        }
        Ok(!self.state.closed.get())
    }

    pub(crate) const fn display(&self) -> *mut vk::Display {
        self.display
    }

    pub(crate) const fn handle(&self) -> vk::Window {
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

pub(crate) unsafe fn create_surface(
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
    unsafe { function.expect("loaded function")(instance, &raw const info, ptr::null(), surface) }
}
