use std::ffi::{CStr, c_char, c_int, c_void};
use std::fmt;
use std::ptr;

use crate::vk;

const WL_DISPLAY_GET_REGISTRY: u32 = 1;
const WL_REGISTRY_BIND: u32 = 0;
const WL_COMPOSITOR_CREATE_SURFACE: u32 = 0;

#[repr(C)]
struct WlInterface {
    name: *const c_char,
    version: c_int,
    method_count: c_int,
    methods: *const c_void,
    event_count: c_int,
    events: *const c_void,
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

unsafe impl Sync for RegistryListener {}

static REGISTRY_LISTENER: RegistryListener = RegistryListener {
    global: Some(registry_global),
    global_remove: Some(registry_global_remove),
};

#[link(name = "wayland-client")]
unsafe extern "C" {
    static wl_registry_interface: WlInterface;
    static wl_compositor_interface: WlInterface;
    static wl_surface_interface: WlInterface;

    fn wl_display_connect(name: *const c_char) -> *mut vk::wl_display;
    fn wl_display_disconnect(display: *mut vk::wl_display);
    fn wl_display_roundtrip(display: *mut vk::wl_display) -> c_int;
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

#[derive(Debug)]
pub(super) struct WaylandError(String);

impl fmt::Display for WaylandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WaylandError {}

#[derive(Default)]
struct RegistryState {
    compositor_name: Option<u32>,
    compositor_version: u32,
}

pub(crate) struct Window {
    display: *mut vk::wl_display,
    registry: *mut c_void,
    compositor: *mut c_void,
    surface: *mut vk::wl_surface,
    registry_state: Box<RegistryState>,
}

impl Window {
    pub(super) fn new(
        _title: &str,
        _width: u32,
        _height: u32,
        visible: bool,
    ) -> Result<Self, WaylandError> {
        if visible {
            return Err(WaylandError(
                "the capability probe creates an unconfigured Wayland surface; visible windows require the future XDG-shell presentation probe"
                    .into(),
            ));
        }
        // SAFETY: A null name asks libwayland-client to use WAYLAND_DISPLAY.
        let display = unsafe { wl_display_connect(ptr::null()) };
        if display.is_null() {
            return Err(WaylandError(
                "wl_display_connect failed; ensure WAYLAND_DISPLAY names a reachable compositor"
                    .into(),
            ));
        }
        let mut window = Self {
            display,
            registry: ptr::null_mut(),
            compositor: ptr::null_mut(),
            surface: ptr::null_mut(),
            registry_state: Box::default(),
        };

        // SAFETY: The display proxy is live and the registry interface is exported by the client.
        window.registry = unsafe {
            wl_proxy_marshal_flags(
                display.cast(),
                WL_DISPLAY_GET_REGISTRY,
                &raw const wl_registry_interface,
                wl_proxy_get_version(display.cast()),
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        if window.registry.is_null() {
            return Err(WaylandError(
                "wl_display_get_registry returned no registry proxy".into(),
            ));
        }
        // SAFETY: The static listener table and boxed callback state outlive the registry proxy.
        let listener_result = unsafe {
            wl_proxy_add_listener(
                window.registry,
                (&raw const REGISTRY_LISTENER).cast(),
                (&raw mut *window.registry_state).cast(),
            )
        };
        if listener_result != 0 {
            return Err(WaylandError(format!(
                "wl_registry_add_listener failed with {listener_result}"
            )));
        }
        // SAFETY: The display remains connected and dispatches the registry callbacks synchronously.
        if unsafe { wl_display_roundtrip(display) } < 0 {
            return Err(WaylandError(
                "Wayland registry roundtrip failed while discovering wl_compositor".into(),
            ));
        }
        let compositor_name = window
            .registry_state
            .compositor_name
            .ok_or_else(|| WaylandError("the Wayland registry exposes no wl_compositor".into()))?;
        let compositor_version = window.registry_state.compositor_version.min(1);
        // SAFETY: The announced global implements wl_compositor and version one has create_surface.
        window.compositor = unsafe {
            wl_proxy_marshal_flags(
                window.registry,
                WL_REGISTRY_BIND,
                &raw const wl_compositor_interface,
                compositor_version,
                0,
                compositor_name,
                wl_compositor_interface.name,
                compositor_version,
                ptr::null_mut::<c_void>(),
            )
        };
        if window.compositor.is_null() {
            return Err(WaylandError(
                "wl_registry_bind returned no wl_compositor proxy".into(),
            ));
        }
        // SAFETY: The compositor proxy is live and supports the version-one create_surface request.
        window.surface = unsafe {
            wl_proxy_marshal_flags(
                window.compositor,
                WL_COMPOSITOR_CREATE_SURFACE,
                &raw const wl_surface_interface,
                wl_proxy_get_version(window.compositor),
                0,
                ptr::null_mut::<c_void>(),
            )
            .cast()
        };
        if window.surface.is_null() {
            return Err(WaylandError(
                "wl_compositor_create_surface returned no surface proxy".into(),
            ));
        }
        Ok(window)
    }

    pub(super) const fn display(&self) -> *mut vk::wl_display {
        self.display
    }

    pub(super) const fn surface(&self) -> *mut vk::wl_surface {
        self.surface
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: Proxies are client-owned and destroyed child-first before disconnecting.
        unsafe {
            if !self.surface.is_null() {
                wl_proxy_destroy(self.surface.cast());
            }
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

pub(super) unsafe fn create_surface(
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
    // SAFETY: The Wayland objects/instance are live, output is writable, and the function matches.
    unsafe { function.expect("loaded function")(instance, &raw const info, ptr::null(), surface) }
}

unsafe extern "C" fn registry_global(
    data: *mut c_void,
    _registry: *mut c_void,
    name: u32,
    interface: *const c_char,
    version: u32,
) {
    if data.is_null() || interface.is_null() {
        return;
    }
    // SAFETY: Wayland supplies the listener data and a NUL-terminated interface name.
    let state = unsafe { &mut *data.cast::<RegistryState>() };
    // SAFETY: The compositor owns this string for the duration of the callback.
    if unsafe { CStr::from_ptr(interface) } == c"wl_compositor" {
        state.compositor_name = Some(name);
        state.compositor_version = version;
    }
}

unsafe extern "C" fn registry_global_remove(
    _data: *mut c_void,
    _registry: *mut c_void,
    _name: u32,
) {
}
