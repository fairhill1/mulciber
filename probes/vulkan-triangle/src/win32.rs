use std::cell::Cell;
use std::ffi::{CStr, c_int, c_void};
use std::fmt;
use std::mem;
use std::ptr;
use std::time::Duration;

use crate::vk;

pub(crate) type SurfaceFunction = vk::PFN_vkCreateWin32SurfaceKHR;

pub(crate) const fn surface_extension(_window: &Window) -> &'static CStr {
    c"VK_KHR_win32_surface"
}

pub(crate) const fn surface_description(_window: &Window) -> &'static str {
    "Win32 surface extension"
}

pub(crate) const fn create_surface_name(_window: &Window) -> &'static CStr {
    c"vkCreateWin32SurfaceKHR"
}

pub(crate) const fn acquire_timeout(_window: &Window) -> u64 {
    u64::MAX
}

pub(crate) const fn resize_commit_interval(_window: &Window) -> Duration {
    Duration::ZERO
}

pub type Handle = *mut c_void;
pub type Hinstance = Handle;
pub type Hwnd = Handle;
type Hcursor = Handle;
type Hicon = Handle;
type Hbrush = Handle;
type Hmenu = Handle;
type Lresult = isize;
type Wparam = usize;
type Lparam = isize;
type Atom = u16;

const CS_OWNDC: u32 = 0x0020;
const CW_USEDEFAULT: c_int = i32::MIN;
const IDC_ARROW: *const u16 = 32_512_usize as *const u16;
const PM_REMOVE: u32 = 0x0001;
const SW_SHOW: c_int = 5;
const WM_CLOSE: u32 = 0x0010;
const WM_DESTROY: u32 = 0x0002;
const WM_ENTERSIZEMOVE: u32 = 0x0231;
const WM_EXITSIZEMOVE: u32 = 0x0232;
const WM_NCCREATE: u32 = 0x0081;
const WM_NCDESTROY: u32 = 0x0082;
const WM_QUIT: u32 = 0x0012;
const WM_SIZE: u32 = 0x0005;
const WM_TIMER: u32 = 0x0113;
const GWLP_USERDATA: c_int = -21;
const LIVE_RESIZE_TIMER: usize = 1;
const LIVE_RESIZE_INTERVAL_MS: u32 = 16;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
const WS_MAXIMIZEBOX: u32 = 0x0001_0000;
const WS_MINIMIZEBOX: u32 = 0x0002_0000;
const WS_OVERLAPPED: u32 = 0;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_THICKFRAME: u32 = 0x0004_0000;

