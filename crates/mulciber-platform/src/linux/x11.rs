use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::{CString, c_char, c_int, c_long, c_uint, c_ulong, c_ushort, c_void};
use std::ptr;

use super::keymap;
use crate::{
    ButtonState, CursorMode, InputEvent, LogicalPosition, Modifiers, PlatformError, PointerButton,
    ScrollDelta,
};

#[repr(C)]
struct Display {
    _unused: [u8; 0],
}

type XWindow = c_ulong;

type Atom = c_ulong;
type Status = c_int;

const FALSE: c_int = 0;
const TRUE: c_int = 1;
const KEY_PRESS_MASK: c_long = 0x0000_0001;
const KEY_RELEASE_MASK: c_long = 0x0000_0002;
const BUTTON_PRESS_MASK: c_long = 0x0000_0004;
const BUTTON_RELEASE_MASK: c_long = 0x0000_0008;
const POINTER_MOTION_MASK: c_long = 0x0000_0040;
const STRUCTURE_NOTIFY_MASK: c_long = 0x0002_0000;
const FOCUS_CHANGE_MASK: c_long = 0x0020_0000;
const KEY_PRESS: c_int = 2;
const KEY_RELEASE: c_int = 3;
const BUTTON_PRESS: c_int = 4;
const BUTTON_RELEASE: c_int = 5;
const MOTION_NOTIFY: c_int = 6;
const FOCUS_IN: c_int = 9;
const FOCUS_OUT: c_int = 10;
const DESTROY_NOTIFY: c_int = 17;
const MAP_NOTIFY: c_int = 19;
const CONFIGURE_NOTIFY: c_int = 22;
const CLIENT_MESSAGE: c_int = 33;
const XEVENT_PADDING_WORDS: usize = 24;
const XA_CARDINAL: Atom = 6;
const PROP_MODE_REPLACE: c_int = 0;
const NOTIFY_NORMAL: c_int = 0;
const NOTIFY_WHILE_GRABBED: c_int = 3;
const SHIFT_MASK: c_uint = 1 << 0;
const LOCK_MASK: c_uint = 1 << 1;
const CONTROL_MASK: c_uint = 1 << 2;
const MOD1_MASK: c_uint = 1 << 3;
const MOD4_MASK: c_uint = 1 << 6;
const GRAB_MODE_ASYNC: c_int = 1;
const GRAB_SUCCESS: c_int = 0;
const CURRENT_TIME: c_ulong = 0;
/// X keyboards under XKB report evdev key codes offset by eight.
const X_KEYCODE_EVDEV_OFFSET: c_uint = 8;
const SCROLL_BUTTON_FIRST: c_uint = 4;
const SCROLL_BUTTON_LAST: c_uint = 7;

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
    fn XkbSetDetectableAutoRepeat(
        display: *mut Display,
        detectable: c_int,
        supported: *mut c_int,
    ) -> c_int;
    fn XQueryPointer(
        display: *mut Display,
        window: XWindow,
        root: *mut XWindow,
        child: *mut XWindow,
        root_x: *mut c_int,
        root_y: *mut c_int,
        window_x: *mut c_int,
        window_y: *mut c_int,
        mask: *mut c_uint,
    ) -> c_int;
    fn XGrabPointer(
        display: *mut Display,
        grab_window: XWindow,
        owner_events: c_int,
        event_mask: c_uint,
        pointer_mode: c_int,
        keyboard_mode: c_int,
        confine_to: XWindow,
        cursor: c_ulong,
        time: c_ulong,
    ) -> c_int;
    fn XUngrabPointer(display: *mut Display, time: c_ulong) -> c_int;
    fn XWarpPointer(
        display: *mut Display,
        source: XWindow,
        destination: XWindow,
        source_x: c_int,
        source_y: c_int,
        source_width: c_uint,
        source_height: c_uint,
        destination_x: c_int,
        destination_y: c_int,
    ) -> c_int;
    fn XCreateBitmapFromData(
        display: *mut Display,
        drawable: c_ulong,
        data: *const c_char,
        width: c_uint,
        height: c_uint,
    ) -> c_ulong;
    fn XCreatePixmapCursor(
        display: *mut Display,
        source: c_ulong,
        mask: c_ulong,
        foreground: *mut XColor,
        background: *mut XColor,
        x: c_uint,
        y: c_uint,
    ) -> c_ulong;
    fn XFreePixmap(display: *mut Display, pixmap: c_ulong) -> c_int;
    fn XFreeCursor(display: *mut Display, cursor: c_ulong) -> c_int;
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct XColor {
    pixel: c_ulong,
    red: c_ushort,
    green: c_ushort,
    blue: c_ushort,
    flags: c_char,
    pad: c_char,
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

/// The shared prefix and coordinate layout of X keyboard, button, and motion events.
#[repr(C)]
#[derive(Clone, Copy)]
struct XKeyEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    window: XWindow,
    root: XWindow,
    subwindow: XWindow,
    time: c_ulong,
    x: c_int,
    y: c_int,
    x_root: c_int,
    y_root: c_int,
    state: c_uint,
    keycode: c_uint,
    same_screen: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XButtonEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    window: XWindow,
    root: XWindow,
    subwindow: XWindow,
    time: c_ulong,
    x: c_int,
    y: c_int,
    x_root: c_int,
    y_root: c_int,
    state: c_uint,
    button: c_uint,
    same_screen: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XMotionEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    window: XWindow,
    root: XWindow,
    subwindow: XWindow,
    time: c_ulong,
    x: c_int,
    y: c_int,
    x_root: c_int,
    y_root: c_int,
    state: c_uint,
    is_hint: c_char,
    same_screen: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XFocusChangeEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    window: XWindow,
    mode: c_int,
    detail: c_int,
}

