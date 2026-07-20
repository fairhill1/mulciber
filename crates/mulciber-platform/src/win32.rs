//! Native Win32 window ownership, thread-message dispatch, and nested resize lifecycle.

use std::cell::{Cell, RefCell};
use std::ffi::{c_int, c_void};
use std::marker::PhantomData;
use std::mem;
use std::ptr::{self, NonNull};
use std::rc::Rc;

use crate::{
    ButtonState, CursorMode, InputEvent, KeyCode, LogicalPosition, Modifiers, PhysicalExtent,
    PlatformError, PointerButton, PumpStatus, ScrollDelta, WindowDescriptor, WindowEvent,
    WindowMetrics, WindowRevision,
};

type Handle = *mut c_void;
type Hinstance = Handle;
type Hwnd = Handle;
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
const HID_USAGE_GENERIC_MOUSE: u16 = 0x02;
const HID_USAGE_PAGE_GENERIC: u16 = 0x01;
const HTCLIENT: u16 = 0x0001;
const IDC_ARROW: *const u16 = 32_512_usize as *const u16;
const MOUSE_MOVE_ABSOLUTE: u16 = 0x0001;
const PM_REMOVE: u32 = 0x0001;
const RID_INPUT: u32 = 0x1000_0003;
const RIDEV_REMOVE: u32 = 0x0000_0001;
const RIM_TYPEMOUSE: u32 = 0;
const SW_SHOW: c_int = 5;
const VK_CAPITAL: usize = 0x14;
const VK_CONTROL: usize = 0x11;
const VK_F4: usize = 0x73;
const VK_LWIN: usize = 0x5B;
const VK_MENU: usize = 0x12;
const VK_RWIN: usize = 0x5C;
const VK_SHIFT: usize = 0x10;
const WM_CAPTURECHANGED: u32 = 0x0215;
const WM_CHAR: u32 = 0x0102;
const WM_CLOSE: u32 = 0x0010;
const WM_DESTROY: u32 = 0x0002;
const WM_ENTERSIZEMOVE: u32 = 0x0231;
const WM_EXITSIZEMOVE: u32 = 0x0232;
const WM_INPUT: u32 = 0x00FF;
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_KILLFOCUS: u32 = 0x0008;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_MBUTTONDOWN: u32 = 0x0207;
const WM_MBUTTONUP: u32 = 0x0208;
const WM_MOUSEHWHEEL: u32 = 0x020E;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_MOUSEWHEEL: u32 = 0x020A;
const WM_MOVE: u32 = 0x0003;
const WM_NCCREATE: u32 = 0x0081;
const WM_NCDESTROY: u32 = 0x0082;
const WM_QUIT: u32 = 0x0012;
const WM_RBUTTONDOWN: u32 = 0x0204;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_SETCURSOR: u32 = 0x0020;
const WM_SETFOCUS: u32 = 0x0007;
const WM_SIZE: u32 = 0x0005;
const WM_SYSCHAR: u32 = 0x0106;
const WM_SYSKEYDOWN: u32 = 0x0104;
const WM_SYSKEYUP: u32 = 0x0105;
const WM_TIMER: u32 = 0x0113;
const WM_XBUTTONDOWN: u32 = 0x020B;
const WM_XBUTTONUP: u32 = 0x020C;
const WHEEL_DELTA: f64 = 120.0;
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

#[repr(C)]
#[derive(Clone, Copy)]
struct RawInputDevice {
    usage_page: u16,
    usage: u16,
    flags: u32,
    target: Hwnd,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawInputHeader {
    kind: u32,
    size: u32,
    device: Handle,
    w_param: Wparam,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawMouse {
    flags: u16,
    reserved: u16,
    button_flags: u16,
    button_data: u16,
    raw_buttons: u32,
    last_x: i32,
    last_y: i32,
    extra_information: u32,
}

/// The mouse-shaped prefix of Win32's variable-length `RAWINPUT` payload.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawInputMouse {
    header: RawInputHeader,
    mouse: RawMouse,
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
    fn ClientToScreen(window: Hwnd, point: *mut Point) -> i32;
    fn ClipCursor(rect: *const Rect) -> i32;
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
    fn GetCapture() -> Hwnd;
    fn GetClientRect(window: Hwnd, rect: *mut Rect) -> i32;
    fn GetKeyState(virtual_key: c_int) -> i16;
    fn GetRawInputData(
        input: Handle,
        command: u32,
        data: *mut c_void,
        size: *mut u32,
        header_size: u32,
    ) -> u32;
    fn GetWindowLongPtrW(window: Hwnd, index: c_int) -> isize;
    fn IsWindow(window: Hwnd) -> i32;
    fn KillTimer(window: Hwnd, event: usize) -> i32;
    fn LoadCursorW(instance: Hinstance, cursor_name: *const u16) -> Hcursor;
    fn PeekMessageW(message: *mut Msg, window: Hwnd, min: u32, max: u32, remove: u32) -> i32;
    fn RegisterClassExW(class: *const WindowClassExW) -> Atom;
    fn RegisterRawInputDevices(devices: *const RawInputDevice, count: u32, size: u32) -> i32;
    fn ReleaseCapture() -> i32;
    fn ScreenToClient(window: Hwnd, point: *mut Point) -> i32;
    fn SetCapture(window: Hwnd) -> Hwnd;
    fn SetCursor(cursor: Hcursor) -> Hcursor;
    fn SetCursorPos(x: c_int, y: c_int) -> i32;
    fn ShowWindow(window: Hwnd, command: c_int) -> i32;
    fn SetTimer(window: Hwnd, event: usize, milliseconds: u32, callback: *const c_void) -> usize;
    fn SetWindowLongPtrW(window: Hwnd, index: c_int, value: isize) -> isize;
    fn TranslateMessage(message: *const Msg) -> i32;
    fn UnregisterClassW(class_name: *const u16, instance: Hinstance) -> i32;
}

/// A Win32 application event pump confined to its creating thread.
pub struct Application {
    window_slot: WindowSlot,
    _creating_thread: PhantomData<Rc<()>>,
}

impl Application {
    /// Creates a Win32 application event pump on the current thread.
    ///
    /// # Errors
    ///
    /// This constructor currently cannot fail. It returns a result so peer platforms retain the
    /// same application construction contract while Win32 connection policy remains evidence-led.
    pub fn new() -> Result<Self, PlatformError> {
        Ok(Self {
            window_slot: WindowSlot::new(),
            _creating_thread: PhantomData,
        })
    }

    /// Creates and shows one native Win32 window.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or out-of-range extent, while another window is alive, or
    /// when Win32 cannot register the class or create the native window.
    pub fn create_window(&self, descriptor: &WindowDescriptor) -> Result<Window, PlatformError> {
        self.create_window_with_visibility(descriptor, true)
    }

    fn create_window_with_visibility(
        &self,
        descriptor: &WindowDescriptor,
        visible: bool,
    ) -> Result<Window, PlatformError> {
        if descriptor.logical_size().is_empty() {
            return Err(PlatformError::invalid_request(
                "window creation requires a non-empty logical extent",
            ));
        }
        let window_lease = self.window_slot.claim()?;
        Window::new(descriptor, visible, window_lease)
    }