const WINDOW_STYLE: u32 = WS_OVERLAPPED
    | WS_CAPTION
    | WS_SYSMENU
    | WS_THICKFRAME
    | WS_MINIMIZEBOX
    | WS_MAXIMIZEBOX
    | WS_CLIPSIBLINGS
    | WS_CLIPCHILDREN;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Msg {
    hwnd: Hwnd,
    message: u32,
    w_param: Wparam,
    l_param: Lparam,
    time: u32,
    point: Point,
    private: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

type WindowProcedure = Option<unsafe extern "system" fn(Hwnd, u32, Wparam, Lparam) -> Lresult>;

#[repr(C)]
struct WindowClassExW {
    size: u32,
    style: u32,
    window_procedure: WindowProcedure,
    class_extra: c_int,
    window_extra: c_int,
    instance: Hinstance,
    icon: Hicon,
    cursor: Hcursor,
    background: Hbrush,
    menu_name: *const u16,
    class_name: *const u16,
    small_icon: Hicon,
}

#[repr(C)]
struct CreateStructW {
    create_parameters: *mut c_void,
    instance: Hinstance,
    menu: Hmenu,
    parent: Hwnd,
    height: c_int,
    width: c_int,
    y: c_int,
    x: c_int,
    style: i32,
    name: *const u16,
    class_name: *const u16,
    extended_style: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
    fn GetModuleHandleW(module_name: *const u16) -> Hinstance;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn AdjustWindowRectEx(rect: *mut Rect, style: u32, menu: i32, extended_style: u32) -> i32;
    fn CreateWindowExW(
        extended_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: c_int,
        y: c_int,
        width: c_int,
        height: c_int,
        parent: Hwnd,
        menu: Hmenu,
        instance: Hinstance,
        parameter: *mut c_void,
    ) -> Hwnd;
    fn DefWindowProcW(window: Hwnd, message: u32, w_param: Wparam, l_param: Lparam) -> Lresult;
    fn DestroyWindow(window: Hwnd) -> i32;
    fn DispatchMessageW(message: *const Msg) -> Lresult;
    fn GetClientRect(window: Hwnd, rect: *mut Rect) -> i32;
    fn GetWindowLongPtrW(window: Hwnd, index: c_int) -> isize;
    fn IsWindow(window: Hwnd) -> i32;
    fn KillTimer(window: Hwnd, event: usize) -> i32;
    fn LoadCursorW(instance: Hinstance, cursor_name: *const u16) -> Hcursor;
    fn PeekMessageW(message: *mut Msg, window: Hwnd, min: u32, max: u32, remove: u32) -> i32;
    fn PostQuitMessage(exit_code: c_int);
    fn RegisterClassExW(class: *const WindowClassExW) -> Atom;
    fn ShowWindow(window: Hwnd, command: c_int) -> i32;
    fn SetTimer(window: Hwnd, event: usize, milliseconds: u32, callback: *const c_void) -> usize;
    fn SetWindowLongPtrW(window: Hwnd, index: c_int, value: isize) -> isize;
    fn TranslateMessage(message: *const Msg) -> i32;
    fn UnregisterClassW(class_name: *const u16, instance: Hinstance) -> i32;
}

#[derive(Debug)]
pub struct WindowError(String);

impl fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WindowError {}

pub struct Window {
    instance: Hinstance,
    handle: Hwnd,
    class_name: Vec<u16>,
    state: Box<WindowState>,
}

type LiveResizeCallback = unsafe fn(*mut c_void);

#[derive(Default)]
struct WindowState {
    live_resize_callback: Cell<Option<LiveResizeCallback>>,
    live_resize_context: Cell<*mut c_void>,
    callback_active: Cell<bool>,
    in_size_move: Cell<bool>,
    timer_error: Cell<u32>,
}

struct CallbackRegistration<'a> {
    state: &'a WindowState,
}

impl Drop for CallbackRegistration<'_> {
    fn drop(&mut self) {
        self.state.live_resize_callback.set(None);
        self.state.live_resize_context.set(ptr::null_mut());
    }
}

pub(crate) fn create_window(
    title: &str,
    width: u32,
    height: u32,
    visible: bool,
    requested_platform: Option<&str>,
) -> Result<Window, WindowError> {
    if requested_platform.is_some_and(|platform| platform != "windows") {
        return Err(WindowError(
            "Windows supports only --platform windows".into(),
        ));
    }
    Window::new(title, width, height, visible)
}