/// The generic Xlib event union, sized by its libX11 padding member.
#[repr(C)]
union XEvent {
    kind: c_int,
    key: XKeyEvent,
    button: XButtonEvent,
    motion: XMotionEvent,
    focus_change: XFocusChangeEvent,
    configure: XConfigureEvent,
    client_message: XClientMessageEvent,
    padding: [c_long; XEVENT_PADDING_WORDS],
}

struct WindowState {
    width: Cell<u32>,
    height: Cell<u32>,
    closed: Cell<bool>,
    destroyed: Cell<bool>,
    pending_sync: Cell<Option<XSyncValue>>,
    input: RefCell<VecDeque<InputEvent>>,
    modifiers: Cell<Modifiers>,
    focused: Cell<bool>,
    pointer_position: Cell<LogicalPosition>,
    pressed_keys: [Cell<u64>; 4],
    capture_requested: Cell<bool>,
    capture_engaged: Cell<bool>,
}

impl WindowState {
    fn push_input(&self, event: InputEvent) {
        self.input.borrow_mut().push_back(event);
    }

    fn key_slot(&self, evdev_code: u32) -> Option<(&Cell<u64>, u64)> {
        let slot = self.pressed_keys.get(evdev_code as usize / 64)?;
        Some((slot, 1_u64 << (evdev_code % 64)))
    }

    fn key_is_pressed(&self, evdev_code: u32) -> bool {
        self.key_slot(evdev_code)
            .is_some_and(|(slot, bit)| slot.get() & bit != 0)
    }

    fn set_key_pressed(&self, evdev_code: u32, pressed: bool) {
        if let Some((slot, bit)) = self.key_slot(evdev_code) {
            if pressed {
                slot.set(slot.get() | bit);
            } else {
                slot.set(slot.get() & !bit);
            }
        }
    }

    fn clear_pressed_keys(&self) {
        for slot in &self.pressed_keys {
            slot.set(0);
        }
    }
}

pub(super) struct Window {
    display: *mut Display,
    handle: XWindow,
    wm_protocols: Atom,
    wm_delete: Atom,
    wm_sync_request: Atom,
    sync_counter: c_ulong,
    invisible_cursor: Cell<c_ulong>,
    state: WindowState,
}

