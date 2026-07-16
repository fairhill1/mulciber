use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt;
use std::ptr;

use crate::vk;

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
const ZXDG_DECORATION_MANAGER_DESTROY: u32 = 0;
const ZXDG_DECORATION_MANAGER_GET_TOPLEVEL_DECORATION: u32 = 1;
const ZXDG_TOPLEVEL_DECORATION_DESTROY: u32 = 0;
const ZXDG_TOPLEVEL_DECORATION_SET_MODE: u32 = 1;
const ZXDG_TOPLEVEL_DECORATION_MODE_SERVER_SIDE: u32 = 2;
const WL_MARSHAL_FLAG_DESTROY: u32 = 1;
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

    fn wl_display_connect(name: *const c_char) -> *mut vk::wl_display;
    fn wl_display_disconnect(display: *mut vk::wl_display);
    fn wl_display_roundtrip(display: *mut vk::wl_display) -> c_int;
    fn wl_display_dispatch_pending(display: *mut vk::wl_display) -> c_int;
    fn wl_display_dispatch(display: *mut vk::wl_display) -> c_int;
    fn wl_display_flush(display: *mut vk::wl_display) -> c_int;
    fn wl_display_get_fd(display: *mut vk::wl_display) -> c_int;
    fn wl_display_get_error(display: *mut vk::wl_display) -> c_int;
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
}

#[derive(Debug)]
pub struct WindowError(String);

impl fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WindowError {}

#[derive(Default)]
struct RegistryState {
    compositor_name: Option<u32>,
    compositor_version: u32,
    wm_base_name: Option<u32>,
    wm_base_version: u32,
    decoration_manager_name: Option<u32>,
    decoration_manager_version: u32,
}

struct WindowState {
    width: Cell<u32>,
    height: Cell<u32>,
    pending_width: Cell<u32>,
    pending_height: Cell<u32>,
    pending_serial: Cell<u32>,
    configured: Cell<bool>,
    closed: Cell<bool>,
    decoration_mode: Cell<u32>,
}

pub struct Window {
    display: *mut vk::wl_display,
    registry: *mut c_void,
    compositor: *mut c_void,
    surface: *mut vk::wl_surface,
    wm_base: *mut c_void,
    decoration_manager: *mut c_void,
    xdg_surface: *mut c_void,
    toplevel: *mut c_void,
    decoration: *mut c_void,
    registry_state: Box<RegistryState>,
    state: Box<WindowState>,
}

