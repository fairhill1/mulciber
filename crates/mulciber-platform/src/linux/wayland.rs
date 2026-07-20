use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::time::{Duration, Instant};

use super::keymap;
use crate::{
    ButtonState, CursorMode, InputEvent, KeyCode, LogicalPosition, Modifiers, PlatformError,
    PlatformErrorKind, PointerButton, ScrollDelta, WindowMode,
};

#[repr(C)]
struct WlDisplay {
    _unused: [u8; 0],
}

#[repr(C)]
struct WlSurface {
    _unused: [u8; 0],
}

/// The libwayland `wl_array` layout used by `xdg_toplevel.configure` to carry its state set.
#[repr(C)]
struct WlArray {
    size: usize,
    alloc: usize,
    data: *mut c_void,
}

const WL_DISPLAY_GET_REGISTRY: u32 = 1;
const WL_REGISTRY_BIND: u32 = 0;
const WL_COMPOSITOR_CREATE_SURFACE: u32 = 0;
const WL_SURFACE_DESTROY: u32 = 0;
const WL_SURFACE_COMMIT: u32 = 6;
const XDG_WM_BASE_DESTROY: u32 = 0;
const XDG_WM_BASE_GET_XDG_SURFACE: u32 = 2;
const XDG_WM_BASE_PONG: u32 = 3;
const XDG_SURFACE_DESTROY: u32 = 0;
const XDG_SURFACE_GET_TOPLEVEL: u32 = 1;
const XDG_SURFACE_ACK_CONFIGURE: u32 = 4;
const XDG_TOPLEVEL_DESTROY: u32 = 0;
const XDG_TOPLEVEL_SET_TITLE: u32 = 2;
const XDG_TOPLEVEL_SET_APP_ID: u32 = 3;
const XDG_TOPLEVEL_SET_FULLSCREEN: u32 = 11;
const XDG_TOPLEVEL_UNSET_FULLSCREEN: u32 = 12;
const XDG_TOPLEVEL_STATE_FULLSCREEN: u32 = 2;
const ZXDG_DECORATION_MANAGER_DESTROY: u32 = 0;
const ZXDG_DECORATION_MANAGER_GET_TOPLEVEL_DECORATION: u32 = 1;
const ZXDG_TOPLEVEL_DECORATION_DESTROY: u32 = 0;
const ZXDG_TOPLEVEL_DECORATION_SET_MODE: u32 = 1;
const ZXDG_TOPLEVEL_DECORATION_MODE_SERVER_SIDE: u32 = 2;
const WL_MARSHAL_FLAG_DESTROY: u32 = 1;
const WL_SEAT_GET_POINTER: u32 = 0;
const WL_SEAT_GET_KEYBOARD: u32 = 1;
const WL_SEAT_RELEASE: u32 = 3;
const WL_SEAT_RELEASE_SINCE: u32 = 5;
const WL_SEAT_CAPABILITY_POINTER: u32 = 1;
const WL_SEAT_CAPABILITY_KEYBOARD: u32 = 2;
/// Seat protocol level this module speaks: version 5 adds pointer frames and discrete axis steps.
const WL_SEAT_BIND_VERSION_LIMIT: u32 = 5;
const WL_POINTER_SET_CURSOR: u32 = 0;
const WL_POINTER_RELEASE: u32 = 1;
const WL_KEYBOARD_RELEASE: u32 = 0;
const WL_INPUT_RELEASE_SINCE: u32 = 3;
const WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1: u32 = 1;
const WL_KEYBOARD_KEY_STATE_PRESSED: u32 = 1;
const WL_POINTER_AXIS_VERTICAL: u32 = 0;
const WL_POINTER_AXIS_HORIZONTAL: u32 = 1;
const WL_POINTER_AXIS_SOURCE_FINGER: u32 = 1;
const WL_POINTER_AXIS_SOURCE_CONTINUOUS: u32 = 2;
const WL_POINTER_FRAME_SINCE: u32 = 5;
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;
/// One coarse wheel detent in Wayland axis units when no discrete steps accompany the frame.
const WL_AXIS_UNITS_PER_STEP: f64 = 15.0;
const ZWP_RELATIVE_POINTER_MANAGER_DESTROY: u32 = 0;
const ZWP_RELATIVE_POINTER_MANAGER_GET_RELATIVE_POINTER: u32 = 1;
const ZWP_RELATIVE_POINTER_DESTROY: u32 = 0;
const ZWP_POINTER_CONSTRAINTS_DESTROY: u32 = 0;
const ZWP_POINTER_CONSTRAINTS_LOCK_POINTER: u32 = 1;
const ZWP_POINTER_CONSTRAINTS_LIFETIME_PERSISTENT: u32 = 2;
const ZWP_LOCKED_POINTER_DESTROY: u32 = 0;
const WP_CURSOR_SHAPE_MANAGER_DESTROY: u32 = 0;
const WP_CURSOR_SHAPE_MANAGER_GET_POINTER: u32 = 1;
const WP_CURSOR_SHAPE_DEVICE_DESTROY: u32 = 0;
const WP_CURSOR_SHAPE_DEVICE_SET_SHAPE: u32 = 1;
const WP_CURSOR_SHAPE_DEVICE_SHAPE_DEFAULT: u32 = 1;
const XKB_KEYMAP_FORMAT_TEXT_V1: c_int = 1;
const XKB_MOD_INVALID: u32 = u32::MAX;
const PROT_READ: c_int = 0x1;
const MAP_PRIVATE: c_int = 0x02;
const POLLIN: i16 = 0x0001;
const POLLERR: i16 = 0x0008;
const POLLHUP: i16 = 0x0010;
const POLLNVAL: i16 = 0x0020;

#[repr(C)]
struct WlInterface {
    name: *const c_char,
    version: c_int,
    method_count: c_int,
    methods: *const WlMessage,
    event_count: c_int,
    events: *const WlMessage,
}

// SAFETY: Protocol interface metadata is immutable for the entire process.
unsafe impl Sync for WlInterface {}

#[repr(C)]
struct WlMessage {
    name: *const c_char,
    signature: *const c_char,
    types: *const *const WlInterface,
}

// SAFETY: Protocol message metadata and its referenced strings are immutable and static.
unsafe impl Sync for WlMessage {}

struct InterfaceTypes<const N: usize>([*const WlInterface; N]);

// SAFETY: Every pointer targets immutable protocol metadata with process lifetime.
unsafe impl<const N: usize> Sync for InterfaceTypes<N> {}

static XDG_WM_BASE_GET_SURFACE_TYPES: InterfaceTypes<2> = InterfaceTypes([
    &raw const XDG_SURFACE_INTERFACE,
    &raw const wl_surface_interface,
]);
static XDG_SURFACE_GET_TOPLEVEL_TYPES: InterfaceTypes<1> =
    InterfaceTypes([&raw const XDG_TOPLEVEL_INTERFACE]);
static ZXDG_DECORATION_GET_TOPLEVEL_TYPES: InterfaceTypes<2> = InterfaceTypes([
    &raw const ZXDG_TOPLEVEL_DECORATION_INTERFACE,
    &raw const XDG_TOPLEVEL_INTERFACE,
]);

static XDG_WM_BASE_REQUESTS: [WlMessage; 4] = [
    message(c"destroy", c""),
    message(c"create_positioner", c"n"),
    typed_message(c"get_xdg_surface", c"no", &XDG_WM_BASE_GET_SURFACE_TYPES.0),
    message(c"pong", c"u"),
];
static XDG_WM_BASE_EVENTS: [WlMessage; 1] = [message(c"ping", c"u")];
static XDG_WM_BASE_INTERFACE: WlInterface =
    interface(c"xdg_wm_base", &XDG_WM_BASE_REQUESTS, &XDG_WM_BASE_EVENTS);

static XDG_SURFACE_REQUESTS: [WlMessage; 5] = [
    message(c"destroy", c""),
    typed_message(c"get_toplevel", c"n", &XDG_SURFACE_GET_TOPLEVEL_TYPES.0),
    message(c"get_popup", c"n?oo"),
    message(c"set_window_geometry", c"iiii"),
    message(c"ack_configure", c"u"),
];
static XDG_SURFACE_EVENTS: [WlMessage; 1] = [message(c"configure", c"u")];
static XDG_SURFACE_INTERFACE: WlInterface =
    interface(c"xdg_surface", &XDG_SURFACE_REQUESTS, &XDG_SURFACE_EVENTS);

static XDG_TOPLEVEL_REQUESTS: [WlMessage; 14] = [
    message(c"destroy", c""),
    message(c"set_parent", c"?o"),
    message(c"set_title", c"s"),
    message(c"set_app_id", c"s"),
    message(c"show_window_menu", c"ouii"),
    message(c"move", c"ou"),
    message(c"resize", c"ouu"),
    message(c"set_max_size", c"ii"),
    message(c"set_min_size", c"ii"),
    message(c"set_maximized", c""),
    message(c"unset_maximized", c""),
    message(c"set_fullscreen", c"?o"),
    message(c"unset_fullscreen", c""),
    message(c"set_minimized", c""),
];
static XDG_TOPLEVEL_EVENTS: [WlMessage; 2] =
    [message(c"configure", c"iia"), message(c"close", c"")];
static XDG_TOPLEVEL_INTERFACE: WlInterface = interface(
    c"xdg_toplevel",
    &XDG_TOPLEVEL_REQUESTS,
    &XDG_TOPLEVEL_EVENTS,
);

static ZXDG_DECORATION_MANAGER_REQUESTS: [WlMessage; 2] = [
    message(c"destroy", c""),
    typed_message(
        c"get_toplevel_decoration",
        c"no",
        &ZXDG_DECORATION_GET_TOPLEVEL_TYPES.0,
    ),
];
static ZXDG_DECORATION_MANAGER_EVENTS: [WlMessage; 0] = [];
static ZXDG_DECORATION_MANAGER_INTERFACE: WlInterface = versioned_interface(
    c"zxdg_decoration_manager_v1",
    2,
    &ZXDG_DECORATION_MANAGER_REQUESTS,
    &ZXDG_DECORATION_MANAGER_EVENTS,
);
static ZXDG_TOPLEVEL_DECORATION_REQUESTS: [WlMessage; 3] = [
    message(c"destroy", c""),
    message(c"set_mode", c"u"),
    message(c"unset_mode", c""),
];
static ZXDG_TOPLEVEL_DECORATION_EVENTS: [WlMessage; 1] = [message(c"configure", c"u")];
static ZXDG_TOPLEVEL_DECORATION_INTERFACE: WlInterface = versioned_interface(
    c"zxdg_toplevel_decoration_v1",
    2,
    &ZXDG_TOPLEVEL_DECORATION_REQUESTS,
    &ZXDG_TOPLEVEL_DECORATION_EVENTS,
);