impl Window {
    pub fn new(title: &str, width: u32, height: u32, visible: bool) -> Result<Self, WindowError> {
        let class_name = wide("MulciberVulkanProbe");
        let title = wide(title);
        let state = Box::new(WindowState::default());
        let state_pointer = ptr::from_ref(state.as_ref()).cast_mut().cast::<c_void>();

        // SAFETY: All pointers refer to live, NUL-terminated buffers for the duration of each call.
        unsafe {
            let instance = GetModuleHandleW(ptr::null());
            if instance.is_null() {
                return Err(last_error("GetModuleHandleW"));
            }
            let class = WindowClassExW {
                size: u32::try_from(mem::size_of::<WindowClassExW>())
                    .expect("WNDCLASSEXW size fits u32"),
                style: CS_OWNDC,
                window_procedure: Some(window_procedure),
                class_extra: 0,
                window_extra: 0,
                instance,
                icon: ptr::null_mut(),
                cursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
                background: ptr::null_mut(),
                menu_name: ptr::null(),
                class_name: class_name.as_ptr(),
                small_icon: ptr::null_mut(),
            };
            if RegisterClassExW(&raw const class) == 0 {
                return Err(last_error("RegisterClassExW"));
            }

            let mut rectangle = Rect {
                left: 0,
                top: 0,
                right: i32::try_from(width)
                    .map_err(|_| WindowError("width is too large".into()))?,
                bottom: i32::try_from(height)
                    .map_err(|_| WindowError("height is too large".into()))?,
            };
            if AdjustWindowRectEx(&raw mut rectangle, WINDOW_STYLE, 0, 0) == 0 {
                UnregisterClassW(class_name.as_ptr(), instance);
                return Err(last_error("AdjustWindowRectEx"));
            }

            let handle = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WINDOW_STYLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                rectangle.right - rectangle.left,
                rectangle.bottom - rectangle.top,
                ptr::null_mut(),
                ptr::null_mut(),
                instance,
                state_pointer,
            );
            if handle.is_null() {
                UnregisterClassW(class_name.as_ptr(), instance);
                return Err(last_error("CreateWindowExW"));
            }
            if GetWindowLongPtrW(handle, GWLP_USERDATA) != state_pointer as isize {
                DestroyWindow(handle);
                UnregisterClassW(class_name.as_ptr(), instance);
                return Err(WindowError(
                    "Win32 did not retain Mulciber's window state".into(),
                ));
            }
            if visible {
                ShowWindow(handle, SW_SHOW);
            }

            Ok(Self {
                instance,
                handle,
                class_name,
                state,
            })
        }
    }

    pub fn instance(&self) -> Hinstance {
        self.instance
    }

    pub fn handle(&self) -> Hwnd {
        self.handle
    }

    pub fn client_extent(&self) -> Result<(u32, u32), WindowError> {
        let mut rectangle = Rect::default();
        // SAFETY: The window is live and `rectangle` is writable.
        if unsafe { GetClientRect(self.handle, &raw mut rectangle) } == 0 {
            return Err(last_error("GetClientRect"));
        }
        let width = u32::try_from(rectangle.right - rectangle.left).unwrap_or(0);
        let height = u32::try_from(rectangle.bottom - rectangle.top).unwrap_or(0);
        Ok((width, height))
    }

    pub fn pump_events<F>(&self, live_resize: &mut F) -> Result<bool, WindowError>
    where
        F: FnMut(),
    {
        debug_assert!(!self.handle.is_null());
        debug_assert!(self.state.live_resize_callback.get().is_none());
        self.state.timer_error.set(0);
        self.state
            .live_resize_context
            .set(ptr::from_mut(live_resize).cast());
        self.state
            .live_resize_callback
            .set(Some(invoke_callback::<F>));
        let _registration = CallbackRegistration { state: &self.state };
        let mut message = Msg::default();
        // SAFETY: The message buffer is writable; retrieved messages are initialized by Win32.
        unsafe {
            while PeekMessageW(&raw mut message, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                if message.message == WM_QUIT {
                    return Ok(false);
                }
                TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }
        let timer_error = self.state.timer_error.get();
        if timer_error == 0 {
            Ok(true)
        } else {
            Err(WindowError(format!(
                "live-resize timer failed with Win32 error {timer_error}"
            )))
        }
    }
}

pub(crate) unsafe fn create_surface(
    function: SurfaceFunction,
    instance: vk::VkInstance,
    window: &Window,
    surface: *mut vk::VkSurfaceKHR,
) -> vk::VkResult {
    let info = vk::VkWin32SurfaceCreateInfoKHR {
        sType: vk::VK_STRUCTURE_TYPE_WIN32_SURFACE_CREATE_INFO_KHR,
        hinstance: window.instance(),
        hwnd: window.handle(),
        ..Default::default()
    };
    // SAFETY: The Win32 handles/instance are live, output is writable, and the function matches.
    unsafe { function.expect("loaded function")(instance, &raw const info, ptr::null(), surface) }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: These resources were created by this value and are released once.
        unsafe {
            if !self.handle.is_null() && IsWindow(self.handle) != 0 {
                DestroyWindow(self.handle);
            }
            UnregisterClassW(self.class_name.as_ptr(), self.instance);
        }
    }
}

