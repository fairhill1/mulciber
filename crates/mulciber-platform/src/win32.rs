//! Native Win32 window ownership, thread-message dispatch, and nested resize lifecycle.

use std::cell::{Cell, RefCell};
use std::ffi::{c_int, c_void};
use std::marker::PhantomData;
use std::mem;
use std::ptr::{self, NonNull};
use std::rc::Rc;

use crate::{
    PhysicalExtent, PlatformError, PumpStatus, WindowDescriptor, WindowEvent, WindowMetrics,
    WindowRevision,
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
    fn RegisterClassExW(class: *const WindowClassExW) -> Atom;
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
            return Err(PlatformError::new(
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
            return Err(PlatformError::new(
                "Win32 window title contains an interior NUL",
            ));
        }
        let size = descriptor.logical_size();
        let width = i32::try_from(size.width())
            .map_err(|_| PlatformError::new("window width is too large for Win32"))?;
        let height = i32::try_from(size.height())
            .map_err(|_| PlatformError::new("window height is too large for Win32"))?;
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
                return Err(PlatformError::new(
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
            return Err(PlatformError::new(
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
        // SAFETY: These resources were created by this value and are released once.
        unsafe {
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
                unsafe { dispatch_window_events(window, state) };
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
    use super::{WindowSlot, metrics_transition};
    use crate::{PhysicalExtent, WindowEvent, WindowMetrics, WindowRevision};

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
}