static REGION_ONLY_TYPES: InterfaceTypes<1> = InterfaceTypes([&raw const wl_region_interface]);
static ZWP_RELATIVE_POINTER_MANAGER_GET_TYPES: InterfaceTypes<2> = InterfaceTypes([
    &raw const ZWP_RELATIVE_POINTER_INTERFACE,
    &raw const wl_pointer_interface,
]);
static ZWP_POINTER_CONSTRAINTS_LOCK_TYPES: InterfaceTypes<5> = InterfaceTypes([
    &raw const ZWP_LOCKED_POINTER_INTERFACE,
    &raw const wl_surface_interface,
    &raw const wl_pointer_interface,
    &raw const wl_region_interface,
    ptr::null(),
]);
static ZWP_POINTER_CONSTRAINTS_CONFINE_TYPES: InterfaceTypes<5> = InterfaceTypes([
    &raw const ZWP_CONFINED_POINTER_INTERFACE,
    &raw const wl_surface_interface,
    &raw const wl_pointer_interface,
    &raw const wl_region_interface,
    ptr::null(),
]);
static WP_CURSOR_SHAPE_GET_POINTER_TYPES: InterfaceTypes<2> = InterfaceTypes([
    &raw const WP_CURSOR_SHAPE_DEVICE_INTERFACE,
    &raw const wl_pointer_interface,
]);
// The tablet-tool half of the cursor-shape protocol is never marshalled by this module, so its
// tablet interface entry stays untyped.
static WP_CURSOR_SHAPE_GET_TABLET_TYPES: InterfaceTypes<2> =
    InterfaceTypes([&raw const WP_CURSOR_SHAPE_DEVICE_INTERFACE, ptr::null()]);

static ZWP_RELATIVE_POINTER_MANAGER_REQUESTS: [WlMessage; 2] = [
    message(c"destroy", c""),
    typed_message(
        c"get_relative_pointer",
        c"no",
        &ZWP_RELATIVE_POINTER_MANAGER_GET_TYPES.0,
    ),
];
static ZWP_RELATIVE_POINTER_MANAGER_EVENTS: [WlMessage; 0] = [];
static ZWP_RELATIVE_POINTER_MANAGER_INTERFACE: WlInterface = interface(
    c"zwp_relative_pointer_manager_v1",
    &ZWP_RELATIVE_POINTER_MANAGER_REQUESTS,
    &ZWP_RELATIVE_POINTER_MANAGER_EVENTS,
);
static ZWP_RELATIVE_POINTER_REQUESTS: [WlMessage; 1] = [message(c"destroy", c"")];
static ZWP_RELATIVE_POINTER_EVENTS: [WlMessage; 1] = [message(c"relative_motion", c"uuffff")];
static ZWP_RELATIVE_POINTER_INTERFACE: WlInterface = interface(
    c"zwp_relative_pointer_v1",
    &ZWP_RELATIVE_POINTER_REQUESTS,
    &ZWP_RELATIVE_POINTER_EVENTS,
);

static ZWP_POINTER_CONSTRAINTS_REQUESTS: [WlMessage; 3] = [
    message(c"destroy", c""),
    typed_message(
        c"lock_pointer",
        c"noo?ou",
        &ZWP_POINTER_CONSTRAINTS_LOCK_TYPES.0,
    ),
    typed_message(
        c"confine_pointer",
        c"noo?ou",
        &ZWP_POINTER_CONSTRAINTS_CONFINE_TYPES.0,
    ),
];
static ZWP_POINTER_CONSTRAINTS_EVENTS: [WlMessage; 0] = [];
static ZWP_POINTER_CONSTRAINTS_INTERFACE: WlInterface = interface(
    c"zwp_pointer_constraints_v1",
    &ZWP_POINTER_CONSTRAINTS_REQUESTS,
    &ZWP_POINTER_CONSTRAINTS_EVENTS,
);
static ZWP_LOCKED_POINTER_REQUESTS: [WlMessage; 3] = [
    message(c"destroy", c""),
    message(c"set_cursor_position_hint", c"ff"),
    typed_message(c"set_region", c"?o", &REGION_ONLY_TYPES.0),
];
static ZWP_LOCKED_POINTER_EVENTS: [WlMessage; 2] =
    [message(c"locked", c""), message(c"unlocked", c"")];
static ZWP_LOCKED_POINTER_INTERFACE: WlInterface = interface(
    c"zwp_locked_pointer_v1",
    &ZWP_LOCKED_POINTER_REQUESTS,
    &ZWP_LOCKED_POINTER_EVENTS,
);
static ZWP_CONFINED_POINTER_REQUESTS: [WlMessage; 2] = [
    message(c"destroy", c""),
    typed_message(c"set_region", c"?o", &REGION_ONLY_TYPES.0),
];
static ZWP_CONFINED_POINTER_EVENTS: [WlMessage; 2] =
    [message(c"confined", c""), message(c"unconfined", c"")];
static ZWP_CONFINED_POINTER_INTERFACE: WlInterface = interface(
    c"zwp_confined_pointer_v1",
    &ZWP_CONFINED_POINTER_REQUESTS,
    &ZWP_CONFINED_POINTER_EVENTS,
);

static WP_CURSOR_SHAPE_MANAGER_REQUESTS: [WlMessage; 3] = [
    message(c"destroy", c""),
    typed_message(c"get_pointer", c"no", &WP_CURSOR_SHAPE_GET_POINTER_TYPES.0),
    typed_message(
        c"get_tablet_tool_v2",
        c"no",
        &WP_CURSOR_SHAPE_GET_TABLET_TYPES.0,
    ),
];
static WP_CURSOR_SHAPE_MANAGER_EVENTS: [WlMessage; 0] = [];
static WP_CURSOR_SHAPE_MANAGER_INTERFACE: WlInterface = interface(
    c"wp_cursor_shape_manager_v1",
    &WP_CURSOR_SHAPE_MANAGER_REQUESTS,
    &WP_CURSOR_SHAPE_MANAGER_EVENTS,
);
static WP_CURSOR_SHAPE_DEVICE_REQUESTS: [WlMessage; 2] =
    [message(c"destroy", c""), message(c"set_shape", c"uu")];
static WP_CURSOR_SHAPE_DEVICE_EVENTS: [WlMessage; 0] = [];
static WP_CURSOR_SHAPE_DEVICE_INTERFACE: WlInterface = interface(
    c"wp_cursor_shape_device_v1",
    &WP_CURSOR_SHAPE_DEVICE_REQUESTS,
    &WP_CURSOR_SHAPE_DEVICE_EVENTS,
);

const fn message(name: &'static CStr, signature: &'static CStr) -> WlMessage {
    WlMessage {
        name: name.as_ptr(),
        signature: signature.as_ptr(),
        types: ptr::null(),
    }
}

const fn typed_message(
    name: &'static CStr,
    signature: &'static CStr,
    types: &'static [*const WlInterface],
) -> WlMessage {
    WlMessage {
        name: name.as_ptr(),
        signature: signature.as_ptr(),
        types: types.as_ptr(),
    }
}

const fn interface(
    name: &'static CStr,
    methods: &'static [WlMessage],
    events: &'static [WlMessage],
) -> WlInterface {
    versioned_interface(name, 1, methods, events)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
const fn versioned_interface(
    name: &'static CStr,
    version: c_int,
    methods: &'static [WlMessage],
    events: &'static [WlMessage],
) -> WlInterface {
    WlInterface {
        name: name.as_ptr(),
        version,
        method_count: methods.len() as c_int,
        methods: methods.as_ptr(),
        event_count: events.len() as c_int,
        events: events.as_ptr(),
    }
}

#[repr(C)]
struct RegistryListener {
    global: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            registry: *mut c_void,
            name: u32,
            interface: *const c_char,
            version: u32,
        ),
    >,
    global_remove:
        Option<unsafe extern "C" fn(data: *mut c_void, registry: *mut c_void, name: u32)>,
}

// SAFETY: The listener table contains only immutable function pointers.
unsafe impl Sync for RegistryListener {}

#[repr(C)]
struct WmBaseListener {
    ping: Option<unsafe extern "C" fn(data: *mut c_void, wm_base: *mut c_void, serial: u32)>,
}

// SAFETY: The listener table contains only an immutable function pointer.
unsafe impl Sync for WmBaseListener {}

#[repr(C)]
struct XdgSurfaceListener {
    configure:
        Option<unsafe extern "C" fn(data: *mut c_void, xdg_surface: *mut c_void, serial: u32)>,
}

// SAFETY: The listener table contains only an immutable function pointer.
unsafe impl Sync for XdgSurfaceListener {}

#[repr(C)]
struct XdgToplevelListener {
    configure: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            toplevel: *mut c_void,
            width: i32,
            height: i32,
            states: *mut c_void,
        ),
    >,
    close: Option<unsafe extern "C" fn(data: *mut c_void, toplevel: *mut c_void)>,
}

// SAFETY: The listener table contains only immutable function pointers.
unsafe impl Sync for XdgToplevelListener {}

#[repr(C)]
struct DecorationListener {
    configure: Option<unsafe extern "C" fn(data: *mut c_void, decoration: *mut c_void, mode: u32)>,
}

// SAFETY: The listener table contains only an immutable function pointer.
unsafe impl Sync for DecorationListener {}

#[repr(C)]
struct SeatListener {
    capabilities:
        Option<unsafe extern "C" fn(data: *mut c_void, seat: *mut c_void, capabilities: u32)>,
    name: Option<unsafe extern "C" fn(data: *mut c_void, seat: *mut c_void, name: *const c_char)>,
}

// SAFETY: The listener table contains only immutable function pointers.
unsafe impl Sync for SeatListener {}

#[repr(C)]
struct KeyboardListener {
    keymap: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            keyboard: *mut c_void,
            format: u32,
            fd: c_int,
            size: u32,
        ),
    >,
    enter: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            keyboard: *mut c_void,
            serial: u32,
            surface: *mut c_void,
            keys: *mut c_void,
        ),
    >,
    leave: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            keyboard: *mut c_void,
            serial: u32,
            surface: *mut c_void,
        ),
    >,
    key: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            keyboard: *mut c_void,
            serial: u32,
            time: u32,
            key: u32,
            state: u32,
        ),
    >,
    modifiers: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            keyboard: *mut c_void,
            serial: u32,
            depressed: u32,
            latched: u32,
            locked: u32,
            group: u32,
        ),
    >,
    repeat_info: Option<
        unsafe extern "C" fn(data: *mut c_void, keyboard: *mut c_void, rate: i32, delay: i32),
    >,
}

// SAFETY: The listener table contains only immutable function pointers.
unsafe impl Sync for KeyboardListener {}

