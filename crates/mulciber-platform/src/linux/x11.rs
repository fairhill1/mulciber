use std::cell::Cell;
use std::ffi::{CString, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::ptr;

use crate::PlatformError;

#[repr(C)]
struct Display {
    _unused: [u8; 0],
}

type XWindow = c_ulong;

type Atom = c_ulong;
type Status = c_int;

const FALSE: c_int = 0;
const STRUCTURE_NOTIFY_MASK: c_long = 0x0002_0000;
const DESTROY_NOTIFY: c_int = 17;
const MAP_NOTIFY: c_int = 19;
const CONFIGURE_NOTIFY: c_int = 22;
const CLIENT_MESSAGE: c_int = 33;
const XEVENT_PADDING_WORDS: usize = 24;
const XA_CARDINAL: Atom = 6;
const PROP_MODE_REPLACE: c_int = 0;

#[link(name = "X11")]
unsafe extern "C" {
    fn XOpenDisplay(display_name: *const c_char) -> *mut Display;
    fn XDefaultScreen(display: *mut Display) -> c_int;
    fn XRootWindow(display: *mut Display, screen_number: c_int) -> XWindow;
    fn XCreateSimpleWindow(
        display: *mut Display,
        parent: XWindow,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        border_width: c_uint,
        border: c_ulong,
        background: c_ulong,
    ) -> XWindow;
    fn XSetWindowBackgroundPixmap(display: *mut Display, window: XWindow, pixmap: c_ulong)
    -> c_int;
    fn XStoreName(display: *mut Display, window: XWindow, window_name: *const c_char) -> c_int;
    fn XSelectInput(display: *mut Display, window: XWindow, event_mask: c_long) -> c_int;
    fn XInternAtom(display: *mut Display, atom_name: *const c_char, only_if_exists: c_int) -> Atom;
    fn XSetWMProtocols(
        display: *mut Display,
        window: XWindow,
        protocols: *mut Atom,
        count: c_int,
    ) -> Status;
    fn XMapWindow(display: *mut Display, window: XWindow) -> c_int;
    fn XFlush(display: *mut Display) -> c_int;
    fn XPending(display: *mut Display) -> c_int;
    fn XNextEvent(display: *mut Display, event: *mut XEvent) -> c_int;
    fn XDestroyWindow(display: *mut Display, window: XWindow) -> c_int;
    fn XCloseDisplay(display: *mut Display) -> c_int;
    fn XChangeProperty(
        display: *mut Display,
        window: XWindow,
        property: Atom,
        kind: Atom,
        format: c_int,
        mode: c_int,
        data: *const u8,
        count: c_int,
    ) -> c_int;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XSyncValue {
    hi: c_int,
    lo: c_uint,
}

#[link(name = "Xext")]
unsafe extern "C" {
    fn XSyncQueryExtension(
        display: *mut Display,
        event_base: *mut c_int,
        error_base: *mut c_int,
    ) -> c_int;
    fn XSyncInitialize(
        display: *mut Display,
        major_version: *mut c_int,
        minor_version: *mut c_int,
    ) -> Status;
    fn XSyncCreateCounter(display: *mut Display, initial: XSyncValue) -> c_ulong;
    fn XSyncSetCounter(display: *mut Display, counter: c_ulong, value: XSyncValue) -> Status;
    fn XSyncDestroyCounter(display: *mut Display, counter: c_ulong) -> Status;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XConfigureEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    event: XWindow,
    window: XWindow,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    border_width: c_int,
    above: XWindow,
    override_redirect: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XClientMessageEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    window: XWindow,
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

struct WindowState {
    width: Cell<u32>,
    height: Cell<u32>,
    closed: Cell<bool>,
    pending_sync: Cell<Option<XSyncValue>>,
}

pub(super) struct Window {
    display: *mut Display,
    handle: XWindow,
    wm_protocols: Atom,
    wm_delete: Atom,
    wm_sync_request: Atom,
    sync_counter: c_ulong,
    state: WindowState,
}

impl Window {
    pub(super) fn new(
        title: &str,
        width: u32,
        height: u32,
        visible: bool,
    ) -> Result<Self, PlatformError> {
        let title = CString::new(title)
            .map_err(|_| PlatformError::new("X11 window title contains an interior NUL"))?;
        // SAFETY: A null display name asks Xlib to use the DISPLAY environment variable.
        let display = unsafe { XOpenDisplay(ptr::null()) };
        if display.is_null() {
            return Err(PlatformError::new(
                "XOpenDisplay failed; ensure DISPLAY names a reachable X11 server",
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
            return Err(PlatformError::new("XCreateSimpleWindow returned no window"));
        }
        // A `None` background stops the server from clearing every resize step to the solid
        // background color before the next presented frame arrives, which reads as whole-window
        // black flashing during interactive resize.
        // SAFETY: The display and freshly created window are live; `0` is Pixmap `None`.
        unsafe { XSetWindowBackgroundPixmap(display, handle, 0) };
        let mut window = Self {
            display,
            handle,
            wm_protocols: 0,
            wm_delete: 0,
            wm_sync_request: 0,
            sync_counter: 0,
            state: WindowState {
                width: Cell::new(width),
                height: Cell::new(height),
                closed: Cell::new(false),
                pending_sync: Cell::new(None),
            },
        };
        // SAFETY: The display/window are live, the title is NUL-terminated, and the event mask
        // requests the structure notifications this module consumes.
        unsafe {
            XStoreName(window.display, window.handle, title.as_ptr());
            XSelectInput(window.display, window.handle, STRUCTURE_NOTIFY_MASK);
        }
        window.register_wm_protocols()?;
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

    fn register_wm_protocols(&mut self) -> Result<(), PlatformError> {
        // SAFETY: The display is live and both atom names are NUL-terminated.
        unsafe {
            self.wm_protocols = XInternAtom(self.display, c"WM_PROTOCOLS".as_ptr(), FALSE);
            self.wm_delete = XInternAtom(self.display, c"WM_DELETE_WINDOW".as_ptr(), FALSE);
        }
        if self.wm_protocols == 0 || self.wm_delete == 0 {
            return Err(PlatformError::new(
                "XInternAtom returned no WM_PROTOCOLS/WM_DELETE_WINDOW atom",
            ));
        }
        self.create_sync_counter();
        let mut protocols = [self.wm_delete, self.wm_sync_request];
        let count = if self.sync_counter == 0 { 1 } else { 2 };
        // SAFETY: The display/window are live and the protocol array outlives the call.
        if unsafe { XSetWMProtocols(self.display, self.handle, protocols.as_mut_ptr(), count) } == 0
        {
            return Err(PlatformError::new(
                "XSetWMProtocols failed to register WM_DELETE_WINDOW",
            ));
        }
        Ok(())
    }

    /// Registers `_NET_WM_SYNC_REQUEST` so the window manager paces interactive resize against
    /// presented frames; `KWin` freezes X11 window content for the whole drag without it. Absence
    /// of the `XSync` extension leaves `sync_counter` at zero and skips the protocol.
    fn create_sync_counter(&mut self) {
        let mut event_base = 0;
        let mut error_base = 0;
        let mut major = 0;
        let mut minor = 0;
        // SAFETY: The display is live and every output pointer is writable.
        unsafe {
            if XSyncQueryExtension(self.display, &raw mut event_base, &raw mut error_base) == 0
                || XSyncInitialize(self.display, &raw mut major, &raw mut minor) == 0
            {
                return;
            }
            self.wm_sync_request =
                XInternAtom(self.display, c"_NET_WM_SYNC_REQUEST".as_ptr(), FALSE);
            let counter_property = XInternAtom(
                self.display,
                c"_NET_WM_SYNC_REQUEST_COUNTER".as_ptr(),
                FALSE,
            );
            if self.wm_sync_request == 0 || counter_property == 0 {
                return;
            }
            self.sync_counter = XSyncCreateCounter(self.display, XSyncValue { hi: 0, lo: 0 });
            if self.sync_counter == 0 {
                return;
            }
            let counter: c_ulong = self.sync_counter;
            XChangeProperty(
                self.display,
                self.handle,
                counter_property,
                XA_CARDINAL,
                32,
                PROP_MODE_REPLACE,
                (&raw const counter).cast(),
                1,
            );
        }
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
                {
                    let protocol = message.data[0].unsigned_abs();
                    if protocol == self.wm_delete {
                        self.state.closed.set(true);
                    } else if self.sync_counter != 0 && protocol == self.wm_sync_request {
                        // data holds the 64-bit sync value the window manager waits for as
                        // CARD32 low/high halves after the protocol atom and timestamp.
                        self.state.pending_sync.set(Some(XSyncValue {
                            hi: i32::try_from(message.data[3]).unwrap_or(0),
                            lo: u32::try_from(message.data[2] & 0xffff_ffff).unwrap_or(0),
                        }));
                    }
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

    pub(super) fn client_extent(&self) -> (u32, u32) {
        (self.state.width.get(), self.state.height.get())
    }

    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn pump_events(&self) -> Result<bool, PlatformError> {
        // X11 delivers resize as ordinary queued ConfigureNotify events instead of a nested native
        // resize loop, so the live-resize callback contract has nothing to drive here. Xlib pushes
        // fatal connection errors through its process-exiting default I/O handler rather than a
        // recoverable return value, which this probe records as a known Xlib boundary.
        //
        // A sync request stored by the previous pump was answered by the frame the render loop
        // produced between pumps, so report that value before draining the next resize step. The
        // rare unrendered iteration (zero extent or an unacquirable image) merely degrades one
        // step to the unsynchronized behavior.
        if let Some(value) = self.state.pending_sync.replace(None) {
            // SAFETY: The display is live and the counter was created by this window.
            unsafe {
                XSyncSetCounter(self.display, self.sync_counter, value);
            }
        }
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

    pub(super) const fn display(&self) -> *mut c_void {
        self.display.cast()
    }

    pub(super) const fn handle(&self) -> u64 {
        self.handle
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: This value owns both handles and Vulkan destroys its surface before this drop.
        unsafe {
            if self.sync_counter != 0 {
                XSyncDestroyCounter(self.display, self.sync_counter);
            }
            if self.handle != 0 {
                XDestroyWindow(self.display, self.handle);
            }
            if !self.display.is_null() {
                XCloseDisplay(self.display);
            }
        }
    }
}