unsafe extern "system" fn window_procedure(
    window: Hwnd,
    message: u32,
    w_param: Wparam,
    l_param: Lparam,
) -> Lresult {
    match message {
        WM_NCCREATE => {
            // SAFETY: Win32 supplies a live CREATESTRUCTW for WM_NCCREATE. The creation parameter
            // points to the boxed state that remains stable for the lifetime of the window.
            let state = unsafe { (*(l_param as *const CreateStructW)).create_parameters };
            // SAFETY: This stores the application-owned pointer without dereferencing it.
            unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, state as isize) };
            1
        }
        WM_ENTERSIZEMOVE => {
            if let Some(state) = unsafe { state_for_window(window) } {
                state.in_size_move.set(true);
            }
            // SAFETY: Win32 supplied this live window handle. A window-owned timer delivers ticks
            // through this same procedure while DefWindowProc runs its nested sizing loop.
            if unsafe {
                SetTimer(
                    window,
                    LIVE_RESIZE_TIMER,
                    LIVE_RESIZE_INTERVAL_MS,
                    ptr::null(),
                )
            } == 0
                && let Some(state) = unsafe { state_for_window(window) }
            {
                state.timer_error.set(unsafe { GetLastError() });
            }
            0
        }
        WM_EXITSIZEMOVE => {
            if let Some(state) = unsafe { state_for_window(window) } {
                state.in_size_move.set(false);
            }
            // SAFETY: The timer belongs to this live window and fixed identifier.
            if unsafe { KillTimer(window, LIVE_RESIZE_TIMER) } == 0
                && let Some(state) = unsafe { state_for_window(window) }
            {
                state.timer_error.set(unsafe { GetLastError() });
            }
            0
        }
        WM_SIZE => {
            // SAFETY: Size notifications are synchronous on this thread, and callback registration
            // remains live throughout the nested Win32 sizing loop.
            if let Some(state) = unsafe { state_for_window(window) }
                && state.in_size_move.get()
            {
                unsafe { invoke_live_resize(state) };
            }
            0
        }
        WM_TIMER if w_param == LIVE_RESIZE_TIMER => {
            // SAFETY: Callback registration is scoped around DispatchMessageW. Timer messages run
            // synchronously on this thread, and the reentrancy flag prevents nested mutable calls.
            if let Some(state) = unsafe { state_for_window(window) } {
                unsafe { invoke_live_resize(state) };
            }
            0
        }
        WM_CLOSE => {
            // SAFETY: Win32 supplied this live window handle.
            unsafe { DestroyWindow(window) };
            0
        }
        WM_DESTROY => {
            // SAFETY: Posting the thread's quit message has no pointer preconditions.
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_NCDESTROY => {
            // SAFETY: Stop any outstanding sizing timer before clearing the borrowed state pointer.
            unsafe {
                KillTimer(window, LIVE_RESIZE_TIMER);
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                DefWindowProcW(window, message, w_param, l_param)
            }
        }
        // SAFETY: Unknown messages are delegated with their original Win32 values.
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}

unsafe fn state_for_window(window: Hwnd) -> Option<&'static WindowState> {
    // SAFETY: The value was installed from a boxed WindowState during WM_NCCREATE and is cleared
    // during WM_NCDESTROY before that box can be dropped.
    let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const WindowState;
    unsafe { pointer.as_ref() }
}

unsafe fn invoke_live_resize(state: &WindowState) {
    if let Some(callback) = state.live_resize_callback.get()
        && !state.callback_active.replace(true)
    {
        // SAFETY: The registration owns a live exclusive callback borrow on this thread.
        unsafe { callback(state.live_resize_context.get()) };
        state.callback_active.set(false);
    }
}

unsafe fn invoke_callback<F>(context: *mut c_void)
where
    F: FnMut(),
{
    // SAFETY: `pump_events` installed this pointer from a live exclusive borrow of F and clears the
    // callback before that borrow expires. Window callbacks execute synchronously on this thread.
    unsafe { (&mut *context.cast::<F>())() };
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn last_error(operation: &str) -> WindowError {
    // SAFETY: GetLastError has no preconditions and returns thread-local state.
    WindowError(format!("{operation} failed with Win32 error {}", unsafe {
        GetLastError()
    }))
}