    /// Dispatches queued Win32 messages and reports lifecycle events for `window`.
    ///
    /// `RedrawRequested` can be delivered from inside Win32's nested interactive-resize loop before
    /// this method returns. The handler must therefore leave the window and application alive for
    /// the complete call. The first handler error stops delivery of this call's remaining events —
    /// including nested-sizing-loop redraws — and is returned once native dispatch completes;
    /// platform state still advances so a later pump does not replay the dropped events.
    ///
    /// # Errors
    ///
    /// Returns a converted platform error when native extent queries or the live-resize timer
    /// fail, otherwise the first error returned by `handler`.
    pub fn pump_events<E>(
        &mut self,
        window: &Window,
        mut handler: impl FnMut(WindowEvent) -> Result<(), E>,
    ) -> Result<PumpStatus, E>
    where
        E: From<PlatformError>,
    {
        let mut handler_error = None;
        let status = pump_native_events(window, |event| {
            if handler_error.is_some() {
                return;
            }
            if let Err(error) = handler(event) {
                handler_error = Some(error);
            }
        })?;
        match handler_error {
            Some(error) => Err(error),
            None => Ok(status),
        }
    }
}

fn pump_native_events<F>(window: &Window, mut handler: F) -> Result<PumpStatus, PlatformError>
where
    F: FnMut(WindowEvent),
{
    debug_assert!(window.state.event_callback.get().is_none());
    window.state.timer_error.set(0);
    window.state.callback_error.borrow_mut().take();
    window
        .state
        .event_context
        .set(ptr::from_mut(&mut handler).cast());
    window.state.event_callback.set(Some(invoke_callback::<F>));
    let _registration = CallbackRegistration {
        state: &window.state,
    };

    let mut quit_requested = false;
    let mut message = Msg::default();
    // SAFETY: The message buffer is writable; retrieved messages are initialized by Win32 and
    // dispatched synchronously while callback registration remains live.
    unsafe {
        while PeekMessageW(&raw mut message, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            if message.message == WM_QUIT {
                quit_requested = true;
                break;
            }
            TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }

    if let Some(error) = window.state.callback_error.borrow_mut().take() {
        return Err(error);
    }
    let timer_error = window.state.timer_error.get();
    if timer_error != 0 {
        return Err(PlatformError::new(format!(
            "live-resize timer failed with Win32 error {timer_error}"
        )));
    }
    // Focus can change while no callback is registered (including during ShowWindow). Reconcile
    // the retained state once per pump so the public stream still observes the transition.
    // SAFETY: The window and callback registration are live on this thread.
    unsafe { dispatch_focus_transition(window.handle.as_ptr(), &window.state) };
    if quit_requested || window.state.close_requested.get() || !window.is_open() {
        if !window.state.close_reported.replace(true) {
            // SAFETY: Registration above owns a live exclusive handler borrow on this thread.
            unsafe { invoke_event_callback(&window.state, WindowEvent::CloseRequested) };
        }
        window.state.last_metrics.set(None);
        return Ok(PumpStatus::Exit);
    }

    // Normal frame opportunity after queued messages. Nested live-resize opportunities were
    // already delivered synchronously by the window procedure.
    // SAFETY: The window and callback registration are live for this synchronous dispatch.
    unsafe { dispatch_window_events(window.handle.as_ptr(), &window.state) };
    if let Some(error) = window.state.callback_error.borrow_mut().take() {
        Err(error)
    } else {
        Ok(PumpStatus::Continue)
    }
}

/// An owned Win32 window confined to its creating thread.
pub struct Window {
    instance: NonNull<c_void>,
    handle: NonNull<c_void>,
    class_name: Vec<u16>,
    state: Box<WindowState>,
    _window_lease: WindowLease,
    _creating_thread: PhantomData<Rc<()>>,
}

type EventCallback = unsafe fn(*mut c_void, WindowEvent);

struct WindowState {
    event_callback: Cell<Option<EventCallback>>,
    event_context: Cell<*mut c_void>,
    callback_active: Cell<bool>,
    callback_error: RefCell<Option<PlatformError>>,
    in_size_move: Cell<bool>,
    timer_error: Cell<u32>,
    revision: Cell<WindowRevision>,
    last_extent: Cell<PhysicalExtent>,
    last_metrics: Cell<Option<WindowMetrics>>,
    focused: Cell<bool>,
    last_focused: Cell<bool>,
    last_modifiers: Cell<Modifiers>,
    captured_pointer_buttons: Cell<u16>,
    last_pointer_position: Cell<LogicalPosition>,
    capture_requested: Cell<bool>,
    capture_engaged: Cell<bool>,
    last_raw_absolute: Cell<Option<(i32, i32)>>,
    close_requested: Cell<bool>,
    close_reported: Cell<bool>,
}

impl WindowState {
    fn new() -> Self {
        Self {
            event_callback: Cell::new(None),
            event_context: Cell::new(ptr::null_mut()),
            callback_active: Cell::new(false),
            callback_error: RefCell::new(None),
            in_size_move: Cell::new(false),
            timer_error: Cell::new(0),
            revision: Cell::new(WindowRevision::INITIAL),
            last_extent: Cell::new(PhysicalExtent::default()),
            last_metrics: Cell::new(None),
            focused: Cell::new(false),
            last_focused: Cell::new(false),
            last_modifiers: Cell::new(Modifiers::default()),
            captured_pointer_buttons: Cell::new(0),
            last_pointer_position: Cell::new(LogicalPosition::default()),
            capture_requested: Cell::new(false),
            capture_engaged: Cell::new(false),
            last_raw_absolute: Cell::new(None),
            close_requested: Cell::new(false),
            close_reported: Cell::new(false),
        }
    }
}

struct CallbackRegistration<'a> {
    state: &'a WindowState,
}

impl Drop for CallbackRegistration<'_> {
    fn drop(&mut self) {
        self.state.event_callback.set(None);
        self.state.event_context.set(ptr::null_mut());
    }
}