impl Window {
    pub(super) fn new(
        title: &str,
        width: u32,
        height: u32,
        visible: bool,
    ) -> Result<Self, PlatformError> {
        let title = CString::new(title).map_err(|_| {
            PlatformError::invalid_request("X11 window title contains an interior NUL")
        })?;
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
            invisible_cursor: Cell::new(0),
            state: WindowState {
                width: Cell::new(width),
                height: Cell::new(height),
                closed: Cell::new(false),
                destroyed: Cell::new(false),
                pending_sync: Cell::new(None),
                input: RefCell::new(VecDeque::new()),
                modifiers: Cell::new(Modifiers::default()),
                focused: Cell::new(false),
                pointer_position: Cell::new(LogicalPosition::default()),
                pressed_keys: [const { Cell::new(0) }; 4],
                capture_requested: Cell::new(false),
                capture_engaged: Cell::new(false),
            },
        };
        // SAFETY: The display/window are live, the title is NUL-terminated, and the event mask
        // requests the structure, keyboard, pointer, and focus notifications this module consumes.
        // Detectable auto-repeat turns held-key repeats into consecutive KeyPress events; where a
        // server lacks it, repeats degrade to release/press pairs instead of being misreported.
        unsafe {
            XStoreName(window.display, window.handle, title.as_ptr());
            // XStoreName writes the legacy Latin-1 WM_NAME; window managers read non-ASCII
            // titles from the UTF-8 _NET_WM_NAME property instead.
            let net_wm_name = XInternAtom(window.display, c"_NET_WM_NAME".as_ptr(), FALSE);
            let utf8_string = XInternAtom(window.display, c"UTF8_STRING".as_ptr(), FALSE);
            if net_wm_name != 0 && utf8_string != 0 {
                XChangeProperty(
                    window.display,
                    window.handle,
                    net_wm_name,
                    utf8_string,
                    8,
                    PROP_MODE_REPLACE,
                    title.as_bytes().as_ptr(),
                    c_int::try_from(title.as_bytes().len()).unwrap_or(0),
                );
            }
            XSelectInput(
                window.display,
                window.handle,
                STRUCTURE_NOTIFY_MASK
                    | KEY_PRESS_MASK
                    | KEY_RELEASE_MASK
                    | BUTTON_PRESS_MASK
                    | BUTTON_RELEASE_MASK
                    | POINTER_MOTION_MASK
                    | FOCUS_CHANGE_MASK,
            );
            let mut detectable_supported = 0;
            XkbSetDetectableAutoRepeat(window.display, TRUE, &raw mut detectable_supported);
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

    /// Applies one structure or input event to the window state; returns true for `MapNotify`.
    fn handle_event(&self, event: &XEvent) -> bool {
        // SAFETY: Every Xlib event begins with the c_int type discriminant.
        match unsafe { event.kind } {
            KEY_PRESS | KEY_RELEASE => {
                // SAFETY: The discriminant identifies the key member as initialized.
                let key = unsafe { event.key };
                self.handle_key_event(&key);
                false
            }
            BUTTON_PRESS | BUTTON_RELEASE => {
                // SAFETY: The discriminant identifies the button member as initialized.
                let button = unsafe { event.button };
                self.handle_button_event(&button);
                false
            }
            MOTION_NOTIFY => {
                // SAFETY: The discriminant identifies the motion member as initialized.
                let motion = unsafe { event.motion };
                self.handle_motion_event(&motion);
                false
            }
            FOCUS_IN | FOCUS_OUT => {
                // SAFETY: The discriminant identifies the focus-change member as initialized.
                let focus = unsafe { event.focus_change };
                // Grab-initiated focus excursions (window-manager keyboard grabs such as
                // alt-tab overlays) are transient and are not application focus changes.
                if focus.mode == NOTIFY_NORMAL || focus.mode == NOTIFY_WHILE_GRABBED {
                    self.handle_focus_event(focus.kind == FOCUS_IN);
                }
                false
            }
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
                // The server already destroyed the window (an external client can do this
                // directly); its handle and grab are gone and must not be touched again.
                self.state.closed.set(true);
                self.state.destroyed.set(true);
                self.state.capture_engaged.set(false);
                false
            }
            MAP_NOTIFY => true,
            _ => false,
        }
    }