/// Pointer events through seat version five; `wl_fixed_t` arguments arrive as `i32`.
#[repr(C)]
struct PointerListener {
    enter: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            pointer: *mut c_void,
            serial: u32,
            surface: *mut c_void,
            surface_x: i32,
            surface_y: i32,
        ),
    >,
    leave: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            pointer: *mut c_void,
            serial: u32,
            surface: *mut c_void,
        ),
    >,
    motion: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            pointer: *mut c_void,
            time: u32,
            surface_x: i32,
            surface_y: i32,
        ),
    >,
    button: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            pointer: *mut c_void,
            serial: u32,
            time: u32,
            button: u32,
            state: u32,
        ),
    >,
    axis: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            pointer: *mut c_void,
            time: u32,
            axis: u32,
            value: i32,
        ),
    >,
    frame: Option<unsafe extern "C" fn(data: *mut c_void, pointer: *mut c_void)>,
    axis_source: Option<unsafe extern "C" fn(data: *mut c_void, pointer: *mut c_void, source: u32)>,
    axis_stop:
        Option<unsafe extern "C" fn(data: *mut c_void, pointer: *mut c_void, time: u32, axis: u32)>,
    axis_discrete: Option<
        unsafe extern "C" fn(data: *mut c_void, pointer: *mut c_void, axis: u32, discrete: i32),
    >,
}

// SAFETY: The listener table contains only immutable function pointers.
unsafe impl Sync for PointerListener {}

#[repr(C)]
struct RelativePointerListener {
    relative_motion: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            relative_pointer: *mut c_void,
            utime_hi: u32,
            utime_lo: u32,
            dx: i32,
            dy: i32,
            dx_unaccel: i32,
            dy_unaccel: i32,
        ),
    >,
}

// SAFETY: The listener table contains only an immutable function pointer.
unsafe impl Sync for RelativePointerListener {}

#[repr(C)]
struct LockedPointerListener {
    locked: Option<unsafe extern "C" fn(data: *mut c_void, locked_pointer: *mut c_void)>,
    unlocked: Option<unsafe extern "C" fn(data: *mut c_void, locked_pointer: *mut c_void)>,
}

// SAFETY: The listener table contains only immutable function pointers.
unsafe impl Sync for LockedPointerListener {}

static REGISTRY_LISTENER: RegistryListener = RegistryListener {
    global: Some(registry_global),
    global_remove: Some(registry_global_remove),
};
static WM_BASE_LISTENER: WmBaseListener = WmBaseListener {
    ping: Some(wm_base_ping),
};
static XDG_SURFACE_LISTENER: XdgSurfaceListener = XdgSurfaceListener {
    configure: Some(xdg_surface_configure),
};
static XDG_TOPLEVEL_LISTENER: XdgToplevelListener = XdgToplevelListener {
    configure: Some(xdg_toplevel_configure),
    close: Some(xdg_toplevel_close),
};
static DECORATION_LISTENER: DecorationListener = DecorationListener {
    configure: Some(decoration_configure),
};
static SEAT_LISTENER: SeatListener = SeatListener {
    capabilities: Some(seat_capabilities),
    name: Some(seat_name),
};
static KEYBOARD_LISTENER: KeyboardListener = KeyboardListener {
    keymap: Some(keyboard_keymap),
    enter: Some(keyboard_enter),
    leave: Some(keyboard_leave),
    key: Some(keyboard_key),
    modifiers: Some(keyboard_modifiers),
    repeat_info: Some(keyboard_repeat_info),
};
static POINTER_LISTENER: PointerListener = PointerListener {
    enter: Some(pointer_enter),
    leave: Some(pointer_leave),
    motion: Some(pointer_motion),
    button: Some(pointer_button),
    axis: Some(pointer_axis),
    frame: Some(pointer_frame),
    axis_source: Some(pointer_axis_source),
    axis_stop: Some(pointer_axis_stop),
    axis_discrete: Some(pointer_axis_discrete),
};
static RELATIVE_POINTER_LISTENER: RelativePointerListener = RelativePointerListener {
    relative_motion: Some(relative_pointer_motion),
};
static LOCKED_POINTER_LISTENER: LockedPointerListener = LockedPointerListener {
    locked: Some(locked_pointer_locked),
    unlocked: Some(locked_pointer_unlocked),
};

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

#[link(name = "wayland-client")]
unsafe extern "C" {
    static wl_registry_interface: WlInterface;
    static wl_compositor_interface: WlInterface;
    static wl_surface_interface: WlInterface;
    static wl_seat_interface: WlInterface;
    static wl_pointer_interface: WlInterface;
    static wl_keyboard_interface: WlInterface;
    static wl_region_interface: WlInterface;

    fn wl_display_connect(name: *const c_char) -> *mut WlDisplay;
    fn wl_display_disconnect(display: *mut WlDisplay);
    fn wl_display_roundtrip(display: *mut WlDisplay) -> c_int;
    fn wl_display_dispatch_pending(display: *mut WlDisplay) -> c_int;
    fn wl_display_flush(display: *mut WlDisplay) -> c_int;
    fn wl_display_prepare_read(display: *mut WlDisplay) -> c_int;
    fn wl_display_read_events(display: *mut WlDisplay) -> c_int;
    fn wl_display_cancel_read(display: *mut WlDisplay);
    fn wl_display_get_fd(display: *mut WlDisplay) -> c_int;
    fn wl_display_get_error(display: *mut WlDisplay) -> c_int;
    fn wl_proxy_get_version(proxy: *mut c_void) -> u32;
    fn wl_proxy_add_listener(
        proxy: *mut c_void,
        implementation: *const c_void,
        data: *mut c_void,
    ) -> c_int;
    fn wl_proxy_marshal_flags(
        proxy: *mut c_void,
        opcode: u32,
        interface: *const WlInterface,
        version: u32,
        flags: u32,
        ...
    ) -> *mut c_void;
    fn wl_proxy_destroy(proxy: *mut c_void);
}

unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
    fn mmap(
        addr: *mut c_void,
        length: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, length: usize) -> c_int;
    fn close(fd: c_int) -> c_int;
}

#[repr(C)]
struct XkbContext {
    _unused: [u8; 0],
}

#[repr(C)]
struct XkbKeymap {
    _unused: [u8; 0],
}

// The Wayland keyboard contract hands every client an xkb keymap; libxkbcommon is the platform's
// canonical parser for it and is used here only to resolve modifier-mask bit positions.
#[link(name = "xkbcommon")]
unsafe extern "C" {
    fn xkb_context_new(flags: c_int) -> *mut XkbContext;
    fn xkb_context_unref(context: *mut XkbContext);
    fn xkb_keymap_new_from_string(
        context: *mut XkbContext,
        string: *const c_char,
        format: c_int,
        flags: c_int,
    ) -> *mut XkbKeymap;
    fn xkb_keymap_unref(keymap: *mut XkbKeymap);
    fn xkb_keymap_mod_get_index(keymap: *mut XkbKeymap, name: *const c_char) -> u32;
}

#[derive(Default)]
struct RegistryState {
    compositor_name: Option<u32>,
    compositor_version: u32,
    wm_base_name: Option<u32>,
    wm_base_version: u32,
    decoration_manager_name: Option<u32>,
    decoration_manager_version: u32,
    seat_name: Option<u32>,
    seat_version: u32,
    relative_pointer_manager_name: Option<u32>,
    pointer_constraints_name: Option<u32>,
    cursor_shape_manager_name: Option<u32>,
}

/// The xkb mask bit positions of the aggregate modifiers, or [`XKB_MOD_INVALID`] when the current
/// keymap does not define one.
#[derive(Clone, Copy)]
struct ModifierIndices {
    shift: u32,
    caps_lock: u32,
    control: u32,
    alt: u32,
    super_key: u32,
}

impl Default for ModifierIndices {
    fn default() -> Self {
        Self {
            shift: XKB_MOD_INVALID,
            caps_lock: XKB_MOD_INVALID,
            control: XKB_MOD_INVALID,
            alt: XKB_MOD_INVALID,
            super_key: XKB_MOD_INVALID,
        }
    }
}

struct WindowState {
    width: Cell<u32>,
    height: Cell<u32>,
    pending_width: Cell<u32>,
    pending_height: Cell<u32>,
    pending_serial: Cell<u32>,
    configured: Cell<bool>,
    pending_fullscreen: Cell<bool>,
    fullscreen_confirmed: Cell<bool>,
    fullscreen_requested: Cell<bool>,
    closed: Cell<bool>,
    decoration_mode: Cell<u32>,
    input: RefCell<VecDeque<InputEvent>>,
    pointer_proxy: Cell<*mut c_void>,
    keyboard_proxy: Cell<*mut c_void>,
    modifier_indices: Cell<ModifierIndices>,
    modifiers: Cell<Modifiers>,
    keyboard_focused: Cell<bool>,
    pointer_inside: Cell<bool>,
    pointer_enter_serial: Cell<u32>,
    pointer_position: Cell<LogicalPosition>,
    axis_precise_x: Cell<f64>,
    axis_precise_y: Cell<f64>,
    axis_steps_x: Cell<f64>,
    axis_steps_y: Cell<f64>,
    axis_source: Cell<Option<u32>>,
    axis_pending: Cell<bool>,
    repeat_rate: Cell<i32>,
    repeat_delay_ms: Cell<i32>,
    repeat_key: Cell<Option<(u32, KeyCode)>>,
    repeat_due: Cell<Option<Instant>>,
    capture_requested: Cell<bool>,
    capture_active: Cell<bool>,
}

impl WindowState {
    fn push_input(&self, event: InputEvent) {
        self.input.borrow_mut().push_back(event);
    }
}

pub(super) struct Window {
    display: *mut WlDisplay,
    registry: *mut c_void,
    compositor: *mut c_void,
    surface: *mut WlSurface,
    wm_base: *mut c_void,
    decoration_manager: *mut c_void,
    xdg_surface: *mut c_void,
    toplevel: *mut c_void,
    decoration: *mut c_void,
    seat: *mut c_void,
    relative_pointer_manager: *mut c_void,
    pointer_constraints: *mut c_void,
    cursor_shape_manager: *mut c_void,
    relative_pointer: Cell<*mut c_void>,
    locked_pointer: Cell<*mut c_void>,
    cursor_shape_device: Cell<*mut c_void>,
    registry_state: Box<RegistryState>,
    state: Box<WindowState>,
}