impl Window {
    fn new(
        descriptor: &WindowDescriptor,
        visible: bool,
        window_lease: WindowLease,
    ) -> Result<Self, PlatformError> {
        if descriptor.title().contains('\0') {
            return Err(PlatformError::invalid_request(
                "Win32 window title contains an interior NUL",
            ));
        }
        let size = descriptor.logical_size();
        let width = i32::try_from(size.width())
            .map_err(|_| PlatformError::invalid_request("window width is too large for Win32"))?;
        let height = i32::try_from(size.height())
            .map_err(|_| PlatformError::invalid_request("window height is too large for Win32"))?;
        let title = wide(descriptor.title());
        let state = Box::new(WindowState::new());
        let class_name = wide(&format!("MulciberPlatformWindow-{:p}", state.as_ref()));
        let state_pointer = ptr::from_ref(state.as_ref()).cast_mut().cast::<c_void>();

        // SAFETY: All pointers refer to live, NUL-terminated buffers for the duration of each call.
        unsafe {
            let raw_instance = GetModuleHandleW(ptr::null());
            let Some(instance) = NonNull::new(raw_instance) else {
                return Err(last_error("GetModuleHandleW"));
            };
            let class = WindowClassExW {
                size: u32::try_from(mem::size_of::<WindowClassExW>())
                    .expect("WNDCLASSEXW size fits u32"),
                style: CS_OWNDC,
                window_procedure: Some(window_procedure),
                class_extra: 0,
                window_extra: 0,
                instance: instance.as_ptr(),
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
                right: width,
                bottom: height,
            };
            if AdjustWindowRectEx(&raw mut rectangle, WINDOW_STYLE, 0, 0) == 0 {
                UnregisterClassW(class_name.as_ptr(), instance.as_ptr());
                return Err(last_error("AdjustWindowRectEx"));
            }

            let raw_handle = CreateWindowExW(
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
                instance.as_ptr(),
                state_pointer,
            );
            let Some(handle) = NonNull::new(raw_handle) else {
                UnregisterClassW(class_name.as_ptr(), instance.as_ptr());
                return Err(last_error("CreateWindowExW"));
            };
            if GetWindowLongPtrW(handle.as_ptr(), GWLP_USERDATA) != state_pointer as isize {
                DestroyWindow(handle.as_ptr());
                UnregisterClassW(class_name.as_ptr(), instance.as_ptr());
                return Err(PlatformError::with_kind(
                    crate::PlatformErrorKind::Internal,
                    "Win32 did not retain Mulciber's window state",
                ));
            }
            if visible {
                ShowWindow(handle.as_ptr(), SW_SHOW);
            }

            let window = Self {
                instance,
                handle,
                class_name,
                state,
                _window_lease: window_lease,
                _creating_thread: PhantomData,
            };
            let initial_metrics = window.current_window_metrics()?;
            window.state.last_metrics.set(initial_metrics);
            Ok(window)
        }
    }

    /// Returns current drawable metrics, or `None` while the client extent is empty or closed.
    #[must_use]
    pub fn rendering_metrics(&self) -> Option<WindowMetrics> {
        self.current_window_metrics().ok().flatten()
    }

    /// Returns a borrowed opaque target accepted by Mulciber's graphics surface creation.
    #[must_use]
    pub fn surface_target(&self) -> SurfaceTarget<'_> {
        SurfaceTarget {
            instance: self.instance,
            window: self.handle,
            _window: PhantomData,
        }
    }

    /// Requests how this window interacts with the system pointer.
    ///
    /// [`CursorMode::Captured`] hides the cursor through the window's `WM_SETCURSOR` handling,
    /// confines it to the client area with `ClipCursor`, keeps it recentered, and reports
    /// raw-input motion as [`InputEvent::PointerDelta`] instead of absolute positions. Capture
    /// engages while the window is focused, releases on focus loss, and is best-effort reapplied
    /// on focus gain; requesting it on an unfocused window stores the intent for the next gain.
    ///
    /// # Errors
    ///
    /// Returns a converted platform error when raw-input registration or cursor confinement
    /// fails while engaging capture on a focused window.
    pub fn set_cursor_mode(&self, mode: CursorMode) -> Result<(), PlatformError> {
        match mode {
            CursorMode::Captured => {
                if self.state.capture_requested.get() {
                    return Ok(());
                }
                if self.state.focused.get() {
                    // SAFETY: The window handle is live on its creating thread.
                    unsafe { engage_pointer_capture(self.handle.as_ptr(), &self.state)? };
                }
                self.state.capture_requested.set(true);
                Ok(())
            }
            CursorMode::Normal => {
                self.state.capture_requested.set(false);
                // SAFETY: Clip and raw-input registration belong to this thread's live window.
                unsafe { release_pointer_capture(&self.state) };
                Ok(())
            }
        }
    }

    /// Returns the requested cursor mode, independent of whether focus currently suspends it.
    #[must_use]
    pub fn cursor_mode(&self) -> CursorMode {
        if self.state.capture_requested.get() {
            CursorMode::Captured
        } else {
            CursorMode::Normal
        }
    }

    fn is_open(&self) -> bool {
        // SAFETY: The handle value remains initialized after native destruction; Win32 reports
        // whether it still identifies a live window.
        unsafe { IsWindow(self.handle.as_ptr()) != 0 }
    }

    fn current_window_metrics(&self) -> Result<Option<WindowMetrics>, PlatformError> {
        current_window_metrics(self.handle.as_ptr(), &self.state)
    }
}

/// A borrowed native target whose ownership remains with its [`Window`].
pub struct SurfaceTarget<'window> {
    instance: NonNull<c_void>,
    window: NonNull<c_void>,
    _window: PhantomData<&'window Window>,
}

struct WindowSlot {
    live: Rc<Cell<bool>>,
}

impl WindowSlot {
    fn new() -> Self {
        Self {
            live: Rc::new(Cell::new(false)),
        }
    }

    fn claim(&self) -> Result<WindowLease, PlatformError> {
        if self.live.get() {
            return Err(PlatformError::lifecycle(
                "the initial Win32 extraction supports one live window per application",
            ));
        }
        self.live.set(true);
        Ok(WindowLease {
            live: Rc::clone(&self.live),
        })
    }
}

struct WindowLease {
    live: Rc<Cell<bool>>,
}

impl Drop for WindowLease {
    fn drop(&mut self) {
        self.live.set(false);
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: These resources were created by this value and are released once. The cursor
        // clip and raw-input registration are process-global, so they are released even when an
        // external client already destroyed the native window.
        unsafe {
            release_pointer_capture(&self.state);
            if IsWindow(self.handle.as_ptr()) != 0 {
                DestroyWindow(self.handle.as_ptr());
            }
            UnregisterClassW(self.class_name.as_ptr(), self.instance.as_ptr());
        }
    }
}