    fn handle_key_event(&self, event: &XKeyEvent) {
        let evdev_code = event.keycode.saturating_sub(X_KEYCODE_EVDEV_OFFSET);
        if keymap::is_modifier_key(evdev_code) {
            // The event's state mask predates its own transition; the live query reflects it.
            self.sync_modifiers(self.query_modifiers());
            return;
        }
        let modifiers = modifiers_from_x_state(event.state);
        self.sync_modifiers(modifiers);
        let pressed = event.kind == KEY_PRESS;
        let repeat = pressed && self.state.key_is_pressed(evdev_code);
        self.state.set_key_pressed(evdev_code, pressed);
        self.state.push_input(InputEvent::Keyboard {
            key: keymap::evdev_key_code(evdev_code),
            state: if pressed {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            },
            repeat,
            modifiers,
        });
    }

    fn handle_button_event(&self, event: &XButtonEvent) {
        let modifiers = modifiers_from_x_state(event.state);
        self.sync_modifiers(modifiers);
        let position = LogicalPosition::new(f64::from(event.x), f64::from(event.y));
        self.state.pointer_position.set(position);
        if (SCROLL_BUTTON_FIRST..=SCROLL_BUTTON_LAST).contains(&event.button) {
            // Core X delivers wheel detents as button presses four through seven; the paired
            // release carries no scroll information.
            if event.kind == BUTTON_PRESS {
                let (x, y) = scroll_steps(event.button);
                self.state.push_input(InputEvent::Scroll {
                    delta: ScrollDelta::Coarse { x, y },
                    position,
                    modifiers,
                });
            }
            return;
        }
        self.state.push_input(InputEvent::PointerButton {
            button: x11_pointer_button(event.button),
            state: if event.kind == BUTTON_PRESS {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            },
            position,
            modifiers,
        });
    }

    fn handle_motion_event(&self, event: &XMotionEvent) {
        let modifiers = modifiers_from_x_state(event.state);
        self.sync_modifiers(modifiers);
        if self.state.capture_engaged.get() {
            self.handle_captured_motion(event.x, event.y, modifiers);
            return;
        }
        let position = LogicalPosition::new(f64::from(event.x), f64::from(event.y));
        self.state.pointer_position.set(position);
        self.state.push_input(InputEvent::PointerMoved {
            position,
            modifiers,
        });
    }

    /// Converts grabbed motion into deltas against the window center and warps back to it.
    ///
    /// The motion generated by the warp itself lands exactly on the center and is filtered out
    /// rather than reported as a mirrored delta.
    fn handle_captured_motion(&self, x: c_int, y: c_int, modifiers: Modifiers) {
        let (center_x, center_y) = self.capture_center();
        if x == center_x && y == center_y {
            return;
        }
        self.state.push_input(InputEvent::PointerDelta {
            delta_x: f64::from(x - center_x),
            delta_y: f64::from(y - center_y),
            modifiers,
        });
        self.warp_to_center();
    }

    fn handle_focus_event(&self, focused: bool) {
        if self.state.focused.replace(focused) == focused {
            return;
        }
        self.state.push_input(InputEvent::FocusChanged { focused });
        if focused {
            if self.state.capture_requested.get() && !self.state.capture_engaged.get() {
                // Reapplying a stored capture intent is best-effort; a concurrent grab by
                // another client leaves the intent pending for the next focus gain.
                let _ = self.engage_grab();
            }
        } else {
            self.state.clear_pressed_keys();
            self.state.modifiers.set(Modifiers::default());
            self.release_grab();
        }
    }

    fn sync_modifiers(&self, modifiers: Modifiers) {
        if self.state.modifiers.replace(modifiers) != modifiers {
            self.state
                .push_input(InputEvent::ModifiersChanged(modifiers));
        }
    }

    /// Reads the live modifier mask, which unlike event state includes the current transition.
    fn query_modifiers(&self) -> Modifiers {
        let mut root = 0;
        let mut child = 0;
        let mut root_x = 0;
        let mut root_y = 0;
        let mut window_x = 0;
        let mut window_y = 0;
        let mut mask: c_uint = 0;
        // SAFETY: The display and window are live and every output pointer is writable.
        unsafe {
            XQueryPointer(
                self.display,
                self.handle,
                &raw mut root,
                &raw mut child,
                &raw mut root_x,
                &raw mut root_y,
                &raw mut window_x,
                &raw mut window_y,
                &raw mut mask,
            );
        }
        modifiers_from_x_state(mask)
    }