impl Window {
    pub(super) fn new(
        title: &str,
        width: u32,
        height: u32,
        _visible: bool,
    ) -> Result<Self, PlatformError> {
        let title = CString::new(title).map_err(|_| {
            PlatformError::invalid_request("Wayland window title contains an interior NUL")
        })?;
        // SAFETY: A null name asks libwayland-client to use WAYLAND_DISPLAY.
        let display = unsafe { wl_display_connect(ptr::null()) };
        if display.is_null() {
            return Err(PlatformError::new(
                "wl_display_connect failed; ensure WAYLAND_DISPLAY names a reachable compositor",
            ));
        }
        let mut window = Self {
            display,
            registry: ptr::null_mut(),
            compositor: ptr::null_mut(),
            surface: ptr::null_mut(),
            wm_base: ptr::null_mut(),
            decoration_manager: ptr::null_mut(),
            xdg_surface: ptr::null_mut(),
            toplevel: ptr::null_mut(),
            decoration: ptr::null_mut(),
            seat: ptr::null_mut(),
            relative_pointer_manager: ptr::null_mut(),
            pointer_constraints: ptr::null_mut(),
            cursor_shape_manager: ptr::null_mut(),
            relative_pointer: Cell::new(ptr::null_mut()),
            locked_pointer: Cell::new(ptr::null_mut()),
            cursor_shape_device: Cell::new(ptr::null_mut()),
            registry_state: Box::default(),
            state: Box::new(WindowState {
                width: Cell::new(width),
                height: Cell::new(height),
                pending_width: Cell::new(width),
                pending_height: Cell::new(height),
                pending_serial: Cell::new(0),
                configured: Cell::new(false),
                pending_fullscreen: Cell::new(false),
                fullscreen_confirmed: Cell::new(false),
                fullscreen_requested: Cell::new(false),
                closed: Cell::new(false),
                decoration_mode: Cell::new(0),
                input: RefCell::new(VecDeque::new()),
                pointer_proxy: Cell::new(ptr::null_mut()),
                keyboard_proxy: Cell::new(ptr::null_mut()),
                modifier_indices: Cell::new(ModifierIndices::default()),
                modifiers: Cell::new(Modifiers::default()),
                keyboard_focused: Cell::new(false),
                pointer_inside: Cell::new(false),
                pointer_enter_serial: Cell::new(0),
                pointer_position: Cell::new(LogicalPosition::default()),
                axis_precise_x: Cell::new(0.0),
                axis_precise_y: Cell::new(0.0),
                axis_steps_x: Cell::new(0.0),
                axis_steps_y: Cell::new(0.0),
                axis_source: Cell::new(None),
                axis_pending: Cell::new(false),
                repeat_rate: Cell::new(0),
                repeat_delay_ms: Cell::new(0),
                repeat_key: Cell::new(None),
                repeat_due: Cell::new(None),
                capture_requested: Cell::new(false),
                capture_active: Cell::new(false),
            }),
        };
        window.create_registry()?;
        window.bind_globals()?;
        window.create_xdg_toplevel(&title)?;
        window.await_initial_configure()?;
        Ok(window)
    }