unsafe extern "system" fn window_procedure(
    window: Hwnd,
    message: u32,
    w_param: Wparam,
    l_param: Lparam,
) -> Lresult {
    // SAFETY: Win32 calls this procedure synchronously on the owning window thread.
    if let Some(result) = unsafe { handle_input_message(window, message, w_param, l_param) } {
        return result;
    }
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
            if let Some(state) = unsafe { state_for_window(window) } {
                // SAFETY: The clip rectangle tracks this live window's moved client bounds.
                unsafe { reclip_captured_cursor(window, state) };
                if state.in_size_move.get() {
                    unsafe { dispatch_window_events(window, state) };
                }
            }
            0
        }
        WM_MOVE => {
            if let Some(state) = unsafe { state_for_window(window) } {
                // SAFETY: The clip rectangle tracks this live window's moved client bounds.
                unsafe { reclip_captured_cursor(window, state) };
            }
            0
        }
        WM_TIMER if w_param == LIVE_RESIZE_TIMER => {
            // SAFETY: Callback registration is scoped around DispatchMessageW. Timer messages run
            // synchronously on this thread, and the reentrancy flag prevents nested mutable calls.
            if let Some(state) = unsafe { state_for_window(window) } {
                unsafe { dispatch_window_events(window, state) };
            }
            0
        }
        WM_CLOSE => {
            // Keep the HWND alive until `Window` drops. Graphics surfaces borrow it, so reporting
            // the close first lets the application retire presentation work and destroy the
            // surface before native window destruction.
            if let Some(state) = unsafe { state_for_window(window) } {
                state.close_requested.set(true);
            }
            0
        }
        WM_DESTROY => {
            // Application::pump_events observes the destroyed handle directly. Posting WM_QUIT
            // here would poison the thread queue for a later window created by the same application.
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

unsafe fn handle_input_message(
    window: Hwnd,
    message: u32,
    w_param: Wparam,
    l_param: Lparam,
) -> Option<Lresult> {
    let state = unsafe { state_for_window(window) };
    match message {
        WM_SETFOCUS | WM_KILLFOCUS => {
            if let Some(state) = state {
                state.focused.set(message == WM_SETFOCUS);
                // SAFETY: Focus messages are synchronous. If creation changes focus before a
                // callback exists, the next pump reconciles the retained value.
                unsafe { dispatch_focus_transition(window, state) };
            }
            Some(0)
        }
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
            if let Some(state) = state {
                // SAFETY: Keyboard messages are delivered on the window thread.
                unsafe { dispatch_keyboard_event(state, message, w_param, l_param) };
            }
            if message == WM_SYSKEYDOWN
                && w_param == VK_F4
                && l_param.cast_unsigned() & (1 << 29) != 0
            {
                // SAFETY: Preserve native Alt+F4 after reporting the physical key press.
                Some(unsafe { DefWindowProcW(window, message, w_param, l_param) })
            } else {
                Some(0)
            }
        }
        // TranslateMessage derives these from physical key messages. Text and IME are a separate
        // future contract; consuming them prevents the render-only window's default OS beep.
        WM_CHAR | WM_SYSCHAR => Some(0),
        WM_MOUSEMOVE => {
            if let Some(state) = state {
                if state.capture_engaged.get() {
                    // Deltas come from WM_INPUT; absolute motion only recenters the pinned cursor.
                    // SAFETY: The warp targets this live window's client center.
                    unsafe { recenter_captured_cursor(window, l_param) };
                } else {
                    let position = pointer_position(l_param);
                    state.last_pointer_position.set(position);
                    // SAFETY: Pointer messages are synchronous while pump registration is live.
                    unsafe {
                        dispatch_input_event(
                            state,
                            InputEvent::PointerMoved {
                                position,
                                modifiers: win32_modifiers(),
                            },
                        );
                    }
                }
            }
            Some(0)
        }
        WM_INPUT => {
            if let Some(state) = state
                && state.capture_engaged.get()
            {
                // SAFETY: The raw-input handle in l_param is valid for this synchronous message.
                unsafe { dispatch_raw_pointer_delta(state, l_param) };
            }
            // SAFETY: Raw-input messages require default processing for system cleanup.
            Some(unsafe { DefWindowProcW(window, message, w_param, l_param) })
        }
        WM_SETCURSOR => {
            if let Some(state) = state
                && state.capture_engaged.get()
                && low_word(l_param.cast_unsigned()) == HTCLIENT
            {
                // SAFETY: A null cursor hides the pointer over the captured client area.
                unsafe { SetCursor(ptr::null_mut()) };
                return Some(1);
            }
            None
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
        | WM_MBUTTONUP | WM_XBUTTONDOWN | WM_XBUTTONUP => {
            if let Some(state) = state
                && let Some((button, mask, button_state)) = pointer_button(message, w_param)
            {
                // SAFETY: Capture remains confined to this live window's thread.
                unsafe {
                    dispatch_pointer_button(window, state, button, mask, button_state, l_param);
                }
            }
            Some(Lresult::from(matches!(
                message,
                WM_XBUTTONDOWN | WM_XBUTTONUP
            )))
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            if let Some(state) = state {
                // SAFETY: Screen coordinates are converted against this live window.
                unsafe { dispatch_scroll_event(window, state, message, w_param, l_param) };
            }
            Some(0)
        }
        WM_CAPTURECHANGED => {
            if let Some(state) = state {
                // SAFETY: Win32 already transferred capture; reconcile retained button state.
                unsafe { release_captured_buttons(state) };
            }
            Some(0)
        }
        _ => None,
    }
}

unsafe fn dispatch_focus_transition(window: Hwnd, state: &WindowState) {
    if state.event_callback.get().is_none() || state.last_focused.get() == state.focused.get() {
        return;
    }
    let focused = state.focused.get();
    state.last_focused.set(focused);
    // SAFETY: The caller guarantees callback registration remains live for this dispatch.
    unsafe { dispatch_input_event(state, InputEvent::FocusChanged { focused }) };
    if focused {
        // SAFETY: This is the window thread, so the thread-local key state is authoritative.
        unsafe { dispatch_modifiers_if_changed(state, win32_modifiers()) };
        if state.capture_requested.get() && !state.capture_engaged.get() {
            // Reapplying a stored capture intent is best-effort; a failure leaves the intent
            // pending for the next focus gain.
            // SAFETY: The window handle is live on its creating thread.
            let _ = unsafe { engage_pointer_capture(window, state) };
        }
    } else {
        state.captured_pointer_buttons.set(0);
        state.last_modifiers.set(Modifiers::default());
        // SAFETY: The clip and raw-input registration belong to this thread's window; the stored
        // capture intent survives for reapplication on the next focus gain.
        unsafe { release_pointer_capture(state) };
        // SAFETY: Capture, if present, belongs to this window on the current thread.
        if unsafe { GetCapture() } == window {
            unsafe { ReleaseCapture() };
        }
    }
}

/// Registers this window for raw mouse input, confines the cursor to the client area, and pins
/// it to the center. The cursor itself is hidden by the window's `WM_SETCURSOR` handling while
/// capture is engaged, so no global show-counter state is disturbed.
unsafe fn engage_pointer_capture(window: Hwnd, state: &WindowState) -> Result<(), PlatformError> {
    // SAFETY: The window handle is live and the bounds query writes only local values.
    let bounds = unsafe { client_bounds_on_screen(window)? };
    let device = RawInputDevice {
        usage_page: HID_USAGE_PAGE_GENERIC,
        usage: HID_USAGE_GENERIC_MOUSE,
        flags: 0,
        target: window,
    };
    // SAFETY: The device array is one live element of the declared size.
    if unsafe { RegisterRawInputDevices(&raw const device, 1, raw_input_device_size()) } == 0 {
        return Err(last_error("RegisterRawInputDevices"));
    }
    // SAFETY: The rectangle is live for the call; a failure undoes the raw-input registration.
    if unsafe { ClipCursor(&raw const bounds) } == 0 {
        let error = last_error("ClipCursor");
        // SAFETY: Removal targets the registration made above.
        unsafe { unregister_raw_mouse() };
        return Err(error);
    }
    // SAFETY: Centering is best-effort; the clip above already bounds the cursor.
    unsafe {
        SetCursorPos(
            i32::midpoint(bounds.left, bounds.right),
            i32::midpoint(bounds.top, bounds.bottom),
        )
    };
    state.last_raw_absolute.set(None);
    state.capture_engaged.set(true);
    Ok(())
}

/// Releases the clip and raw-input registration if capture is engaged. The stored capture
/// intent is untouched, so callers decide between suspension and full release.
unsafe fn release_pointer_capture(state: &WindowState) {
    if !state.capture_engaged.replace(false) {
        return;
    }
    // SAFETY: A null rectangle removes this process's cursor confinement; removal targets the
    // engage-time raw-input registration.
    unsafe {
        ClipCursor(ptr::null());
        unregister_raw_mouse();
    }
}

unsafe fn unregister_raw_mouse() {
    let device = RawInputDevice {
        usage_page: HID_USAGE_PAGE_GENERIC,
        usage: HID_USAGE_GENERIC_MOUSE,
        flags: RIDEV_REMOVE,
        target: ptr::null_mut(),
    };
    // SAFETY: The device array is one live element of the declared size; removal requires a null
    // target window.
    unsafe { RegisterRawInputDevices(&raw const device, 1, raw_input_device_size()) };
}

fn raw_input_device_size() -> u32 {
    u32::try_from(mem::size_of::<RawInputDevice>()).expect("RAWINPUTDEVICE size fits u32")
}

/// Re-derives the clip rectangle after the window moves or resizes while capture is engaged.
unsafe fn reclip_captured_cursor(window: Hwnd, state: &WindowState) {
    if !state.capture_engaged.get() {
        return;
    }
    // SAFETY: The window is live; a failed bounds query leaves the previous clip in place.
    if let Ok(bounds) = unsafe { client_bounds_on_screen(window) } {
        // SAFETY: The rectangle is live for the call.
        unsafe { ClipCursor(&raw const bounds) };
    }
}

/// Warps the hidden cursor back to the client center so absolute motion cannot reach the clip
/// edge; the warp's own echo motion lands exactly on the center and is filtered here.
unsafe fn recenter_captured_cursor(window: Hwnd, l_param: Lparam) {
    let mut client = Rect::default();
    // SAFETY: The window is live and the rectangle is writable.
    if unsafe { GetClientRect(window, &raw mut client) } == 0 {
        return;
    }
    let mut center = Point {
        x: (client.right - client.left) / 2,
        y: (client.bottom - client.top) / 2,
    };
    let position = pointer_position(l_param);
    if position == LogicalPosition::new(f64::from(center.x), f64::from(center.y)) {
        return;
    }
    // SAFETY: The point converts against this live window; the warp itself is best-effort.
    unsafe {
        if ClientToScreen(window, &raw mut center) != 0 {
            SetCursorPos(center.x, center.y);
        }
    }
}

unsafe fn client_bounds_on_screen(window: Hwnd) -> Result<Rect, PlatformError> {
    let mut client = Rect::default();
    // SAFETY: The window is live and the rectangle is writable.
    if unsafe { GetClientRect(window, &raw mut client) } == 0 {
        return Err(last_error("GetClientRect"));
    }
    if client.right <= client.left || client.bottom <= client.top {
        return Err(PlatformError::new(
            "pointer capture requires a non-empty client area",
        ));
    }
    let mut top_left = Point {
        x: client.left,
        y: client.top,
    };
    let mut bottom_right = Point {
        x: client.right,
        y: client.bottom,
    };
    // SAFETY: Both points are writable and convert against this live window.
    if unsafe { ClientToScreen(window, &raw mut top_left) } == 0
        || unsafe { ClientToScreen(window, &raw mut bottom_right) } == 0
    {
        return Err(last_error("ClientToScreen"));
    }
    Ok(Rect {
        left: top_left.x,
        top: top_left.y,
        right: bottom_right.x,
        bottom: bottom_right.y,
    })
}

/// Reads one raw mouse packet and reports its motion as a pointer delta.
unsafe fn dispatch_raw_pointer_delta(state: &WindowState, l_param: Lparam) {
    let mut data = RawInputMouse::default();
    let mut size = u32::try_from(mem::size_of::<RawInputMouse>()).expect("RAWINPUT size fits u32");
    let header_size =
        u32::try_from(mem::size_of::<RawInputHeader>()).expect("RAWINPUTHEADER size fits u32");
    // SAFETY: The buffer is writable at the declared size and the handle came from this message.
    let copied = unsafe {
        GetRawInputData(
            l_param as Handle,
            RID_INPUT,
            (&raw mut data).cast(),
            &raw mut size,
            header_size,
        )
    };
    if copied == u32::MAX || data.header.kind != RIM_TYPEMOUSE {
        return;
    }
    let (delta, absolute) = raw_mouse_delta(
        data.mouse.flags,
        data.mouse.last_x,
        data.mouse.last_y,
        state.last_raw_absolute.get(),
    );
    state.last_raw_absolute.set(absolute);
    if let Some((delta_x, delta_y)) = delta {
        // SAFETY: The caller guarantees callback registration remains live for this dispatch.
        unsafe {
            dispatch_input_event(
                state,
                InputEvent::PointerDelta {
                    delta_x,
                    delta_y,
                    modifiers: win32_modifiers(),
                },
            );
        }
    }
}

/// A raw-motion delta to report plus the absolute sample to retain for differencing.
type RawMouseMotion = (Option<(f64, f64)>, Option<(i32, i32)>);

/// Converts one raw mouse sample into a motion delta plus the absolute sample to retain.
///
/// Relative devices report deltas directly. Absolute devices (remote desktop, some tablets)
/// are differenced against the previous sample; the first sample only establishes the baseline.
fn raw_mouse_delta(
    flags: u16,
    x: i32,
    y: i32,
    previous_absolute: Option<(i32, i32)>,
) -> RawMouseMotion {
    if flags & MOUSE_MOVE_ABSOLUTE == 0 {
        let delta = (x != 0 || y != 0).then(|| (f64::from(x), f64::from(y)));
        (delta, None)
    } else {
        let delta = previous_absolute.and_then(|(previous_x, previous_y)| {
            let (delta_x, delta_y) = (x - previous_x, y - previous_y);
            (delta_x != 0 || delta_y != 0).then(|| (f64::from(delta_x), f64::from(delta_y)))
        });
        (delta, Some((x, y)))
    }
}

unsafe fn dispatch_keyboard_event(
    state: &WindowState,
    message: u32,
    virtual_key: Wparam,
    l_param: Lparam,
) {
    // SAFETY: Keyboard messages are being handled on their owning window thread.
    let modifiers = unsafe { win32_modifiers() };
    if is_modifier_key(virtual_key) {
        // SAFETY: The caller guarantees callback registration remains live for this dispatch.
        unsafe { dispatch_modifiers_if_changed(state, modifiers) };
        return;
    }
    let parameter_bits = l_param.cast_unsigned();
    let scan_code =
        u8::try_from((parameter_bits >> 16) & 0xff).expect("masked Win32 scan code fits u8");
    let extended = parameter_bits & (1 << 24) != 0;
    let state_value = if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) {
        ButtonState::Pressed
    } else {
        ButtonState::Released
    };
    let repeat = state_value == ButtonState::Pressed && l_param & (1 << 30) != 0;
    // SAFETY: The caller guarantees callback registration remains live for this dispatch.
    unsafe {
        dispatch_input_event(
            state,
            InputEvent::Keyboard {
                key: win32_key_code(scan_code, extended),
                state: state_value,
                repeat,
                modifiers,
            },
        );
    }
}