    pub(super) fn cursor_mode(&self) -> CursorMode {
        if self.state.capture_requested.get() {
            CursorMode::Captured
        } else {
            CursorMode::Normal
        }
    }

    pub(super) fn set_cursor_mode(&self, mode: CursorMode) -> Result<(), PlatformError> {
        match mode {
            CursorMode::Captured => {
                if self.state.capture_requested.get() {
                    return Ok(());
                }
                self.engage_grab()?;
                self.state.capture_requested.set(true);
                Ok(())
            }
            CursorMode::Normal => {
                self.state.capture_requested.set(false);
                self.release_grab();
                Ok(())
            }
        }
    }

    /// Grabs the pointer confined to the window with an invisible cursor and centers it.
    fn engage_grab(&self) -> Result<(), PlatformError> {
        let cursor = self.ensure_invisible_cursor()?;
        let event_mask = BUTTON_PRESS_MASK | BUTTON_RELEASE_MASK | POINTER_MOTION_MASK;
        // SAFETY: The display, window, and cursor are live; the mask covers only pointer events.
        let status = unsafe {
            XGrabPointer(
                self.display,
                self.handle,
                FALSE,
                c_uint::try_from(event_mask).expect("pointer event mask fits c_uint"),
                GRAB_MODE_ASYNC,
                GRAB_MODE_ASYNC,
                self.handle,
                cursor,
                CURRENT_TIME,
            )
        };
        if status != GRAB_SUCCESS {
            return Err(PlatformError::new(format!(
                "XGrabPointer failed with grab status {status}"
            )));
        }
        self.state.capture_engaged.set(true);
        self.warp_to_center();
        // SAFETY: The display connection is live and buffered requests must reach the server.
        unsafe { XFlush(self.display) };
        Ok(())
    }

    fn release_grab(&self) {
        if !self.state.capture_engaged.replace(false) {
            return;
        }
        // SAFETY: This client owns the active pointer grab; ungrabbing restores the cursor
        // because the invisible cursor only rode on the grab itself.
        unsafe {
            XUngrabPointer(self.display, CURRENT_TIME);
            XFlush(self.display);
        }
    }

    fn capture_center(&self) -> (c_int, c_int) {
        (
            i32::try_from(self.state.width.get() / 2).unwrap_or(i32::MAX),
            i32::try_from(self.state.height.get() / 2).unwrap_or(i32::MAX),
        )
    }

    fn warp_to_center(&self) {
        let (center_x, center_y) = self.capture_center();
        // SAFETY: The display and destination window are live; a zero source window applies the
        // move unconditionally.
        unsafe {
            XWarpPointer(self.display, 0, self.handle, 0, 0, 0, 0, center_x, center_y);
        }
    }

    /// Returns the cached fully transparent cursor, creating it on first capture.
    fn ensure_invisible_cursor(&self) -> Result<c_ulong, PlatformError> {
        let existing = self.invisible_cursor.get();
        if existing != 0 {
            return Ok(existing);
        }
        let empty: [c_char; 1] = [0];
        // SAFETY: The display and window are live and the bitmap data covers one 1x1 cell.
        let pixmap =
            unsafe { XCreateBitmapFromData(self.display, self.handle, empty.as_ptr(), 1, 1) };
        if pixmap == 0 {
            return Err(PlatformError::new(
                "XCreateBitmapFromData returned no pixmap for the invisible cursor",
            ));
        }
        let mut color = XColor::default();
        // SAFETY: The pixmap is live and serves as both shape and mask, so no pixel is drawn.
        let cursor = unsafe {
            XCreatePixmapCursor(
                self.display,
                pixmap,
                pixmap,
                &raw mut color,
                &raw mut color,
                0,
                0,
            )
        };
        // SAFETY: The cursor retains its own copy; the source pixmap is no longer needed.
        unsafe { XFreePixmap(self.display, pixmap) };
        if cursor == 0 {
            return Err(PlatformError::new(
                "XCreatePixmapCursor returned no invisible cursor",
            ));
        }
        self.invisible_cursor.set(cursor);
        Ok(cursor)
    }