    fn create_registry(&mut self) -> Result<(), PlatformError> {
        // SAFETY: The display proxy is live and the registry interface comes from libwayland.
        self.registry = unsafe {
            wl_proxy_marshal_flags(
                self.display.cast(),
                WL_DISPLAY_GET_REGISTRY,
                &raw const wl_registry_interface,
                wl_proxy_get_version(self.display.cast()),
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        if self.registry.is_null() {
            return Err(PlatformError::new(
                "wl_display_get_registry returned no proxy",
            ));
        }
        // SAFETY: The listener and boxed callback state outlive the registry proxy.
        if unsafe {
            wl_proxy_add_listener(
                self.registry,
                (&raw const REGISTRY_LISTENER).cast(),
                (&raw mut *self.registry_state).cast(),
            )
        } != 0
        {
            return Err(PlatformError::new("wl_registry_add_listener failed"));
        }
        // SAFETY: The display remains connected and dispatches registry callbacks synchronously.
        if unsafe { wl_display_roundtrip(self.display) } < 0 {
            return Err(self.display_error("Wayland registry roundtrip"));
        }
        Ok(())
    }

    fn bind_globals(&mut self) -> Result<(), PlatformError> {
        let compositor_name = self.registry_state.compositor_name.ok_or_else(|| {
            PlatformError::with_kind(
                crate::PlatformErrorKind::Unsupported,
                "Wayland registry exposes no wl_compositor",
            )
        })?;
        let wm_base_name = self.registry_state.wm_base_name.ok_or_else(|| {
            PlatformError::with_kind(
                crate::PlatformErrorKind::Unsupported,
                "Wayland compositor exposes no xdg_wm_base",
            )
        })?;
        // SAFETY: The registry announced both globals with at least protocol version one.
        unsafe {
            self.compositor = bind_global(
                self.registry,
                compositor_name,
                &raw const wl_compositor_interface,
                self.registry_state.compositor_version.min(1),
            );
            self.wm_base = bind_global(
                self.registry,
                wm_base_name,
                &raw const XDG_WM_BASE_INTERFACE,
                self.registry_state.wm_base_version.min(1),
            );
        }
        if self.compositor.is_null() || self.wm_base.is_null() {
            return Err(PlatformError::new("binding Wayland shell globals failed"));
        }
        if let Some(name) = self.registry_state.decoration_manager_name {
            // SAFETY: The registry announced this decoration-manager global and version.
            self.decoration_manager = unsafe {
                bind_global(
                    self.registry,
                    name,
                    &raw const ZXDG_DECORATION_MANAGER_INTERFACE,
                    self.registry_state.decoration_manager_version.min(2),
                )
            };
            if self.decoration_manager.is_null() {
                return Err(PlatformError::new(
                    "binding zxdg_decoration_manager_v1 failed",
                ));
            }
        }
        // SAFETY: The static listener outlives the xdg_wm_base proxy.
        if unsafe {
            wl_proxy_add_listener(
                self.wm_base,
                (&raw const WM_BASE_LISTENER).cast(),
                ptr::null_mut(),
            )
        } != 0
        {
            return Err(PlatformError::new("xdg_wm_base_add_listener failed"));
        }
        self.bind_input_globals()?;
        Ok(())
    }

    /// Binds the seat and the optional pointer-capture globals.
    ///
    /// A missing seat is not an error: a render-only session still presents frames, it simply
    /// delivers no input transitions and reports capture as unsupported.
    fn bind_input_globals(&mut self) -> Result<(), PlatformError> {
        if let Some(name) = self.registry_state.seat_name {
            // SAFETY: The registry announced this seat global; the version is capped to the
            // protocol level this module's listener tables cover.
            self.seat = unsafe {
                bind_global(
                    self.registry,
                    name,
                    &raw const wl_seat_interface,
                    self.registry_state
                        .seat_version
                        .min(WL_SEAT_BIND_VERSION_LIMIT),
                )
            };
            if self.seat.is_null() {
                return Err(PlatformError::new("binding wl_seat failed"));
            }
            // SAFETY: The static listener and boxed window state outlive the seat proxy.
            if unsafe {
                wl_proxy_add_listener(
                    self.seat,
                    (&raw const SEAT_LISTENER).cast(),
                    (&raw mut *self.state).cast(),
                )
            } != 0
            {
                return Err(PlatformError::new("wl_seat_add_listener failed"));
            }
        }
        // SAFETY: Each manager global was announced by the registry and is bound at version one;
        // none of them delivers events, so no listener is registered.
        unsafe {
            if let Some(name) = self.registry_state.relative_pointer_manager_name {
                self.relative_pointer_manager = bind_global(
                    self.registry,
                    name,
                    &raw const ZWP_RELATIVE_POINTER_MANAGER_INTERFACE,
                    1,
                );
            }
            if let Some(name) = self.registry_state.pointer_constraints_name {
                self.pointer_constraints = bind_global(
                    self.registry,
                    name,
                    &raw const ZWP_POINTER_CONSTRAINTS_INTERFACE,
                    1,
                );
            }
            if let Some(name) = self.registry_state.cursor_shape_manager_name {
                self.cursor_shape_manager = bind_global(
                    self.registry,
                    name,
                    &raw const WP_CURSOR_SHAPE_MANAGER_INTERFACE,
                    1,
                );
            }
        }
        Ok(())
    }

    fn create_xdg_toplevel(&mut self, title: &CStr) -> Result<(), PlatformError> {
        // SAFETY: The compositor proxy is live and supports create_surface in version one.
        self.surface = unsafe {
            wl_proxy_marshal_flags(
                self.compositor,
                WL_COMPOSITOR_CREATE_SURFACE,
                &raw const wl_surface_interface,
                wl_proxy_get_version(self.compositor),
                0,
                ptr::null_mut::<c_void>(),
            )
            .cast()
        };
        if self.surface.is_null() {
            return Err(PlatformError::new("wl_compositor_create_surface failed"));
        }
        // SAFETY: The shell and wl_surface proxies are live and unassigned to another role.
        self.xdg_surface = unsafe {
            wl_proxy_marshal_flags(
                self.wm_base,
                XDG_WM_BASE_GET_XDG_SURFACE,
                &raw const XDG_SURFACE_INTERFACE,
                wl_proxy_get_version(self.wm_base),
                0,
                ptr::null_mut::<c_void>(),
                self.surface,
            )
        };
        if self.xdg_surface.is_null() {
            return Err(PlatformError::new("xdg_wm_base_get_xdg_surface failed"));
        }
        let state = (&raw mut *self.state).cast();
        // SAFETY: The listener and boxed state outlive the xdg_surface proxy.
        if unsafe {
            wl_proxy_add_listener(
                self.xdg_surface,
                (&raw const XDG_SURFACE_LISTENER).cast(),
                state,
            )
        } != 0
        {
            return Err(PlatformError::new("xdg_surface_add_listener failed"));
        }
        // SAFETY: The xdg_surface is live and does not yet have a role object.
        self.toplevel = unsafe {
            wl_proxy_marshal_flags(
                self.xdg_surface,
                XDG_SURFACE_GET_TOPLEVEL,
                &raw const XDG_TOPLEVEL_INTERFACE,
                wl_proxy_get_version(self.xdg_surface),
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        if self.toplevel.is_null() {
            return Err(PlatformError::new("xdg_surface_get_toplevel failed"));
        }
        // SAFETY: The listener and boxed state outlive the toplevel proxy.
        if unsafe {
            wl_proxy_add_listener(
                self.toplevel,
                (&raw const XDG_TOPLEVEL_LISTENER).cast(),
                state,
            )
        } != 0
        {
            return Err(PlatformError::new("xdg_toplevel_add_listener failed"));
        }
        self.create_server_decoration()?;
        // SAFETY: Strings and proxies are live for each synchronous marshal operation.
        unsafe {
            wl_proxy_marshal_flags(
                self.toplevel,
                XDG_TOPLEVEL_SET_TITLE,
                ptr::null(),
                wl_proxy_get_version(self.toplevel),
                0,
                title.as_ptr(),
            );
            wl_proxy_marshal_flags(
                self.toplevel,
                XDG_TOPLEVEL_SET_APP_ID,
                ptr::null(),
                wl_proxy_get_version(self.toplevel),
                0,
                c"mulciber-vulkan-triangle".as_ptr(),
            );
            wl_proxy_marshal_flags(
                self.surface.cast(),
                WL_SURFACE_COMMIT,
                ptr::null(),
                wl_proxy_get_version(self.surface.cast()),
                0,
            );
        }
        Ok(())
    }

    fn create_server_decoration(&mut self) -> Result<(), PlatformError> {
        if self.decoration_manager.is_null() {
            println!(
                "Wayland decorations: compositor exposes no zxdg_decoration_manager_v1; window is client-undecorated"
            );
            return Ok(());
        }
        // SAFETY: The manager/toplevel are live and no buffer has been committed to the surface.
        self.decoration = unsafe {
            wl_proxy_marshal_flags(
                self.decoration_manager,
                ZXDG_DECORATION_MANAGER_GET_TOPLEVEL_DECORATION,
                &raw const ZXDG_TOPLEVEL_DECORATION_INTERFACE,
                wl_proxy_get_version(self.decoration_manager),
                0,
                ptr::null_mut::<c_void>(),
                self.toplevel,
            )
        };
        if self.decoration.is_null() {
            return Err(PlatformError::new(
                "zxdg_decoration_manager_v1.get_toplevel_decoration failed",
            ));
        }
        // SAFETY: The listener and boxed state outlive the decoration proxy.
        if unsafe {
            wl_proxy_add_listener(
                self.decoration,
                (&raw const DECORATION_LISTENER).cast(),
                (&raw mut *self.state).cast(),
            )
        } != 0
        {
            return Err(PlatformError::new(
                "zxdg_toplevel_decoration_v1_add_listener failed",
            ));
        }
        // SAFETY: The decoration proxy is live and server-side is a valid mode.
        unsafe {
            wl_proxy_marshal_flags(
                self.decoration,
                ZXDG_TOPLEVEL_DECORATION_SET_MODE,
                ptr::null(),
                wl_proxy_get_version(self.decoration),
                0,
                ZXDG_TOPLEVEL_DECORATION_MODE_SERVER_SIDE,
            );
        }
        println!("Wayland decorations: requested server-side window controls");
        Ok(())
    }

    fn await_initial_configure(&self) -> Result<(), PlatformError> {
        while self.state.pending_serial.get() == 0 && !self.state.closed.get() {
            // SAFETY: The display remains connected and callback state remains live.
            if unsafe { wl_display_roundtrip(self.display) } < 0 {
                return Err(self.display_error("waiting for initial XDG-shell configure"));
            }
        }
        if self.state.closed.get() {
            Err(PlatformError::new(
                "Wayland compositor closed the window before initial configure",
            ))
        } else {
            self.apply_pending_configure();
            Ok(())
        }
    }

    fn apply_pending_configure(&self) {
        let serial = self.state.pending_serial.replace(0);
        if serial == 0 {
            return;
        }
        // SAFETY: The serial came from the newest queued configure event for this live proxy.
        unsafe {
            wl_proxy_marshal_flags(
                self.xdg_surface,
                XDG_SURFACE_ACK_CONFIGURE,
                ptr::null(),
                wl_proxy_get_version(self.xdg_surface),
                0,
                serial,
            );
        }
        self.state.width.set(self.state.pending_width.get());
        self.state.height.set(self.state.pending_height.get());
        crate::follow_confirmed_fullscreen(
            &self.state.fullscreen_confirmed,
            &self.state.fullscreen_requested,
            self.state.pending_fullscreen.get(),
        );
        self.state.configured.set(true);
    }

    pub(super) fn client_extent(&self) -> (u32, u32) {
        (self.state.width.get(), self.state.height.get())
    }

    pub(super) fn pump_events(&self) -> Result<bool, PlatformError> {
        // SAFETY: The display and callback state remain live throughout event dispatch.
        if unsafe { wl_display_dispatch_pending(self.display) } < 0 {
            return Err(self.display_error("wl_display_dispatch_pending"));
        }
        // A small probe emits few protocol requests; a failed flush indicates a disconnected or
        // otherwise unusable compositor rather than a sustained writable-socket backpressure case.
        if unsafe { wl_display_flush(self.display) } < 0 {
            return Err(self.display_error("wl_display_flush"));
        }
        // Socket intake must use the multithread read protocol: the Vulkan driver runs its own
        // reader thread on this display, so a blocking `wl_display_dispatch` can sleep forever on
        // socket data that thread already consumed, freezing presentation on a static window.
        // `wl_display_prepare_read` declines when this queue already holds events; they were
        // dispatched above and the next pump collects the remainder.
        // SAFETY: The display remains connected for the whole read-protocol sequence.
        if unsafe { wl_display_prepare_read(self.display) } == 0 {
            let mut descriptor = PollFd {
                // SAFETY: The display is connected and owns its event socket.
                fd: unsafe { wl_display_get_fd(self.display) },
                events: POLLIN,
                revents: 0,
            };
            // SAFETY: `descriptor` is writable and describes one valid poll entry.
            let ready = unsafe { poll(&raw mut descriptor, 1, 0) };
            if ready < 0 || descriptor.revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
                // SAFETY: Every successful prepare_read requires one read_events or cancel_read.
                unsafe { wl_display_cancel_read(self.display) };
                return Err(if ready < 0 {
                    PlatformError::new("polling the Wayland display failed")
                } else {
                    PlatformError::new(format!(
                        "Wayland display poll failed with revents {:#06x}",
                        descriptor.revents
                    ))
                });
            }
            if descriptor.revents & POLLIN != 0 {
                // SAFETY: prepare_read succeeded on this thread and the display socket is readable.
                if unsafe { wl_display_read_events(self.display) } < 0 {
                    return Err(self.display_error("wl_display_read_events"));
                }
                // SAFETY: The display and callback state remain live throughout event dispatch.
                if unsafe { wl_display_dispatch_pending(self.display) } < 0 {
                    return Err(self.display_error("wl_display_dispatch_pending"));
                }
            } else {
                // SAFETY: Every successful prepare_read requires one read_events or cancel_read.
                unsafe { wl_display_cancel_read(self.display) };
            }
        }
        self.synthesize_key_repeat();
        // A drag can queue many configure events. Only the newest serial and extent matter; drawing
        // every obsolete intermediate size makes the whole compositor-managed window trail input.
        self.apply_pending_configure();
        Ok(!self.state.closed.get())
    }

    /// Emits at most one synthesized repeat per pump for the most recent held key.
    ///
    /// Wayland leaves key repeat to the client. Game pumps run at display rate, above every sane
    /// `repeat_info` rate, so a self-paced single repeat per pump reaches the configured cadence
    /// without a burst catch-up path after a render hitch.
    fn synthesize_key_repeat(&self) {
        let Some((_, key)) = self.state.repeat_key.get() else {
            return;
        };
        let rate = self.state.repeat_rate.get();
        if rate <= 0 || !self.state.keyboard_focused.get() {
            return;
        }
        let Some(due) = self.state.repeat_due.get() else {
            return;
        };
        let now = Instant::now();
        if now < due {
            return;
        }
        self.state.push_input(InputEvent::Keyboard {
            key,
            state: ButtonState::Pressed,
            repeat: true,
            modifiers: self.state.modifiers.get(),
        });
        self.state
            .repeat_due
            .set(Some(now + Duration::from_secs_f64(1.0 / f64::from(rate))));
    }

    pub(super) fn next_input_event(&self) -> Option<InputEvent> {
        self.state.input.borrow_mut().pop_front()
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
            CursorMode::Captured => self.engage_capture(),
            CursorMode::Normal => {
                self.release_capture();
                Ok(())
            }
        }
    }

    pub(super) fn window_mode(&self) -> WindowMode {
        if self.state.fullscreen_requested.get() {
            WindowMode::Fullscreen
        } else {
            WindowMode::Windowed
        }
    }

    /// Requests fullscreen on the compositor-chosen output through `xdg_toplevel`; the compositor
    /// answers with a configure whose state set confirms or ends fullscreen.
    pub(super) fn set_window_mode(&self, mode: WindowMode) -> Result<(), PlatformError> {
        let fullscreen = mode == WindowMode::Fullscreen;
        if self.state.fullscreen_requested.replace(fullscreen) == fullscreen {
            return Ok(());
        }
        let (opcode, operation) = if fullscreen {
            (XDG_TOPLEVEL_SET_FULLSCREEN, "xdg_toplevel.set_fullscreen")
        } else {
            (XDG_TOPLEVEL_UNSET_FULLSCREEN, "xdg_toplevel.unset_fullscreen")
        };
        // SAFETY: The toplevel proxy is live; set_fullscreen's nullable output argument lets the
        // compositor choose the window's current output, and unset_fullscreen takes no arguments.
        unsafe {
            if fullscreen {
                wl_proxy_marshal_flags(
                    self.toplevel,
                    opcode,
                    ptr::null(),
                    wl_proxy_get_version(self.toplevel),
                    0,
                    ptr::null_mut::<c_void>(),
                );
            } else {
                wl_proxy_marshal_flags(
                    self.toplevel,
                    opcode,
                    ptr::null(),
                    wl_proxy_get_version(self.toplevel),
                    0,
                );
            }
            // The request must reach the compositor without waiting for the next pump; a failed
            // flush means the connection itself is unusable.
            if wl_display_flush(self.display) < 0 {
                return Err(self.display_error(operation));
            }
        }
        Ok(())
    }

    /// Locks the pointer to the surface and switches motion delivery to relative deltas.
    ///
    /// The lock uses the persistent lifetime, so the compositor itself suspends it on focus loss
    /// and re-establishes it on focus gain; the requested intent survives without reapplication.
    fn engage_capture(&self) -> Result<(), PlatformError> {
        if self.state.capture_requested.get() {
            return Ok(());
        }
        let pointer = self.state.pointer_proxy.get();
        let missing = if self.seat.is_null() || pointer.is_null() {
            Some("the compositor seat exposes no pointer")
        } else if self.pointer_constraints.is_null() {
            Some("the compositor exposes no zwp_pointer_constraints_v1")
        } else if self.relative_pointer_manager.is_null() {
            Some("the compositor exposes no zwp_relative_pointer_manager_v1")
        } else if self.cursor_shape_manager.is_null() {
            Some("the compositor exposes no wp_cursor_shape_manager_v1 to restore the cursor")
        } else {
            None
        };
        if let Some(reason) = missing {
            return Err(PlatformError::with_kind(
                PlatformErrorKind::Unsupported,
                format!("pointer capture is unavailable: {reason}"),
            ));
        }
        let state_pointer = (&raw const *self.state).cast_mut().cast::<c_void>();
        // SAFETY: The manager, pointer, surface, and boxed state are live; the returned proxies
        // are owned by this window and destroyed on release or drop.
        unsafe {
            let relative = wl_proxy_marshal_flags(
                self.relative_pointer_manager,
                ZWP_RELATIVE_POINTER_MANAGER_GET_RELATIVE_POINTER,
                &raw const ZWP_RELATIVE_POINTER_INTERFACE,
                wl_proxy_get_version(self.relative_pointer_manager),
                0,
                ptr::null_mut::<c_void>(),
                pointer,
            );
            if relative.is_null() {
                return Err(PlatformError::new(
                    "zwp_relative_pointer_manager_v1.get_relative_pointer failed",
                ));
            }
            wl_proxy_add_listener(
                relative,
                (&raw const RELATIVE_POINTER_LISTENER).cast(),
                state_pointer,
            );
            let locked = wl_proxy_marshal_flags(
                self.pointer_constraints,
                ZWP_POINTER_CONSTRAINTS_LOCK_POINTER,
                &raw const ZWP_LOCKED_POINTER_INTERFACE,
                wl_proxy_get_version(self.pointer_constraints),
                0,
                ptr::null_mut::<c_void>(),
                self.surface,
                pointer,
                ptr::null_mut::<c_void>(),
                ZWP_POINTER_CONSTRAINTS_LIFETIME_PERSISTENT,
            );
            if locked.is_null() {
                destroy_protocol_proxy(relative, ZWP_RELATIVE_POINTER_DESTROY);
                return Err(PlatformError::new(
                    "zwp_pointer_constraints_v1.lock_pointer failed",
                ));
            }
            wl_proxy_add_listener(
                locked,
                (&raw const LOCKED_POINTER_LISTENER).cast(),
                state_pointer,
            );
            self.relative_pointer.set(relative);
            self.locked_pointer.set(locked);
        }
        self.state.capture_requested.set(true);
        if self.state.pointer_inside.get() {
            // SAFETY: The pointer proxy is live and the serial is its most recent enter serial.
            unsafe { hide_cursor(pointer, self.state.pointer_enter_serial.get()) };
        }
        // SAFETY: The display is connected; flushing applies the capture without waiting for the
        // next pump.
        unsafe { wl_display_flush(self.display) };
        Ok(())
    }

    /// Destroys the lock and relative-motion proxies and restores a default cursor shape.
    fn release_capture(&self) {
        if !self.state.capture_requested.replace(false) {
            return;
        }
        self.state.capture_active.set(false);
        // SAFETY: Both proxies were created by `engage_capture` and are destroyed exactly once.
        unsafe {
            destroy_protocol_proxy(
                self.locked_pointer.replace(ptr::null_mut()),
                ZWP_LOCKED_POINTER_DESTROY,
            );
            destroy_protocol_proxy(
                self.relative_pointer.replace(ptr::null_mut()),
                ZWP_RELATIVE_POINTER_DESTROY,
            );
        }
        let pointer = self.state.pointer_proxy.get();
        if self.state.pointer_inside.get() && !pointer.is_null() {
            let device = self.ensure_cursor_shape_device(pointer);
            if !device.is_null() {
                // SAFETY: The device proxy is live and the serial is the latest pointer enter.
                unsafe {
                    wl_proxy_marshal_flags(
                        device,
                        WP_CURSOR_SHAPE_DEVICE_SET_SHAPE,
                        ptr::null(),
                        wl_proxy_get_version(device),
                        0,
                        self.state.pointer_enter_serial.get(),
                        WP_CURSOR_SHAPE_DEVICE_SHAPE_DEFAULT,
                    );
                }
            }
        }
        // SAFETY: The display is connected; flushing applies the release promptly.
        unsafe { wl_display_flush(self.display) };
    }

    /// Returns the cursor-shape device for the pointer, creating it on first use.
    fn ensure_cursor_shape_device(&self, pointer: *mut c_void) -> *mut c_void {
        let existing = self.cursor_shape_device.get();
        if !existing.is_null() || self.cursor_shape_manager.is_null() {
            return existing;
        }
        // SAFETY: The manager and pointer proxies are live; the device is owned by this window.
        let device = unsafe {
            wl_proxy_marshal_flags(
                self.cursor_shape_manager,
                WP_CURSOR_SHAPE_MANAGER_GET_POINTER,
                &raw const WP_CURSOR_SHAPE_DEVICE_INTERFACE,
                wl_proxy_get_version(self.cursor_shape_manager),
                0,
                ptr::null_mut::<c_void>(),
                pointer,
            )
        };
        self.cursor_shape_device.set(device);
        device
    }

    pub(super) const fn display(&self) -> *mut c_void {
        self.display.cast()
    }

    pub(super) const fn surface(&self) -> *mut c_void {
        self.surface.cast()
    }

    fn display_error(&self, operation: &str) -> PlatformError {
        // SAFETY: The display remains allocated while errors are reported.
        let code = unsafe { wl_display_get_error(self.display) };
        PlatformError::new(format!("{operation} failed with Wayland error {code}"))
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: Protocol role objects are destroyed child-first before the display disconnects.
        // Input proxies go first: constraint and cursor objects reference the pointer and surface,
        // and the compositor restores its own cursor once the lock and surface are gone.
        unsafe {
            destroy_protocol_proxy(self.locked_pointer.get(), ZWP_LOCKED_POINTER_DESTROY);
            destroy_protocol_proxy(self.relative_pointer.get(), ZWP_RELATIVE_POINTER_DESTROY);
            destroy_protocol_proxy(
                self.cursor_shape_device.get(),
                WP_CURSOR_SHAPE_DEVICE_DESTROY,
            );
            release_input_proxy(self.state.keyboard_proxy.get(), WL_KEYBOARD_RELEASE);
            release_input_proxy(self.state.pointer_proxy.get(), WL_POINTER_RELEASE);
            if !self.seat.is_null() {
                if wl_proxy_get_version(self.seat) >= WL_SEAT_RELEASE_SINCE {
                    destroy_protocol_proxy(self.seat, WL_SEAT_RELEASE);
                } else {
                    wl_proxy_destroy(self.seat);
                }
            }
            destroy_protocol_proxy(
                self.relative_pointer_manager,
                ZWP_RELATIVE_POINTER_MANAGER_DESTROY,
            );
            destroy_protocol_proxy(self.pointer_constraints, ZWP_POINTER_CONSTRAINTS_DESTROY);
            destroy_protocol_proxy(self.cursor_shape_manager, WP_CURSOR_SHAPE_MANAGER_DESTROY);
            destroy_protocol_proxy(self.decoration, ZXDG_TOPLEVEL_DECORATION_DESTROY);
            destroy_protocol_proxy(self.toplevel, XDG_TOPLEVEL_DESTROY);
            destroy_protocol_proxy(self.xdg_surface, XDG_SURFACE_DESTROY);
            destroy_protocol_proxy(self.surface.cast(), WL_SURFACE_DESTROY);
            destroy_protocol_proxy(self.decoration_manager, ZXDG_DECORATION_MANAGER_DESTROY);
            destroy_protocol_proxy(self.wm_base, XDG_WM_BASE_DESTROY);
            if !self.compositor.is_null() {
                wl_proxy_destroy(self.compositor);
            }
            if !self.registry.is_null() {
                wl_proxy_destroy(self.registry);
            }
            if !self.display.is_null() {
                wl_display_disconnect(self.display);
            }
        }
    }
}

unsafe fn bind_global(
    registry: *mut c_void,
    name: u32,
    interface: *const WlInterface,
    version: u32,
) -> *mut c_void {
    // SAFETY: The registry announced the supplied name/interface/version combination.
    unsafe {
        wl_proxy_marshal_flags(
            registry,
            WL_REGISTRY_BIND,
            interface,
            version,
            0,
            name,
            (*interface).name,
            version,
            ptr::null_mut::<c_void>(),
        )
    }
}

unsafe fn destroy_protocol_proxy(proxy: *mut c_void, opcode: u32) {
    if !proxy.is_null() {
        // SAFETY: The proxy is client-owned and the opcode is its protocol destructor.
        unsafe {
            wl_proxy_marshal_flags(
                proxy,
                opcode,
                ptr::null(),
                wl_proxy_get_version(proxy),
                WL_MARSHAL_FLAG_DESTROY,
            );
        }
    }
}

unsafe extern "C" fn registry_global(
    data: *mut c_void,
    _registry: *mut c_void,
    name: u32,
    interface_name: *const c_char,
    version: u32,
) {
    if data.is_null() || interface_name.is_null() {
        return;
    }
    // SAFETY: Wayland supplies the listener data and a NUL-terminated interface name.
    let state = unsafe { &mut *data.cast::<RegistryState>() };
    // SAFETY: The compositor owns this NUL-terminated string for the callback duration.
    match unsafe { CStr::from_ptr(interface_name) }.to_bytes() {
        b"wl_compositor" if state.compositor_name.is_none() => {
            state.compositor_name = Some(name);
            state.compositor_version = version;
        }
        b"xdg_wm_base" if state.wm_base_name.is_none() => {
            state.wm_base_name = Some(name);
            state.wm_base_version = version;
        }
        b"zxdg_decoration_manager_v1" if state.decoration_manager_name.is_none() => {
            state.decoration_manager_name = Some(name);
            state.decoration_manager_version = version;
        }
        b"wl_seat" if state.seat_name.is_none() => {
            state.seat_name = Some(name);
            state.seat_version = version;
        }
        b"zwp_relative_pointer_manager_v1" if state.relative_pointer_manager_name.is_none() => {
            state.relative_pointer_manager_name = Some(name);
        }
        b"zwp_pointer_constraints_v1" if state.pointer_constraints_name.is_none() => {
            state.pointer_constraints_name = Some(name);
        }
        b"wp_cursor_shape_manager_v1" if state.cursor_shape_manager_name.is_none() => {
            state.cursor_shape_manager_name = Some(name);
        }
        _ => {}
    }
}

unsafe extern "C" fn registry_global_remove(
    _data: *mut c_void,
    _registry: *mut c_void,
    _name: u32,
) {
}

unsafe extern "C" fn wm_base_ping(_data: *mut c_void, wm_base: *mut c_void, serial: u32) {
    if wm_base.is_null() {
        return;
    }
    // SAFETY: The callback supplies the live xdg_wm_base proxy and matching ping serial.
    unsafe {
        wl_proxy_marshal_flags(
            wm_base,
            XDG_WM_BASE_PONG,
            ptr::null(),
            wl_proxy_get_version(wm_base),
            0,
            serial,
        );
    }
}

unsafe extern "C" fn xdg_toplevel_configure(
    data: *mut c_void,
    _toplevel: *mut c_void,
    width: i32,
    height: i32,
    states: *mut c_void,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    state.pending_width.set(
        u32::try_from(width)
            .ok()
            .filter(|width| *width != 0)
            .unwrap_or_else(|| state.width.get()),
    );
    state.pending_height.set(
        u32::try_from(height)
            .ok()
            .filter(|height| *height != 0)
            .unwrap_or_else(|| state.height.get()),
    );
    // Each configure carries the complete state set, so an absent fullscreen entry means the
    // compositor considers the toplevel windowed.
    let mut fullscreen = false;
    if let Some(states) = ptr::NonNull::new(states.cast::<WlArray>()) {
        // SAFETY: libwayland passes a live wl_array of u32 states for the callback's duration.
        let states = unsafe { states.as_ref() };
        if !states.data.is_null() {
            // SAFETY: The array's data holds size/4 contiguous u32 protocol states.
            let entries =
                unsafe { std::slice::from_raw_parts(states.data.cast::<u32>(), states.size / 4) };
            fullscreen = entries.contains(&XDG_TOPLEVEL_STATE_FULLSCREEN);
        }
    }
    state.pending_fullscreen.set(fullscreen);
}

unsafe extern "C" fn xdg_toplevel_close(data: *mut c_void, _toplevel: *mut c_void) {
    if !data.is_null() {
        // SAFETY: Listener data points to the boxed WindowState for the proxy lifetime.
        unsafe { &*data.cast::<WindowState>() }.closed.set(true);
    }
}

unsafe extern "C" fn xdg_surface_configure(
    data: *mut c_void,
    _xdg_surface: *mut c_void,
    serial: u32,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    state.pending_serial.set(serial);
}

unsafe extern "C" fn decoration_configure(data: *mut c_void, _decoration: *mut c_void, mode: u32) {
    if !data.is_null() {
        // SAFETY: Listener data points to the boxed WindowState for the proxy lifetime.
        unsafe { &*data.cast::<WindowState>() }
            .decoration_mode
            .set(mode);
    }
}

/// Releases a `wl_pointer` or `wl_keyboard`, informing servers that support the release request.
unsafe fn release_input_proxy(proxy: *mut c_void, release_opcode: u32) {
    if proxy.is_null() {
        return;
    }
    // SAFETY: The proxy is live and client-owned; `release` is its protocol destructor from
    // version three, while older servers only observe the client-side destruction.
    unsafe {
        if wl_proxy_get_version(proxy) >= WL_INPUT_RELEASE_SINCE {
            wl_proxy_marshal_flags(
                proxy,
                release_opcode,
                ptr::null(),
                wl_proxy_get_version(proxy),
                WL_MARSHAL_FLAG_DESTROY,
            );
        } else {
            wl_proxy_destroy(proxy);
        }
    }
}

/// Detaches the cursor image from the pointer for this client's surfaces.
unsafe fn hide_cursor(pointer: *mut c_void, enter_serial: u32) {
    // SAFETY: The pointer proxy is live and the serial came from its most recent enter event.
    unsafe {
        wl_proxy_marshal_flags(
            pointer,
            WL_POINTER_SET_CURSOR,
            ptr::null(),
            wl_proxy_get_version(pointer),
            0,
            enter_serial,
            ptr::null_mut::<c_void>(),
            0_i32,
            0_i32,
        );
    }
}

fn wl_fixed_to_f64(value: i32) -> f64 {
    f64::from(value) / 256.0
}

fn modifiers_from_masks(effective: u32, indices: ModifierIndices) -> Modifiers {
    let active = |index: u32| index < 32 && effective & (1_u32 << index) != 0;
    let mut bits = 0;
    if active(indices.shift) {
        bits |= Modifiers::SHIFT;
    }
    if active(indices.control) {
        bits |= Modifiers::CONTROL;
    }
    if active(indices.alt) {
        bits |= Modifiers::ALT;
    }
    if active(indices.super_key) {
        bits |= Modifiers::SUPER;
    }
    if active(indices.caps_lock) {
        bits |= Modifiers::CAPS_LOCK;
    }
    Modifiers::from_bits(bits)
}

fn pointer_button_identity(code: u32) -> PointerButton {
    match code {
        BTN_LEFT => PointerButton::Primary,
        BTN_RIGHT => PointerButton::Secondary,
        BTN_MIDDLE => PointerButton::Middle,
        BTN_SIDE => PointerButton::Other(3),
        BTN_EXTRA => PointerButton::Other(4),
        other => {
            PointerButton::Other(u16::try_from(other & 0xffff).expect("masked button fits u16"))
        }
    }
}

/// Folds one pointer frame's axis accumulation into a portable scroll delta.
///
/// Finger and continuous sources preserve their precise logical units; wheel sources report
/// coarse steps, falling back to the conventional axis-units-per-detent division when the frame
/// carried no discrete steps.
fn scroll_delta_for_frame(
    source: Option<u32>,
    precise_x: f64,
    precise_y: f64,
    steps_x: f64,
    steps_y: f64,
) -> ScrollDelta {
    match source {
        Some(WL_POINTER_AXIS_SOURCE_FINGER | WL_POINTER_AXIS_SOURCE_CONTINUOUS) => {
            ScrollDelta::Precise {
                x: precise_x,
                y: precise_y,
            }
        }
        _ if steps_x == 0.0 && steps_y == 0.0 => ScrollDelta::Coarse {
            x: precise_x / WL_AXIS_UNITS_PER_STEP,
            y: precise_y / WL_AXIS_UNITS_PER_STEP,
        },
        _ => ScrollDelta::Coarse {
            x: steps_x,
            y: steps_y,
        },
    }
}

fn flush_axis_frame(state: &WindowState) {
    let pending = state.axis_pending.replace(false);
    let precise_x = state.axis_precise_x.replace(0.0);
    let precise_y = state.axis_precise_y.replace(0.0);
    let steps_x = state.axis_steps_x.replace(0.0);
    let steps_y = state.axis_steps_y.replace(0.0);
    let source = state.axis_source.replace(None);
    if !pending && steps_x == 0.0 && steps_y == 0.0 {
        return;
    }
    state.push_input(InputEvent::Scroll {
        delta: scroll_delta_for_frame(source, precise_x, precise_y, steps_x, steps_y),
        position: state.pointer_position.get(),
        modifiers: state.modifiers.get(),
    });
}

unsafe extern "C" fn seat_capabilities(data: *mut c_void, seat: *mut c_void, capabilities: u32) {
    if data.is_null() || seat.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the seat proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    let wants_pointer = capabilities & WL_SEAT_CAPABILITY_POINTER != 0;
    let pointer = state.pointer_proxy.get();
    if wants_pointer && pointer.is_null() {
        // SAFETY: The seat is live; the created pointer proxy shares the seat's listener data,
        // which outlives every seat-derived proxy.
        let proxy = unsafe {
            wl_proxy_marshal_flags(
                seat,
                WL_SEAT_GET_POINTER,
                &raw const wl_pointer_interface,
                wl_proxy_get_version(seat),
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        if !proxy.is_null() {
            // SAFETY: The static listener and the boxed state outlive the pointer proxy.
            unsafe { wl_proxy_add_listener(proxy, (&raw const POINTER_LISTENER).cast(), data) };
            state.pointer_proxy.set(proxy);
        }
    } else if !wants_pointer && !pointer.is_null() {
        // SAFETY: The proxy was created by this module and is released exactly once.
        unsafe { release_input_proxy(pointer, WL_POINTER_RELEASE) };
        state.pointer_proxy.set(ptr::null_mut());
        state.pointer_inside.set(false);
    }
    let wants_keyboard = capabilities & WL_SEAT_CAPABILITY_KEYBOARD != 0;
    let keyboard = state.keyboard_proxy.get();
    if wants_keyboard && keyboard.is_null() {
        // SAFETY: The seat is live; the created keyboard proxy shares the seat's listener data.
        let proxy = unsafe {
            wl_proxy_marshal_flags(
                seat,
                WL_SEAT_GET_KEYBOARD,
                &raw const wl_keyboard_interface,
                wl_proxy_get_version(seat),
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        if !proxy.is_null() {
            // SAFETY: The static listener and the boxed state outlive the keyboard proxy.
            unsafe { wl_proxy_add_listener(proxy, (&raw const KEYBOARD_LISTENER).cast(), data) };
            state.keyboard_proxy.set(proxy);
        }
    } else if !wants_keyboard && !keyboard.is_null() {
        // SAFETY: The proxy was created by this module and is released exactly once.
        unsafe { release_input_proxy(keyboard, WL_KEYBOARD_RELEASE) };
        state.keyboard_proxy.set(ptr::null_mut());
        state.repeat_key.set(None);
        state.repeat_due.set(None);
        if state.keyboard_focused.replace(false) {
            state.push_input(InputEvent::FocusChanged { focused: false });
        }
    }
}

unsafe extern "C" fn seat_name(_data: *mut c_void, _seat: *mut c_void, _name: *const c_char) {}

unsafe extern "C" fn keyboard_keymap(
    data: *mut c_void,
    _keyboard: *mut c_void,
    format: u32,
    fd: c_int,
    size: u32,
) {
    if fd < 0 {
        return;
    }
    if data.is_null() || format != WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1 || size == 0 {
        // SAFETY: The compositor transferred ownership of this descriptor to the client.
        unsafe { close(fd) };
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the keyboard proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    let length = size as usize;
    // SAFETY: The compositor guarantees `size` readable bytes behind the descriptor; the protocol
    // requires a private read-only mapping.
    let mapping = unsafe { mmap(ptr::null_mut(), length, PROT_READ, MAP_PRIVATE, fd, 0) };
    if mapping.addr() != usize::MAX {
        // SAFETY: The mapping is `length` readable bytes for the duration of this callback.
        let bytes = unsafe { std::slice::from_raw_parts(mapping.cast::<u8>(), length) };
        // The xkb keymap string is NUL-terminated by convention; a malformed final byte skips
        // parsing rather than reading past the mapping.
        if bytes[length - 1] == 0 {
            // SAFETY: xkbcommon parses the NUL-terminated mapping without retaining it.
            unsafe {
                let context = xkb_context_new(0);
                if !context.is_null() {
                    let xkb_keymap = xkb_keymap_new_from_string(
                        context,
                        mapping.cast(),
                        XKB_KEYMAP_FORMAT_TEXT_V1,
                        0,
                    );
                    if !xkb_keymap.is_null() {
                        state.modifier_indices.set(ModifierIndices {
                            shift: xkb_keymap_mod_get_index(xkb_keymap, c"Shift".as_ptr()),
                            caps_lock: xkb_keymap_mod_get_index(xkb_keymap, c"Lock".as_ptr()),
                            control: xkb_keymap_mod_get_index(xkb_keymap, c"Control".as_ptr()),
                            alt: xkb_keymap_mod_get_index(xkb_keymap, c"Mod1".as_ptr()),
                            super_key: xkb_keymap_mod_get_index(xkb_keymap, c"Mod4".as_ptr()),
                        });
                        xkb_keymap_unref(xkb_keymap);
                    }
                    xkb_context_unref(context);
                }
            }
        }
        // SAFETY: The mapping was created above with this exact length.
        unsafe { munmap(mapping, length) };
    }
    // SAFETY: The compositor transferred ownership of this descriptor to the client.
    unsafe { close(fd) };
}

unsafe extern "C" fn keyboard_enter(
    data: *mut c_void,
    _keyboard: *mut c_void,
    _serial: u32,
    _surface: *mut c_void,
    _keys: *mut c_void,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the keyboard proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    if !state.keyboard_focused.replace(true) {
        state.push_input(InputEvent::FocusChanged { focused: true });
    }
}

unsafe extern "C" fn keyboard_leave(
    data: *mut c_void,
    _keyboard: *mut c_void,
    _serial: u32,
    _surface: *mut c_void,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the keyboard proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    state.repeat_key.set(None);
    state.repeat_due.set(None);
    state.modifiers.set(Modifiers::default());
    if state.keyboard_focused.replace(false) {
        state.push_input(InputEvent::FocusChanged { focused: false });
    }
}

unsafe extern "C" fn keyboard_key(
    data: *mut c_void,
    _keyboard: *mut c_void,
    _serial: u32,
    _time: u32,
    key: u32,
    key_state: u32,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the keyboard proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    // Modifier keys surface through the aggregate modifiers event that follows them.
    if keymap::is_modifier_key(key) {
        return;
    }
    let code = keymap::evdev_key_code(key);
    let pressed = key_state == WL_KEYBOARD_KEY_STATE_PRESSED;
    state.push_input(InputEvent::Keyboard {
        key: code,
        state: if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        },
        repeat: false,
        modifiers: state.modifiers.get(),
    });
    if pressed {
        let delay = u64::try_from(state.repeat_delay_ms.get().max(0)).unwrap_or(0);
        state.repeat_key.set(Some((key, code)));
        state
            .repeat_due
            .set(Some(Instant::now() + Duration::from_millis(delay)));
    } else if state.repeat_key.get().is_some_and(|(held, _)| held == key) {
        state.repeat_key.set(None);
        state.repeat_due.set(None);
    }
}

unsafe extern "C" fn keyboard_modifiers(
    data: *mut c_void,
    _keyboard: *mut c_void,
    _serial: u32,
    depressed: u32,
    latched: u32,
    locked: u32,
    _group: u32,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the keyboard proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    let modifiers =
        modifiers_from_masks(depressed | latched | locked, state.modifier_indices.get());
    if state.modifiers.replace(modifiers) != modifiers {
        state.push_input(InputEvent::ModifiersChanged(modifiers));
    }
}

unsafe extern "C" fn keyboard_repeat_info(
    data: *mut c_void,
    _keyboard: *mut c_void,
    rate: i32,
    delay: i32,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the keyboard proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    state.repeat_rate.set(rate);
    state.repeat_delay_ms.set(delay);
}

unsafe extern "C" fn pointer_enter(
    data: *mut c_void,
    pointer: *mut c_void,
    serial: u32,
    _surface: *mut c_void,
    surface_x: i32,
    surface_y: i32,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the pointer proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    state.pointer_enter_serial.set(serial);
    state.pointer_inside.set(true);
    state.pointer_position.set(LogicalPosition::new(
        wl_fixed_to_f64(surface_x),
        wl_fixed_to_f64(surface_y),
    ));
    if state.capture_requested.get() && !pointer.is_null() {
        // SAFETY: The pointer proxy delivering this event is live and the serial is current.
        unsafe { hide_cursor(pointer, serial) };
    }
}

unsafe extern "C" fn pointer_leave(
    data: *mut c_void,
    _pointer: *mut c_void,
    _serial: u32,
    _surface: *mut c_void,
) {
    if !data.is_null() {
        // SAFETY: Listener data points to the boxed WindowState for the pointer proxy lifetime.
        unsafe { &*data.cast::<WindowState>() }
            .pointer_inside
            .set(false);
    }
}

unsafe extern "C" fn pointer_motion(
    data: *mut c_void,
    _pointer: *mut c_void,
    _time: u32,
    surface_x: i32,
    surface_y: i32,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the pointer proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    let position = LogicalPosition::new(wl_fixed_to_f64(surface_x), wl_fixed_to_f64(surface_y));
    state.pointer_position.set(position);
    // While the lock is active, motion arrives as relative deltas instead.
    if !state.capture_active.get() {
        state.push_input(InputEvent::PointerMoved {
            position,
            modifiers: state.modifiers.get(),
        });
    }
}

unsafe extern "C" fn pointer_button(
    data: *mut c_void,
    _pointer: *mut c_void,
    _serial: u32,
    _time: u32,
    button: u32,
    button_state: u32,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the pointer proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    state.push_input(InputEvent::PointerButton {
        button: pointer_button_identity(button),
        state: if button_state == 1 {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        },
        position: state.pointer_position.get(),
        modifiers: state.modifiers.get(),
    });
}

unsafe extern "C" fn pointer_axis(
    data: *mut c_void,
    pointer: *mut c_void,
    _time: u32,
    axis: u32,
    value: i32,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the pointer proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    let value = wl_fixed_to_f64(value);
    // Wayland's vertical axis grows downward; the portable contract keeps wheel-forward positive.
    if axis == WL_POINTER_AXIS_VERTICAL {
        state.axis_precise_y.set(state.axis_precise_y.get() - value);
    } else if axis == WL_POINTER_AXIS_HORIZONTAL {
        state.axis_precise_x.set(state.axis_precise_x.get() + value);
    } else {
        return;
    }
    state.axis_pending.set(true);
    // Servers below seat version five never send frame events, so each axis event is its own frame.
    // SAFETY: The pointer proxy delivering this event is live.
    if !pointer.is_null() && unsafe { wl_proxy_get_version(pointer) } < WL_POINTER_FRAME_SINCE {
        flush_axis_frame(state);
    }
}

unsafe extern "C" fn pointer_frame(data: *mut c_void, _pointer: *mut c_void) {
    if !data.is_null() {
        // SAFETY: Listener data points to the boxed WindowState for the pointer proxy lifetime.
        flush_axis_frame(unsafe { &*data.cast::<WindowState>() });
    }
}

unsafe extern "C" fn pointer_axis_source(data: *mut c_void, _pointer: *mut c_void, source: u32) {
    if !data.is_null() {
        // SAFETY: Listener data points to the boxed WindowState for the pointer proxy lifetime.
        unsafe { &*data.cast::<WindowState>() }
            .axis_source
            .set(Some(source));
    }
}

unsafe extern "C" fn pointer_axis_stop(
    _data: *mut c_void,
    _pointer: *mut c_void,
    _time: u32,
    _axis: u32,
) {
}

unsafe extern "C" fn pointer_axis_discrete(
    data: *mut c_void,
    _pointer: *mut c_void,
    axis: u32,
    discrete: i32,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the pointer proxy lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    if axis == WL_POINTER_AXIS_VERTICAL {
        state
            .axis_steps_y
            .set(state.axis_steps_y.get() - f64::from(discrete));
    } else if axis == WL_POINTER_AXIS_HORIZONTAL {
        state
            .axis_steps_x
            .set(state.axis_steps_x.get() + f64::from(discrete));
    }
}

unsafe extern "C" fn relative_pointer_motion(
    data: *mut c_void,
    _relative_pointer: *mut c_void,
    _utime_hi: u32,
    _utime_lo: u32,
    dx: i32,
    dy: i32,
    _dx_unaccel: i32,
    _dy_unaccel: i32,
) {
    if data.is_null() {
        return;
    }
    // SAFETY: Listener data points to the boxed WindowState for the relative-pointer lifetime.
    let state = unsafe { &*data.cast::<WindowState>() };
    // The relative pointer also reports while the lock is inactive; those intervals already
    // deliver absolute motion, so deltas surface only while capture holds.
    if state.capture_active.get() {
        state.push_input(InputEvent::PointerDelta {
            delta_x: wl_fixed_to_f64(dx),
            delta_y: wl_fixed_to_f64(dy),
            modifiers: state.modifiers.get(),
        });
    }
}

unsafe extern "C" fn locked_pointer_locked(data: *mut c_void, _locked_pointer: *mut c_void) {
    if !data.is_null() {
        // SAFETY: Listener data points to the boxed WindowState for the locked-pointer lifetime.
        unsafe { &*data.cast::<WindowState>() }
            .capture_active
            .set(true);
    }
}

unsafe extern "C" fn locked_pointer_unlocked(data: *mut c_void, _locked_pointer: *mut c_void) {
    if !data.is_null() {
        // SAFETY: Listener data points to the boxed WindowState for the locked-pointer lifetime.
        unsafe { &*data.cast::<WindowState>() }
            .capture_active
            .set(false);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BTN_EXTRA, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE, ModifierIndices,
        modifiers_from_masks, pointer_button_identity, scroll_delta_for_frame,
    };
    use crate::{PointerButton, ScrollDelta};

    #[test]
    fn modifier_masks_follow_the_keymap_indices() {
        let indices = ModifierIndices {
            shift: 0,
            caps_lock: 1,
            control: 2,
            alt: 3,
            super_key: 6,
        };
        let modifiers = modifiers_from_masks((1 << 0) | (1 << 2) | (1 << 6), indices);
        assert!(modifiers.shift());
        assert!(modifiers.control());
        assert!(modifiers.super_key());
        assert!(!modifiers.alt());
        assert!(!modifiers.caps_lock());
        let unmapped = modifiers_from_masks(u32::MAX, ModifierIndices::default());
        assert_eq!(unmapped, crate::Modifiers::default());
    }

    #[test]
    fn pointer_buttons_preserve_extended_identity() {
        assert_eq!(pointer_button_identity(BTN_LEFT), PointerButton::Primary);
        assert_eq!(pointer_button_identity(BTN_RIGHT), PointerButton::Secondary);
        assert_eq!(pointer_button_identity(BTN_MIDDLE), PointerButton::Middle);
        assert_eq!(pointer_button_identity(BTN_SIDE), PointerButton::Other(3));
        assert_eq!(pointer_button_identity(BTN_EXTRA), PointerButton::Other(4));
        assert_eq!(pointer_button_identity(0x119), PointerButton::Other(0x119));
    }

    #[test]
    fn scroll_frames_distinguish_precise_and_coarse_sources() {
        assert_eq!(
            scroll_delta_for_frame(Some(1), 2.5, -7.5, 0.0, 0.0),
            ScrollDelta::Precise { x: 2.5, y: -7.5 }
        );
        assert_eq!(
            scroll_delta_for_frame(Some(0), 0.0, -15.0, 0.0, -1.0),
            ScrollDelta::Coarse { x: 0.0, y: -1.0 }
        );
        assert_eq!(
            scroll_delta_for_frame(None, 0.0, 30.0, 0.0, 0.0),
            ScrollDelta::Coarse { x: 0.0, y: 2.0 }
        );
    }
}