#[allow(clippy::too_many_lines)]
fn win32_key_code(scan_code: u8, extended: bool) -> KeyCode {
    match (scan_code, extended) {
        (0x01, _) => KeyCode::Escape,
        (0x02, _) => KeyCode::Digit1,
        (0x03, _) => KeyCode::Digit2,
        (0x04, _) => KeyCode::Digit3,
        (0x05, _) => KeyCode::Digit4,
        (0x06, _) => KeyCode::Digit5,
        (0x07, _) => KeyCode::Digit6,
        (0x08, _) => KeyCode::Digit7,
        (0x09, _) => KeyCode::Digit8,
        (0x0a, _) => KeyCode::Digit9,
        (0x0b, _) => KeyCode::Digit0,
        (0x0c, _) => KeyCode::Minus,
        (0x0d, _) => KeyCode::Equal,
        (0x0e, _) => KeyCode::Backspace,
        (0x0f, _) => KeyCode::Tab,
        (0x10, _) => KeyCode::KeyQ,
        (0x11, _) => KeyCode::KeyW,
        (0x12, _) => KeyCode::KeyE,
        (0x13, _) => KeyCode::KeyR,
        (0x14, _) => KeyCode::KeyT,
        (0x15, _) => KeyCode::KeyY,
        (0x16, _) => KeyCode::KeyU,
        (0x17, _) => KeyCode::KeyI,
        (0x18, _) => KeyCode::KeyO,
        (0x19, _) => KeyCode::KeyP,
        (0x1a, _) => KeyCode::BracketLeft,
        (0x1b, _) => KeyCode::BracketRight,
        (0x1c, false) => KeyCode::Enter,
        (0x1c, true) => KeyCode::NumpadEnter,
        (0x1e, _) => KeyCode::KeyA,
        (0x1f, _) => KeyCode::KeyS,
        (0x20, _) => KeyCode::KeyD,
        (0x21, _) => KeyCode::KeyF,
        (0x22, _) => KeyCode::KeyG,
        (0x23, _) => KeyCode::KeyH,
        (0x24, _) => KeyCode::KeyJ,
        (0x25, _) => KeyCode::KeyK,
        (0x26, _) => KeyCode::KeyL,
        (0x27, _) => KeyCode::Semicolon,
        (0x28, _) => KeyCode::Quote,
        (0x29, _) => KeyCode::Backquote,
        (0x2b, _) => KeyCode::Backslash,
        (0x2c, _) => KeyCode::KeyZ,
        (0x2d, _) => KeyCode::KeyX,
        (0x2e, _) => KeyCode::KeyC,
        (0x2f, _) => KeyCode::KeyV,
        (0x30, _) => KeyCode::KeyB,
        (0x31, _) => KeyCode::KeyN,
        (0x32, _) => KeyCode::KeyM,
        (0x33, _) => KeyCode::Comma,
        (0x34, _) => KeyCode::Period,
        (0x35, false) => KeyCode::Slash,
        (0x35, true) => KeyCode::NumpadDivide,
        (0x37, false) => KeyCode::NumpadMultiply,
        (0x39, _) => KeyCode::Space,
        (0x3b, _) => KeyCode::F1,
        (0x3c, _) => KeyCode::F2,
        (0x3d, _) => KeyCode::F3,
        (0x3e, _) => KeyCode::F4,
        (0x3f, _) => KeyCode::F5,
        (0x40, _) => KeyCode::F6,
        (0x41, _) => KeyCode::F7,
        (0x42, _) => KeyCode::F8,
        (0x43, _) => KeyCode::F9,
        (0x44, _) => KeyCode::F10,
        (0x47, false) => KeyCode::Numpad7,
        (0x47, true) => KeyCode::Home,
        (0x48, false) => KeyCode::Numpad8,
        (0x48, true) => KeyCode::ArrowUp,
        (0x49, false) => KeyCode::Numpad9,
        (0x49, true) => KeyCode::PageUp,
        (0x4a, false) => KeyCode::NumpadSubtract,
        (0x4b, false) => KeyCode::Numpad4,
        (0x4b, true) => KeyCode::ArrowLeft,
        (0x4c, false) => KeyCode::Numpad5,
        (0x4d, false) => KeyCode::Numpad6,
        (0x4d, true) => KeyCode::ArrowRight,
        (0x4e, false) => KeyCode::NumpadAdd,
        (0x4f, false) => KeyCode::Numpad1,
        (0x4f, true) => KeyCode::End,
        (0x50, false) => KeyCode::Numpad2,
        (0x50, true) => KeyCode::ArrowDown,
        (0x51, false) => KeyCode::Numpad3,
        (0x51, true) => KeyCode::PageDown,
        (0x52, false) => KeyCode::Numpad0,
        (0x52, true) => KeyCode::Insert,
        (0x53, false) => KeyCode::NumpadDecimal,
        (0x53, true) => KeyCode::Delete,
        (0x57, _) => KeyCode::F11,
        (0x58, _) => KeyCode::F12,
        (0x64, _) => KeyCode::F13,
        (0x65, _) => KeyCode::F14,
        (0x66, _) => KeyCode::F15,
        (0x67, _) => KeyCode::F16,
        (0x68, _) => KeyCode::F17,
        (0x69, _) => KeyCode::F18,
        (0x6a, _) => KeyCode::F19,
        (0x6b, _) => KeyCode::F20,
        _ => KeyCode::Unidentified(u32::from(scan_code) | u32::from(extended) << 8),
    }
}