impl Window {
    pub fn new(title: &str, width: u32, height: u32, _visible: bool) -> Result<Self, WindowError> {
        let title = CString::new(title)
            .map_err(|_| WindowError("Wayland window title contains an interior NUL".into()))?;
        // SAFETY: A null name asks libwayland-client to use WAYLAND_DISPLAY.
        let display = unsafe { wl_display_connect(ptr::null()) };
        if display.is_null() {
            return Err(WindowError(
                "wl_display_connect failed; ensure WAYLAND_DISPLAY names a reachable compositor"
                    .into(),
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
            registry_state: Box::default(),
            state: Box::new(WindowState {
                width: Cell::new(width),
                height: Cell::new(height),
                pending_width: Cell::new(width),
                pending_height: Cell::new(height),
                pending_serial: Cell::new(0),
                configured: Cell::new(false),
                closed: Cell::new(false),
                decoration_mode: Cell::new(0),
            }),
        };
        window.create_registry()?;
        window.bind_globals()?;
        window.create_xdg_toplevel(&title)?;
        window.await_initial_configure()?;
        Ok(window)
    }

    fn create_registry(&mut self) -> Result<(), WindowError> {
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
            return Err(WindowError(
                "wl_display_get_registry returned no proxy".into(),
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
            return Err(WindowError("wl_registry_add_listener failed".into()));
        }
        // SAFETY: The display remains connected and dispatches registry callbacks synchronously.
        if unsafe { wl_display_roundtrip(self.display) } < 0 {
            return Err(self.display_error("Wayland registry roundtrip"));
        }
        Ok(())
    }

    fn bind_globals(&mut self) -> Result<(), WindowError> {
        let compositor_name = self
            .registry_state
            .compositor_name
            .ok_or_else(|| WindowError("Wayland registry exposes no wl_compositor".into()))?;
        let wm_base_name = self
            .registry_state
            .wm_base_name
            .ok_or_else(|| WindowError("Wayland compositor exposes no xdg_wm_base".into()))?;
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
            return Err(WindowError("binding Wayland shell globals failed".into()));
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
                return Err(WindowError(
                    "binding zxdg_decoration_manager_v1 failed".into(),
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
            return Err(WindowError("xdg_wm_base_add_listener failed".into()));
        }
        Ok(())
    }

    fn create_xdg_toplevel(&mut self, title: &CStr) -> Result<(), WindowError> {
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
            return Err(WindowError("wl_compositor_create_surface failed".into()));
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
            return Err(WindowError("xdg_wm_base_get_xdg_surface failed".into()));
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
            return Err(WindowError("xdg_surface_add_listener failed".into()));
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
            return Err(WindowError("xdg_surface_get_toplevel failed".into()));
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
            return Err(WindowError("xdg_toplevel_add_listener failed".into()));
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

    fn create_server_decoration(&mut self) -> Result<(), WindowError> {
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
            return Err(WindowError(
                "zxdg_decoration_manager_v1.get_toplevel_decoration failed".into(),
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
            return Err(WindowError(
                "zxdg_toplevel_decoration_v1_add_listener failed".into(),
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

    fn await_initial_configure(&self) -> Result<(), WindowError> {
        while self.state.pending_serial.get() == 0 && !self.state.closed.get() {
            // SAFETY: The display remains connected and callback state remains live.
            if unsafe { wl_display_roundtrip(self.display) } < 0 {
                return Err(self.display_error("waiting for initial XDG-shell configure"));
            }
        }
        if self.state.closed.get() {
            Err(WindowError(
                "Wayland compositor closed the window before initial configure".into(),
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
        self.state.configured.set(true);
    }

    #[allow(clippy::unnecessary_wraps)]
    pub fn client_extent(&self) -> Result<(u32, u32), WindowError> {
        Ok((self.state.width.get(), self.state.height.get()))
    }

    pub fn pump_events<F>(&self, _live_resize: &mut F) -> Result<bool, WindowError>
    where
        F: FnMut(),
    {
        // SAFETY: The display and callback state remain live throughout event dispatch.
        if unsafe { wl_display_dispatch_pending(self.display) } < 0 {
            return Err(self.display_error("wl_display_dispatch_pending"));
        }
        // A small probe emits few protocol requests; a failed flush indicates a disconnected or
        // otherwise unusable compositor rather than a sustained writable-socket backpressure case.
        if unsafe { wl_display_flush(self.display) } < 0 {
            return Err(self.display_error("wl_display_flush"));
        }
        let mut descriptor = PollFd {
            // SAFETY: The display is connected and owns its event socket.
            fd: unsafe { wl_display_get_fd(self.display) },
            events: POLLIN,
            revents: 0,
        };
        // SAFETY: `descriptor` is writable and describes one valid poll entry.
        let ready = unsafe { poll(&raw mut descriptor, 1, 0) };
        if ready < 0 {
            return Err(WindowError("polling the Wayland display failed".into()));
        }
        if descriptor.revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
            return Err(WindowError(format!(
                "Wayland display poll failed with revents {:#06x}",
                descriptor.revents
            )));
        }
        if descriptor.revents & POLLIN != 0
            // SAFETY: Poll reported readable display events and callback state remains live.
            && unsafe { wl_display_dispatch(self.display) } < 0
        {
            return Err(self.display_error("wl_display_dispatch"));
        }
        // A drag can queue many configure events. Only the newest serial and extent matter; drawing
        // every obsolete intermediate size makes the whole compositor-managed window trail input.
        self.apply_pending_configure();
        Ok(!self.state.closed.get())
    }

    pub(crate) const fn display(&self) -> *mut vk::wl_display {
        self.display
    }

    pub(crate) const fn surface(&self) -> *mut vk::wl_surface {
        self.surface
    }

    fn display_error(&self, operation: &str) -> WindowError {
        // SAFETY: The display remains allocated while errors are reported.
        let code = unsafe { wl_display_get_error(self.display) };
        WindowError(format!("{operation} failed with Wayland error {code}"))
    }
}

pub(crate) unsafe fn create_surface(
    function: vk::PFN_vkCreateWaylandSurfaceKHR,
    instance: vk::VkInstance,
    window: &Window,
    surface: *mut vk::VkSurfaceKHR,
) -> vk::VkResult {
    let info = vk::VkWaylandSurfaceCreateInfoKHR {
        sType: vk::VK_STRUCTURE_TYPE_WAYLAND_SURFACE_CREATE_INFO_KHR,
        display: window.display(),
        surface: window.surface(),
        ..Default::default()
    };
    // SAFETY: Wayland objects/instance are live, output is writable, and the function matches.
    unsafe { function.expect("loaded function")(instance, &raw const info, ptr::null(), surface) }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: Protocol role objects are destroyed child-first before the display disconnects.
        unsafe {
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
    _states: *mut c_void,
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