    pub(super) fn next_input_event(&self) -> Option<InputEvent> {
        self.state.input.borrow_mut().pop_front()
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
        self.release_grab();
        // SAFETY: This value owns both handles and Vulkan destroys its surface before this drop.
        // A window the server already destroyed must not be destroyed again; Xlib treats the
        // resulting BadWindow as fatal through its process-exiting default error handler.
        unsafe {
            if self.invisible_cursor.get() != 0 {
                XFreeCursor(self.display, self.invisible_cursor.get());
            }
            if self.sync_counter != 0 {
                XSyncDestroyCounter(self.display, self.sync_counter);
            }
            if self.handle != 0 && !self.state.destroyed.get() {
                XDestroyWindow(self.display, self.handle);
            }
            if !self.display.is_null() {
                XCloseDisplay(self.display);
            }
        }
    }
}

fn modifiers_from_x_state(state: c_uint) -> Modifiers {
    // Shift, Lock, and Control occupy fixed core mask bits; Mod1 as Alt and Mod4 as Super is the
    // universal XKB convention on evdev-based servers, recorded as such in the input contract.
    let mut bits = 0;
    if state & SHIFT_MASK != 0 {
        bits |= Modifiers::SHIFT;
    }
    if state & CONTROL_MASK != 0 {
        bits |= Modifiers::CONTROL;
    }
    if state & MOD1_MASK != 0 {
        bits |= Modifiers::ALT;
    }
    if state & MOD4_MASK != 0 {
        bits |= Modifiers::SUPER;
    }
    if state & LOCK_MASK != 0 {
        bits |= Modifiers::CAPS_LOCK;
    }
    Modifiers::from_bits(bits)
}

fn x11_pointer_button(button: c_uint) -> PointerButton {
    match button {
        1 => PointerButton::Primary,
        2 => PointerButton::Middle,
        3 => PointerButton::Secondary,
        8 => PointerButton::Other(3),
        9 => PointerButton::Other(4),
        other => PointerButton::Other(u16::try_from(other).unwrap_or(u16::MAX)),
    }
}

/// One coarse step per core scroll button, wheel-forward and scroll-right positive.
const fn scroll_steps(button: c_uint) -> (f64, f64) {
    match button {
        4 => (0.0, 1.0),
        5 => (0.0, -1.0),
        6 => (-1.0, 0.0),
        7 => (1.0, 0.0),
        _ => (0.0, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CONTROL_MASK, LOCK_MASK, MOD1_MASK, MOD4_MASK, SHIFT_MASK, modifiers_from_x_state,
        scroll_steps, x11_pointer_button,
    };
    use crate::PointerButton;

    #[test]
    fn core_modifier_masks_translate_to_aggregate_modifiers() {
        let modifiers = modifiers_from_x_state(SHIFT_MASK | MOD1_MASK | MOD4_MASK);
        assert!(modifiers.shift());
        assert!(modifiers.alt());
        assert!(modifiers.super_key());
        assert!(!modifiers.control());
        let locked = modifiers_from_x_state(CONTROL_MASK | LOCK_MASK);
        assert!(locked.control());
        assert!(locked.caps_lock());
        assert!(!locked.shift());
    }

    #[test]
    fn pointer_buttons_skip_scroll_range_and_preserve_extended_identity() {
        assert_eq!(x11_pointer_button(1), PointerButton::Primary);
        assert_eq!(x11_pointer_button(2), PointerButton::Middle);
        assert_eq!(x11_pointer_button(3), PointerButton::Secondary);
        assert_eq!(x11_pointer_button(8), PointerButton::Other(3));
        assert_eq!(x11_pointer_button(9), PointerButton::Other(4));
    }

    #[test]
    fn scroll_buttons_report_wheel_forward_and_right_positive() {
        assert_eq!(scroll_steps(4), (0.0, 1.0));
        assert_eq!(scroll_steps(5), (0.0, -1.0));
        assert_eq!(scroll_steps(6), (-1.0, 0.0));
        assert_eq!(scroll_steps(7), (1.0, 0.0));
    }
}