fn is_modifier_key(virtual_key: Wparam) -> bool {
    matches!(
        virtual_key,
        VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN | VK_CAPITAL
    )
}

unsafe fn win32_modifiers() -> Modifiers {
    let mut bits = 0;
    // SAFETY: GetKeyState reads thread-local keyboard state and accepts these documented keys.
    if unsafe { key_is_down(VK_SHIFT) } {
        bits |= Modifiers::SHIFT;
    }
    if unsafe { key_is_down(VK_CONTROL) } {
        bits |= Modifiers::CONTROL;
    }
    if unsafe { key_is_down(VK_MENU) } {
        bits |= Modifiers::ALT;
    }
    if unsafe { key_is_down(VK_LWIN) || key_is_down(VK_RWIN) } {
        bits |= Modifiers::SUPER;
    }
    if unsafe { GetKeyState(c_int::try_from(VK_CAPITAL).expect("virtual key fits c_int")) } & 1 != 0
    {
        bits |= Modifiers::CAPS_LOCK;
    }
    Modifiers::from_bits(bits)
}

unsafe fn key_is_down(virtual_key: Wparam) -> bool {
    // SAFETY: The virtual key fits c_int and GetKeyState has no pointer preconditions.
    let virtual_key = c_int::try_from(virtual_key).expect("virtual key fits c_int");
    (unsafe { GetKeyState(virtual_key) }) < 0
}

unsafe fn dispatch_modifiers_if_changed(state: &WindowState, modifiers: Modifiers) {
    if state.last_modifiers.replace(modifiers) != modifiers {
        // SAFETY: The caller guarantees callback registration remains live for this dispatch.
        unsafe { dispatch_input_event(state, InputEvent::ModifiersChanged(modifiers)) };
    }
}

fn pointer_position(l_param: Lparam) -> LogicalPosition {
    let packed = l_param.cast_unsigned();
    let x = f64::from(low_word(packed).cast_signed());
    let y = f64::from(high_word(packed).cast_signed());
    LogicalPosition::new(x, y)
}

fn low_word(value: usize) -> u16 {
    u16::try_from(value & 0xffff).expect("masked Win32 low word fits u16")
}

fn high_word(value: usize) -> u16 {
    u16::try_from((value >> 16) & 0xffff).expect("masked Win32 high word fits u16")
}

fn pointer_button(message: u32, w_param: Wparam) -> Option<(PointerButton, u16, ButtonState)> {
    let state = if matches!(
        message,
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
    ) {
        ButtonState::Pressed
    } else {
        ButtonState::Released
    };
    match message {
        WM_LBUTTONDOWN | WM_LBUTTONUP => Some((PointerButton::Primary, 1 << 0, state)),
        WM_RBUTTONDOWN | WM_RBUTTONUP => Some((PointerButton::Secondary, 1 << 1, state)),
        WM_MBUTTONDOWN | WM_MBUTTONUP => Some((PointerButton::Middle, 1 << 2, state)),
        WM_XBUTTONDOWN | WM_XBUTTONUP => match high_word(w_param) {
            1 => Some((PointerButton::Other(3), 1 << 3, state)),
            2 => Some((PointerButton::Other(4), 1 << 4, state)),
            _ => None,
        },
        _ => None,
    }
}

unsafe fn dispatch_pointer_button(
    window: Hwnd,
    state: &WindowState,
    button: PointerButton,
    mask: u16,
    button_state: ButtonState,
    l_param: Lparam,
) {
    let position = pointer_position(l_param);
    state.last_pointer_position.set(position);
    match button_state {
        ButtonState::Pressed => {
            state
                .captured_pointer_buttons
                .set(state.captured_pointer_buttons.get() | mask);
            // SAFETY: The handle identifies the window currently receiving this button press.
            unsafe { SetCapture(window) };
        }
        ButtonState::Released => {
            state
                .captured_pointer_buttons
                .set(state.captured_pointer_buttons.get() & !mask);
        }
    }
    // SAFETY: The caller guarantees callback registration remains live for this dispatch.
    unsafe {
        dispatch_input_event(
            state,
            InputEvent::PointerButton {
                button,
                state: button_state,
                position,
                modifiers: win32_modifiers(),
            },
        );
    }
    if button_state == ButtonState::Released
        && state.captured_pointer_buttons.get() == 0
        && unsafe { GetCapture() } == window
    {
        // SAFETY: This window owns capture and has no remaining pressed pointer buttons.
        unsafe { ReleaseCapture() };
    }
}

unsafe fn dispatch_scroll_event(
    window: Hwnd,
    state: &WindowState,
    message: u32,
    w_param: Wparam,
    l_param: Lparam,
) {
    let packed = l_param.cast_unsigned();
    let mut point = Point {
        x: i32::from(low_word(packed).cast_signed()),
        y: i32::from(high_word(packed).cast_signed()),
    };
    // SAFETY: The point is writable and the handle identifies this live window.
    if unsafe { ScreenToClient(window, &raw mut point) } == 0 {
        return;
    }
    let mut client = Rect::default();
    // SAFETY: The rectangle is writable and the handle identifies this live window.
    if unsafe { GetClientRect(window, &raw mut client) } == 0
        || point.x < client.left
        || point.y < client.top
        || point.x >= client.right
        || point.y >= client.bottom
    {
        return;
    }
    let steps = f64::from(high_word(w_param).cast_signed()) / WHEEL_DELTA;
    let delta = if message == WM_MOUSEHWHEEL {
        ScrollDelta::Coarse { x: steps, y: 0.0 }
    } else {
        ScrollDelta::Coarse { x: 0.0, y: steps }
    };
    let position = LogicalPosition::new(f64::from(point.x), f64::from(point.y));
    state.last_pointer_position.set(position);
    // SAFETY: The caller guarantees callback registration remains live for this dispatch.
    unsafe {
        dispatch_input_event(
            state,
            InputEvent::Scroll {
                delta,
                position,
                modifiers: win32_modifiers(),
            },
        );
    }
}

unsafe fn release_captured_buttons(state: &WindowState) {
    let masks = state.captured_pointer_buttons.replace(0);
    let position = state.last_pointer_position.get();
    for (mask, button) in [
        (1 << 0, PointerButton::Primary),
        (1 << 1, PointerButton::Secondary),
        (1 << 2, PointerButton::Middle),
        (1 << 3, PointerButton::Other(3)),
        (1 << 4, PointerButton::Other(4)),
    ] {
        if masks & mask != 0 {
            // SAFETY: The caller guarantees callback registration remains live for this dispatch.
            unsafe {
                dispatch_input_event(
                    state,
                    InputEvent::PointerButton {
                        button,
                        state: ButtonState::Released,
                        position,
                        modifiers: win32_modifiers(),
                    },
                );
            }
        }
    }
}

unsafe fn dispatch_input_event(state: &WindowState, event: InputEvent) {
    // SAFETY: The caller guarantees callback registration remains live for this dispatch.
    unsafe { invoke_event_callback(state, WindowEvent::Input(event)) };
}

unsafe fn dispatch_window_events(window: Hwnd, state: &WindowState) {
    let current = match current_window_metrics(window, state) {
        Ok(current) => current,
        Err(error) => {
            state.callback_error.replace(Some(error));
            return;
        }
    };
    let previous = state.last_metrics.get();
    if let Some(event) = metrics_transition(previous, current) {
        // SAFETY: The caller guarantees callback registration remains live for this dispatch.
        unsafe { invoke_event_callback(state, event) };
    }
    if let Some(metrics) = current {
        // SAFETY: The caller guarantees callback registration remains live for this dispatch.
        unsafe { invoke_event_callback(state, WindowEvent::RedrawRequested(metrics)) };
    }
    state.last_metrics.set(current);
}

unsafe fn invoke_event_callback(state: &WindowState, event: WindowEvent) {
    if let Some(callback) = state.event_callback.get()
        && !state.callback_active.replace(true)
    {
        // SAFETY: The registration owns a live exclusive callback borrow on this thread.
        unsafe { callback(state.event_context.get(), event) };
        state.callback_active.set(false);
    }
}

unsafe fn invoke_callback<F>(context: *mut c_void, event: WindowEvent)
where
    F: FnMut(WindowEvent),
{
    // SAFETY: `pump_events` installed this pointer from a live exclusive borrow of F and clears the
    // callback before that borrow expires. Window callbacks execute synchronously on this thread.
    unsafe { (&mut *context.cast::<F>())(event) };
}

fn current_window_metrics(
    window: Hwnd,
    state: &WindowState,
) -> Result<Option<WindowMetrics>, PlatformError> {
    // SAFETY: A destroyed handle is a normal closed-window state and is not queried further.
    if unsafe { IsWindow(window) } == 0 {
        return Ok(None);
    }
    let mut rectangle = Rect::default();
    // SAFETY: The window is live and `rectangle` is writable.
    if unsafe { GetClientRect(window, &raw mut rectangle) } == 0 {
        return Err(last_error("GetClientRect"));
    }
    let width = u32::try_from(rectangle.right - rectangle.left).unwrap_or(0);
    let height = u32::try_from(rectangle.bottom - rectangle.top).unwrap_or(0);
    let extent = PhysicalExtent::new(width, height);
    if extent.is_empty() {
        return Ok(None);
    }
    let revision = if state.last_extent.get() == PhysicalExtent::default() {
        state.revision.get()
    } else if state.last_extent.get() != extent {
        let next = state.revision.get().next();
        state.revision.set(next);
        next
    } else {
        state.revision.get()
    };
    state.last_extent.set(extent);
    // The proven Win32 probe uses client pixels directly. Per-monitor DPI and logical-size evidence
    // remains pending, so this first extraction reports the same explicit scale factor as Linux.
    Ok(Some(WindowMetrics::new(extent, 1.0, revision)))
}

fn metrics_transition(
    previous: Option<WindowMetrics>,
    current: Option<WindowMetrics>,
) -> Option<WindowEvent> {
    match (previous, current) {
        (Some(old), Some(new)) if old.revision() != new.revision() => {
            Some(WindowEvent::MetricsChanged(new))
        }
        (Some(_), None) => Some(WindowEvent::RenderingSuspended),
        (None, Some(metrics)) => Some(WindowEvent::RenderingResumed(metrics)),
        _ => None,
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn last_error(operation: &str) -> PlatformError {
    // SAFETY: GetLastError has no preconditions and returns thread-local state.
    PlatformError::new(format!("{operation} failed with Win32 error {}", unsafe {
        GetLastError()
    }))
}

/// Backend integration details for Mulciber's native Vulkan implementation.
#[doc(hidden)]
pub mod integration {
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use super::{Application, SurfaceTarget, Window};
    use crate::{PlatformError, WindowDescriptor};

    /// Borrowed native handles used to create a Vulkan Win32 surface.
    #[derive(Clone, Copy)]
    pub struct Win32SurfaceTarget {
        /// The process module instance owning the window class.
        pub instance: NonNull<c_void>,
        /// The native Win32 window handle.
        pub window: NonNull<c_void>,
    }

    /// Exposes native handles while `target` and its source window remain alive.
    ///
    /// # Safety
    ///
    /// The returned handles must not be retained beyond the source window, destroyed, or used from
    /// another thread.
    #[must_use]
    pub unsafe fn native_surface_target(target: &SurfaceTarget<'_>) -> Win32SurfaceTarget {
        Win32SurfaceTarget {
            instance: target.instance,
            window: target.window,
        }
    }

    /// Returns whether event delivery is currently inside Win32's nested interactive-resize loop.
    #[must_use]
    pub fn in_live_resize(window: &Window) -> bool {
        window.state.in_size_move.get()
    }

    /// Creates a visible or hidden Win32 window for native validation probes.
    ///
    /// # Errors
    ///
    /// Returns the same creation errors as [`Application::create_window`].
    pub fn create_window(
        application: &Application,
        descriptor: &WindowDescriptor,
        visible: bool,
    ) -> Result<Window, PlatformError> {
        application.create_window_with_visibility(descriptor, visible)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MOUSE_MOVE_ABSOLUTE, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_XBUTTONDOWN, WindowSlot,
        metrics_transition, pointer_button, pointer_position, raw_mouse_delta, win32_key_code,
    };
    use crate::{
        ButtonState, KeyCode, LogicalPosition, PhysicalExtent, PointerButton, WindowEvent,
        WindowMetrics, WindowRevision,
    };

    fn metrics(revision: WindowRevision) -> WindowMetrics {
        WindowMetrics::new(PhysicalExtent::new(960, 540), 1.0, revision)
    }

    #[test]
    fn lifecycle_transitions_preserve_revision_and_suspend_semantics() {
        let first = metrics(WindowRevision::INITIAL);
        let resized = metrics(WindowRevision::INITIAL.next());
        assert_eq!(
            metrics_transition(Some(first), Some(resized)),
            Some(WindowEvent::MetricsChanged(resized))
        );
        assert_eq!(
            metrics_transition(Some(resized), None),
            Some(WindowEvent::RenderingSuspended)
        );
        assert_eq!(
            metrics_transition(None, Some(resized)),
            Some(WindowEvent::RenderingResumed(resized))
        );
    }

    #[test]
    fn window_slot_releases_when_the_window_lease_drops() {
        let slot = WindowSlot::new();
        let lease = slot.claim().expect("first window should claim the slot");
        assert!(slot.claim().is_err());
        drop(lease);
        assert!(slot.claim().is_ok());
    }

    #[test]
    fn physical_key_mapping_distinguishes_navigation_and_numpad_keys() {
        assert_eq!(win32_key_code(0x11, false), KeyCode::KeyW);
        assert_eq!(win32_key_code(0x48, true), KeyCode::ArrowUp);
        assert_eq!(win32_key_code(0x48, false), KeyCode::Numpad8);
        assert_eq!(win32_key_code(0x1c, true), KeyCode::NumpadEnter);
        assert_eq!(win32_key_code(0x6b, false), KeyCode::F20);
        assert_eq!(win32_key_code(0x7f, true), KeyCode::Unidentified(0x17f));
    }

    #[test]
    fn pointer_coordinates_preserve_signed_client_positions() {
        let packed = (i32::from(-7_i16) << 16) | i32::from((-11_i16).cast_unsigned());
        assert_eq!(
            pointer_position(isize::try_from(packed).expect("packed coordinates fit LPARAM")),
            LogicalPosition::new(-11.0, -7.0)
        );
    }

    #[test]
    fn relative_raw_motion_reports_deltas_and_keeps_no_absolute_baseline() {
        assert_eq!(raw_mouse_delta(0, 3, -2, None), (Some((3.0, -2.0)), None));
        assert_eq!(raw_mouse_delta(0, 0, 0, Some((5, 5))), (None, None));
    }

    #[test]
    fn absolute_raw_motion_differences_against_the_previous_sample() {
        assert_eq!(
            raw_mouse_delta(MOUSE_MOVE_ABSOLUTE, 100, 40, None),
            (None, Some((100, 40)))
        );
        assert_eq!(
            raw_mouse_delta(MOUSE_MOVE_ABSOLUTE, 103, 38, Some((100, 40))),
            (Some((3.0, -2.0)), Some((103, 38)))
        );
        assert_eq!(
            raw_mouse_delta(MOUSE_MOVE_ABSOLUTE, 103, 38, Some((103, 38))),
            (None, Some((103, 38)))
        );
    }

    #[test]
    fn pointer_button_mapping_preserves_state_and_extended_identity() {
        assert_eq!(
            pointer_button(WM_LBUTTONDOWN, 0),
            Some((PointerButton::Primary, 1, ButtonState::Pressed))
        );
        assert_eq!(
            pointer_button(WM_LBUTTONUP, 0),
            Some((PointerButton::Primary, 1, ButtonState::Released))
        );
        assert_eq!(
            pointer_button(WM_XBUTTONDOWN, 2 << 16),
            Some((PointerButton::Other(4), 1 << 4, ButtonState::Pressed))
        );
    }
}
