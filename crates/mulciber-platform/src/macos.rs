use std::cell::Cell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::marker::PhantomData;
use std::mem;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::OnceLock;

use crate::{
    ButtonState, CursorMode, InputEvent, KeyCode, LogicalPosition, Modifiers, PhysicalExtent,
    PlatformError, PlatformErrorKind, PointerButton, PumpStatus, ScrollDelta, WindowDescriptor,
    WindowEvent, WindowMetrics, WindowMode, WindowRevision,
};

type Object = *mut c_void;
type Selector = *mut c_void;

const OCCLUSION_STATE_VISIBLE: usize = 1 << 1;
const ASSOCIATION_ASSIGN: usize = 0;
/// `NSWindowCollectionBehaviorFullScreenPrimary`: the window may own a fullscreen Space.
const COLLECTION_BEHAVIOR_FULL_SCREEN_PRIMARY: usize = 1 << 7;
/// `NSWindowStyleMaskFullScreen`: set on the window's style mask while it occupies a Space.
const STYLE_MASK_FULL_SCREEN: usize = 1 << 14;
const UTF8_STRING_ENCODING: usize = 4;
const EVENT_LEFT_MOUSE_DOWN: usize = 1;
const EVENT_LEFT_MOUSE_UP: usize = 2;
const EVENT_RIGHT_MOUSE_DOWN: usize = 3;
const EVENT_RIGHT_MOUSE_UP: usize = 4;
const EVENT_MOUSE_MOVED: usize = 5;
const EVENT_LEFT_MOUSE_DRAGGED: usize = 6;
const EVENT_RIGHT_MOUSE_DRAGGED: usize = 7;
const EVENT_KEY_DOWN: usize = 10;
const EVENT_KEY_UP: usize = 11;
const EVENT_FLAGS_CHANGED: usize = 12;
const EVENT_SCROLL_WHEEL: usize = 22;
const EVENT_OTHER_MOUSE_DOWN: usize = 25;
const EVENT_OTHER_MOUSE_UP: usize = 26;
const EVENT_OTHER_MOUSE_DRAGGED: usize = 27;
const MODIFIER_CAPS_LOCK: usize = 1 << 16;
const MODIFIER_SHIFT: usize = 1 << 17;
const MODIFIER_CONTROL: usize = 1 << 18;
const MODIFIER_OPTION: usize = 1 << 19;
const MODIFIER_COMMAND: usize = 1 << 20;
const MODIFIER_FUNCTION: usize = 1 << 23;
static WINDOW_STATE_KEY: u8 = 0;

struct WindowDelegateState {
    close_requested: Cell<bool>,
    focused: Cell<bool>,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Size {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    origin: Point,
    size: Size,
}

#[link(name = "objc")]
unsafe extern "C" {
    fn class_addMethod(
        class: Object,
        name: Selector,
        implementation: *const c_void,
        types: *const c_char,
    ) -> bool;
    fn objc_allocateClassPair(
        superclass: Object,
        name: *const c_char,
        extra_bytes: usize,
    ) -> Object;
    fn objc_disposeClassPair(class: Object);
    fn objc_getClass(name: *const c_char) -> Object;
    fn objc_getAssociatedObject(object: Object, key: *const c_void) -> Object;
    fn objc_msgSend();
    fn objc_registerClassPair(class: Object);
    fn objc_setAssociatedObject(object: Object, key: *const c_void, value: Object, policy: usize);
    fn sel_registerName(name: *const c_char) -> Selector;
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    static NSDefaultRunLoopMode: Object;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32;
    fn CGWarpMouseCursorPosition(position: Point) -> i32;
}

#[link(name = "System")]
unsafe extern "C" {
    fn pthread_main_np() -> c_int;
}

/// The `AppKit` application connection and native event pump.
pub struct Application {
    raw: NonNull<c_void>,
    window_slot: WindowSlot,
    _main_thread: PhantomData<Rc<()>>,
}

impl Application {
    /// Connects to `AppKit` on the process main thread.
    ///
    /// # Errors
    ///
    /// Returns an error when called off the process main thread or when the required `AppKit`
    /// application objects cannot be created.
    pub fn new() -> Result<Self, PlatformError> {
        // SAFETY: `pthread_main_np` has no preconditions and reports the current thread.
        if unsafe { pthread_main_np() } == 0 {
            return Err(PlatformError::lifecycle(
                "AppKit application creation must run on the process main thread",
            ));
        }

        let _pool = AutoreleasePool::new()?;
        // SAFETY: The current thread is the main thread and selectors match the AppKit SDK ABI.
        unsafe {
            let application = required(
                object(class(c"NSApplication")?, c"sharedApplication"),
                "NSApplication",
            )?;
            if !bool_isize(application.as_ptr(), c"setActivationPolicy:", 0) {
                return Err(PlatformError::new(
                    "could not activate as a regular AppKit application",
                ));
            }
            void(application.as_ptr(), c"finishLaunching");
            Ok(Self {
                raw: application,
                window_slot: WindowSlot::new(),
                _main_thread: PhantomData,
            })
        }
    }

    /// Creates and shows one native window.
    ///
    /// # Errors
    ///
    /// Returns an error when a window is already alive, for an empty requested extent, or when
    /// `AppKit` cannot create the window, title, or content view.
    pub fn create_window(&self, descriptor: &WindowDescriptor) -> Result<Window, PlatformError> {
        if descriptor.logical_size().is_empty() {
            return Err(PlatformError::invalid_request(
                "window creation requires a non-empty logical extent",
            ));
        }
        let window_lease = self.window_slot.claim()?;

        let _pool = AutoreleasePool::new()?;
        // SAFETY: Application and all created objects are used on AppKit's main thread. Selectors and
        // aggregate argument layouts match the macOS SDK ABI.
        unsafe {
            let style = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3);
            let size = descriptor.logical_size();
            let initial_rect = Rect {
                origin: Point::default(),
                size: Size {
                    width: f64::from(size.width()),
                    height: f64::from(size.height()),
                },
            };
            let allocated = object(class(c"NSWindow")?, c"alloc");
            let window = required(
                object_window_init(
                    allocated,
                    c"initWithContentRect:styleMask:backing:defer:",
                    initial_rect,
                    style,
                    2,
                    false,
                ),
                "NSWindow",
            )?;
            void_bool(window.as_ptr(), c"setReleasedWhenClosed:", false);

            let title = owned_string(descriptor.title())?;
            void_object(window.as_ptr(), c"setTitle:", title.as_ptr());
            void(title.as_ptr(), c"release");

            let view = create_content_view(initial_rect.size)?;
            void_object(window.as_ptr(), c"setContentView:", view.as_ptr());
            void(view.as_ptr(), c"release");
            void_bool(view.as_ptr(), c"setWantsLayer:", true);
            void_bool(window.as_ptr(), c"setAcceptsMouseMovedEvents:", true);
            void_usize(
                window.as_ptr(),
                c"setCollectionBehavior:",
                COLLECTION_BEHAVIOR_FULL_SCREEN_PRIMARY,
            );
            if !bool_object(window.as_ptr(), c"makeFirstResponder:", view.as_ptr()) {
                return Err(PlatformError::new(
                    "could not make the Mulciber content view AppKit's first responder",
                ));
            }

            let delegate_state = Rc::new(WindowDelegateState {
                close_requested: Cell::new(false),
                focused: Cell::new(false),
            });
            let delegate = create_window_delegate(&delegate_state)?;
            void_object(window.as_ptr(), c"setDelegate:", delegate.as_ptr());

            void(window.as_ptr(), c"center");
            void_object(
                window.as_ptr(),
                c"makeKeyAndOrderFront:",
                core::ptr::null_mut(),
            );
            void(self.raw.as_ptr(), c"activate");

            let focused = bool_value(window.as_ptr(), c"isKeyWindow");
            delegate_state.focused.set(focused);

            let result = Window {
                raw: window,
                view,
                revision: Cell::new(WindowRevision::INITIAL),
                last_extent: Cell::new(PhysicalExtent::default()),
                last_scale_factor: Cell::new(0.0),
                last_metrics: Cell::new(None),
                delegate_state,
                close_reported: Cell::new(false),
                last_focused: Cell::new(focused),
                captured_pointer_buttons: Cell::new(0),
                cursor_mode: Cell::new(CursorMode::Normal),
                capture_applied: Cell::new(false),
                fullscreen_requested: Cell::new(false),
                fullscreen_confirmed: Cell::new(false),
                delegate,
                _window_lease: window_lease,
                _main_thread: PhantomData,
            };
            let initial_metrics = result.current_window_metrics();
            result.last_metrics.set(initial_metrics);
            Ok(result)
        }
    }

    /// Dispatches queued native events and reports game-facing lifecycle events for `window`.
    ///
    /// The first handler error stops delivery of this call's remaining events; platform state
    /// still advances so a later pump does not replay the dropped events.
    ///
    /// # Errors
    ///
    /// Returns a converted platform error when a required `AppKit` event object or autorelease
    /// pool is unavailable, otherwise the first error returned by `handler`.
    pub fn pump_events<E>(
        &mut self,
        window: &Window,
        mut handler: impl FnMut(WindowEvent) -> Result<(), E>,
    ) -> Result<PumpStatus, E>
    where
        E: From<PlatformError>,
    {
        let mut handler_error = None;
        let status = self.pump_native_events(window, |event| {
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

    fn pump_native_events(
        &mut self,
        window: &Window,
        mut handler: impl FnMut(WindowEvent),
    ) -> Result<PumpStatus, PlatformError> {
        let _pool = AutoreleasePool::new()?;
        // SAFETY: The application is pumped on its creating main thread and the event selectors match
        // the AppKit SDK ABI. `NSDefaultRunLoopMode` is supplied by the linked framework.
        unsafe {
            let date = object(class(c"NSDate")?, c"distantPast");
            loop {
                let event = object_event(
                    self.raw.as_ptr(),
                    c"nextEventMatchingMask:untilDate:inMode:dequeue:",
                    usize::MAX,
                    date,
                    NSDefaultRunLoopMode,
                    true,
                );
                if event.is_null() {
                    break;
                }
                let input = window.translate_input_event(event);
                void_object(self.raw.as_ptr(), c"sendEvent:", event);
                if let Some(focus) = window.take_focus_transition() {
                    handler(WindowEvent::Input(focus));
                }
                if let Some(input) = input {
                    handler(WindowEvent::Input(input));
                }
            }
            void(self.raw.as_ptr(), c"updateWindows");
        }

        if !window.is_open() {
            if !window.close_reported.replace(true) {
                handler(WindowEvent::CloseRequested);
            }
            window.last_metrics.set(None);
            return Ok(PumpStatus::Exit);
        }

        if let Some(focus) = window.take_focus_transition() {
            handler(WindowEvent::Input(focus));
        }

        let previous = window.last_metrics.get();
        let current = window.current_window_metrics();
        if let Some(event) = metrics_transition(previous, current) {
            handler(event);
        }
        if let Some(info) = current {
            handler(WindowEvent::RedrawRequested(info));
        }
        window.last_metrics.set(current);
        Ok(PumpStatus::Continue)
    }
}

/// An owned `AppKit` window that remains confined to its creating main thread.
pub struct Window {
    raw: NonNull<c_void>,
    view: NonNull<c_void>,
    revision: Cell<WindowRevision>,
    last_extent: Cell<PhysicalExtent>,
    last_scale_factor: Cell<f64>,
    last_metrics: Cell<Option<WindowMetrics>>,
    delegate_state: Rc<WindowDelegateState>,
    close_reported: Cell<bool>,
    last_focused: Cell<bool>,
    captured_pointer_buttons: Cell<u32>,
    cursor_mode: Cell<CursorMode>,
    capture_applied: Cell<bool>,
    fullscreen_requested: Cell<bool>,
    fullscreen_confirmed: Cell<bool>,
    delegate: NonNull<c_void>,
    _window_lease: WindowLease,
    _main_thread: PhantomData<Rc<()>>,
}

impl Window {
    /// Returns the current drawable metrics, or `None` while rendering should be suspended.
    #[must_use]
    pub fn rendering_metrics(&self) -> Option<WindowMetrics> {
        self.current_window_metrics()
    }

    /// Returns a borrowed opaque target accepted by Mulciber's graphics surface creation.
    #[must_use]
    pub fn surface_target(&self) -> SurfaceTarget<'_> {
        SurfaceTarget {
            appkit_view: self.view,
            _window: PhantomData,
        }
    }

    /// Requests how this window interacts with the system pointer.
    ///
    /// Capture applies while the window is focused: the cursor is hidden and pinned at the
    /// content-view center, and motion arrives as [`InputEvent::PointerDelta`] instead of
    /// absolute positions. The requested mode persists across focus loss and is reapplied when
    /// focus returns; dropping the window always restores the system cursor.
    ///
    /// # Errors
    ///
    /// Returns an error when the native cursor services refuse the transition; the previous
    /// association state is restored before reporting.
    pub fn set_cursor_mode(&self, mode: CursorMode) -> Result<(), PlatformError> {
        self.cursor_mode.set(mode);
        match mode {
            CursorMode::Captured if self.delegate_state.focused.get() => {
                self.apply_pointer_capture()
            }
            CursorMode::Captured => Ok(()),
            CursorMode::Normal => self.release_pointer_capture(),
        }
    }

    /// Returns the requested cursor mode, whether or not focus currently lets it apply.
    #[must_use]
    pub fn cursor_mode(&self) -> CursorMode {
        self.cursor_mode.get()
    }

    /// Requests whether this window occupies its display as a fullscreen Space or shares the
    /// desktop.
    ///
    /// The transition drives `AppKit`'s native `toggleFullScreen:` animation asynchronously; the
    /// resulting extent change arrives through the ordinary metrics events, and the confirmed
    /// state updates [`Self::window_mode`].
    ///
    /// # Errors
    ///
    /// This request currently cannot fail; it returns a result so the game-facing contract stays
    /// uniform with the platforms whose display servers can refuse fullscreen.
    pub fn set_window_mode(&self, mode: WindowMode) -> Result<(), PlatformError> {
        self.sync_confirmed_fullscreen();
        let fullscreen = mode == WindowMode::Fullscreen;
        if self.fullscreen_requested.replace(fullscreen) == fullscreen {
            return Ok(());
        }
        // SAFETY: The window is alive on AppKit's main thread; toggleFullScreen: accepts a nil
        // sender and animates the transition the collection behavior opted into.
        unsafe {
            void_object(self.raw.as_ptr(), c"toggleFullScreen:", core::ptr::null_mut());
        }
        Ok(())
    }

    /// Returns the requested window mode, following `AppKit`-confirmed transitions so a toggle
    /// stays correct when the user enters or leaves the fullscreen Space through the window
    /// controls or Mission Control.
    #[must_use]
    pub fn window_mode(&self) -> WindowMode {
        self.sync_confirmed_fullscreen();
        if self.fullscreen_requested.get() {
            WindowMode::Fullscreen
        } else {
            WindowMode::Windowed
        }
    }

    /// Drags the requested mode along confirmed fullscreen transitions read from the style mask,
    /// so externally driven transitions update the reported mode without cancelling a request
    /// whose animation has not begun.
    fn sync_confirmed_fullscreen(&self) {
        // SAFETY: The window is alive on AppKit's main thread and styleMask is a plain getter.
        let confirmed =
            unsafe { usize_value(self.raw.as_ptr(), c"styleMask") } & STYLE_MASK_FULL_SCREEN != 0;
        crate::follow_confirmed_fullscreen(
            &self.fullscreen_confirmed,
            &self.fullscreen_requested,
            confirmed,
        );
    }

    fn apply_pointer_capture(&self) -> Result<(), PlatformError> {
        if self.capture_applied.get() {
            return Ok(());
        }
        // SAFETY: The window and view are alive on AppKit's main thread; the CoreGraphics cursor
        // calls have no preconditions beyond a window-server connection.
        unsafe {
            self.warp_pointer_to_view_center()?;
            if CGAssociateMouseAndMouseCursorPosition(0) != 0 {
                return Err(PlatformError::new(
                    "could not detach the cursor for pointer capture",
                ));
            }
            void(class(c"NSCursor")?, c"hide");
        }
        self.capture_applied.set(true);
        Ok(())
    }

    fn release_pointer_capture(&self) -> Result<(), PlatformError> {
        if !self.capture_applied.replace(false) {
            return Ok(());
        }
        // SAFETY: The calls balance the successful capture that set `capture_applied`.
        unsafe {
            let reassociated = CGAssociateMouseAndMouseCursorPosition(1) == 0;
            void(class(c"NSCursor")?, c"unhide");
            if !reassociated {
                return Err(PlatformError::new(
                    "could not reattach the cursor after pointer capture",
                ));
            }
        }
        Ok(())
    }

    unsafe fn warp_pointer_to_view_center(&self) -> Result<(), PlatformError> {
        // SAFETY (caller): The view and window are alive on AppKit's main thread; converted
        // aggregates follow the SDK ABI and are copied immediately.
        unsafe {
            let bounds = rect_value(self.view.as_ptr(), c"bounds");
            let center = Point {
                x: bounds.origin.x + bounds.size.width / 2.0,
                y: bounds.origin.y + bounds.size.height / 2.0,
            };
            let window_point = point_object(
                self.view.as_ptr(),
                c"convertPoint:toView:",
                center,
                core::ptr::null_mut(),
            );
            let screen_rect = rect_rect(
                self.raw.as_ptr(),
                c"convertRectToScreen:",
                Rect {
                    origin: window_point,
                    size: Size::default(),
                },
            );
            let primary = object(object(class(c"NSScreen")?, c"screens"), c"firstObject");
            if primary.is_null() {
                // Without a display there is no cursor position to pin.
                return Ok(());
            }
            let primary_height = rect_value(primary, c"frame").size.height;
            let warped = CGWarpMouseCursorPosition(Point {
                x: screen_rect.origin.x,
                y: primary_height - screen_rect.origin.y,
            });
            if warped != 0 {
                return Err(PlatformError::new(
                    "could not move the cursor into the captured window",
                ));
            }
            Ok(())
        }
    }

    fn take_focus_transition(&self) -> Option<InputEvent> {
        let focused = self.delegate_state.focused.get();
        if self.last_focused.replace(focused) == focused {
            return None;
        }
        if focused {
            if self.cursor_mode.get() == CursorMode::Captured {
                // Best effort: a refused reapplication leaves the cursor free rather than
                // failing the pump; the mode stays requested for the next transition.
                let _ = self.apply_pointer_capture();
            }
        } else {
            self.captured_pointer_buttons.set(0);
            let _ = self.release_pointer_capture();
        }
        Some(InputEvent::FocusChanged { focused })
    }

    fn translate_input_event(&self, event: Object) -> Option<InputEvent> {
        // SAFETY: The event remains alive in the current AppKit autorelease pool. Every selector is
        // valid for NSEvent, and returned scalar/aggregate values are copied immediately.
        unsafe {
            if object(event, c"window") != self.raw.as_ptr() {
                return None;
            }
            let modifiers = appkit_modifiers(usize_value(event, c"modifierFlags"));
            match usize_value(event, c"type") {
                EVENT_KEY_DOWN => Some(InputEvent::Keyboard {
                    key: appkit_key_code(u16_value(event, c"keyCode")),
                    state: ButtonState::Pressed,
                    repeat: bool_value(event, c"isARepeat"),
                    modifiers,
                }),
                EVENT_KEY_UP => Some(InputEvent::Keyboard {
                    key: appkit_key_code(u16_value(event, c"keyCode")),
                    state: ButtonState::Released,
                    repeat: false,
                    modifiers,
                }),
                EVENT_FLAGS_CHANGED => Some(InputEvent::ModifiersChanged(modifiers)),
                EVENT_MOUSE_MOVED
                | EVENT_LEFT_MOUSE_DRAGGED
                | EVENT_RIGHT_MOUSE_DRAGGED
                | EVENT_OTHER_MOUSE_DRAGGED => {
                    if self.capture_applied.get() {
                        // The pinned cursor makes absolute positions meaningless; AppKit's
                        // event deltas are already top-left oriented.
                        return Some(InputEvent::PointerDelta {
                            delta_x: f64_value(event, c"deltaX"),
                            delta_y: f64_value(event, c"deltaY"),
                            modifiers,
                        });
                    }
                    let (position, inside) = self.pointer_position(event);
                    (inside || self.captured_pointer_buttons.get() != 0).then_some(
                        InputEvent::PointerMoved {
                            position,
                            modifiers,
                        },
                    )
                }
                EVENT_LEFT_MOUSE_DOWN | EVENT_RIGHT_MOUSE_DOWN | EVENT_OTHER_MOUSE_DOWN => {
                    let (position, inside) = self.pointer_position(event);
                    if !inside {
                        return None;
                    }
                    let number = usize_value(event, c"buttonNumber");
                    if number < u32::BITS as usize {
                        self.captured_pointer_buttons
                            .set(self.captured_pointer_buttons.get() | (1_u32 << number));
                    }
                    Some(InputEvent::PointerButton {
                        button: appkit_pointer_button(number),
                        state: ButtonState::Pressed,
                        position,
                        modifiers,
                    })
                }
                EVENT_LEFT_MOUSE_UP | EVENT_RIGHT_MOUSE_UP | EVENT_OTHER_MOUSE_UP => {
                    let (position, inside) = self.pointer_position(event);
                    let number = usize_value(event, c"buttonNumber");
                    let mask = (number < u32::BITS as usize).then(|| 1_u32 << number);
                    let captured =
                        mask.is_some_and(|mask| self.captured_pointer_buttons.get() & mask != 0);
                    if let Some(mask) = mask {
                        self.captured_pointer_buttons
                            .set(self.captured_pointer_buttons.get() & !mask);
                    }
                    (inside || captured).then_some(InputEvent::PointerButton {
                        button: appkit_pointer_button(number),
                        state: ButtonState::Released,
                        position,
                        modifiers,
                    })
                }
                EVENT_SCROLL_WHEEL => {
                    let (position, inside) = self.pointer_position(event);
                    inside.then_some(InputEvent::Scroll {
                        delta: if bool_value(event, c"hasPreciseScrollingDeltas") {
                            ScrollDelta::Precise {
                                x: f64_value(event, c"scrollingDeltaX"),
                                y: f64_value(event, c"scrollingDeltaY"),
                            }
                        } else {
                            ScrollDelta::Coarse {
                                x: f64_value(event, c"scrollingDeltaX"),
                                y: f64_value(event, c"scrollingDeltaY"),
                            }
                        },
                        position,
                        modifiers,
                    })
                }
                _ => None,
            }
        }
    }

    unsafe fn pointer_position(&self, event: Object) -> (LogicalPosition, bool) {
        let window_position = unsafe { point_value(event, c"locationInWindow") };
        let view_position = unsafe {
            point_object(
                self.view.as_ptr(),
                c"convertPoint:fromView:",
                window_position,
                core::ptr::null_mut(),
            )
        };
        let bounds = unsafe { rect_value(self.view.as_ptr(), c"bounds") };
        let x = view_position.x - bounds.origin.x;
        let y = bounds.origin.y + bounds.size.height - view_position.y;
        let inside = x >= 0.0 && y >= 0.0 && x < bounds.size.width && y < bounds.size.height;
        (LogicalPosition::new(x, y), inside)
    }

    fn is_open(&self) -> bool {
        !self.delegate_state.close_requested.get()
    }

    fn current_window_metrics(&self) -> Option<WindowMetrics> {
        // SAFETY: The window and content view are alive on AppKit's main thread. Returned rects and
        // scalar values follow the SDK ABI and are copied immediately.
        unsafe {
            if !bool_value(self.raw.as_ptr(), c"isVisible")
                || bool_value(self.raw.as_ptr(), c"isMiniaturized")
                || usize_value(self.raw.as_ptr(), c"occlusionState") & OCCLUSION_STATE_VISIBLE == 0
            {
                return None;
            }
            let logical = rect_value(self.view.as_ptr(), c"bounds");
            let backing = rect_rect(self.view.as_ptr(), c"convertRectToBacking:", logical);
            if backing.size.width <= 0.0 || backing.size.height <= 0.0 {
                return None;
            }
            let width = physical_dimension(backing.size.width)?;
            let height = physical_dimension(backing.size.height)?;
            let extent = PhysicalExtent::new(width, height);
            if extent.is_empty() {
                return None;
            }
            let scale_factor = f64_value(self.raw.as_ptr(), c"backingScaleFactor");
            let revision = if self.last_extent.get() == PhysicalExtent::default() {
                self.revision.get()
            } else if self.last_extent.get() != extent
                || self.last_scale_factor.get().to_bits() != scale_factor.to_bits()
            {
                let next = self.revision.get().next();
                self.revision.set(next);
                next
            } else {
                self.revision.get()
            };
            self.last_extent.set(extent);
            self.last_scale_factor.set(scale_factor);
            Some(WindowMetrics::new(extent, scale_factor, revision))
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // The system cursor must never stay hidden or detached past the window's life.
        let _ = self.release_pointer_capture();
        // Best-effort pool creation must not guard ownership cleanup: the delegate association
        // borrows close_requested, so leaving it attached while Rust fields drop would be unsound.
        let _pool = AutoreleasePool::new().ok();
        // SAFETY: This value owns the `alloc`/`init` window and delegate retains and drops on the
        // creating main thread because its `Rc` marker prevents transfer. The delegate is detached
        // and released before its borrowed state drops. Closing twice is permitted by AppKit.
        unsafe {
            void_object(self.raw.as_ptr(), c"setDelegate:", core::ptr::null_mut());
            void(self.raw.as_ptr(), c"close");
            void(self.raw.as_ptr(), c"release");
            void(self.delegate.as_ptr(), c"release");
        }
    }
}

/// A borrowed native target whose ownership remains with its [`Window`].
pub struct SurfaceTarget<'window> {
    appkit_view: NonNull<c_void>,
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
                "the initial AppKit extraction supports one live window per application",
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

fn create_window_delegate(
    state: &Rc<WindowDelegateState>,
) -> Result<NonNull<c_void>, PlatformError> {
    let delegate_class = window_delegate_class()?;
    // SAFETY: The dynamically registered class inherits NSObject initialization. The association is
    // non-owning and remains valid because Window retains both the delegate and the Rc allocation.
    unsafe {
        let delegate = required(
            object(object(delegate_class, c"alloc"), c"init"),
            "Mulciber AppKit window delegate",
        )?;
        objc_setAssociatedObject(
            delegate.as_ptr(),
            (&raw const WINDOW_STATE_KEY).cast(),
            Rc::as_ptr(state).cast_mut().cast(),
            ASSOCIATION_ASSIGN,
        );
        Ok(delegate)
    }
}

fn create_content_view(size: Size) -> Result<NonNull<c_void>, PlatformError> {
    let view_class = platform_view_class()?;
    // SAFETY: The class inherits NSView and initWithFrame: takes the SDK Rect aggregate. The caller
    // transfers this owned allocation to NSWindow's retained contentView property.
    unsafe {
        required(
            object_rect_init(
                object(view_class, c"alloc"),
                c"initWithFrame:",
                Rect {
                    origin: Point::default(),
                    size,
                },
            ),
            "Mulciber AppKit content view",
        )
    }
}

fn platform_view_class() -> Result<Object, PlatformError> {
    static CLASS: OnceLock<Result<usize, PlatformError>> = OnceLock::new();
    CLASS
        .get_or_init(|| {
            // SAFETY: Registration runs once. The superclass and selectors have process lifetime,
            // and each method implementation matches the registered Objective-C type encoding.
            unsafe {
                let existing = objc_getClass(c"MulciberPlatformView_v1".as_ptr());
                if !existing.is_null() {
                    return Ok(existing as usize);
                }
                let view = objc_allocateClassPair(
                    class(c"NSView")?,
                    c"MulciberPlatformView_v1".as_ptr(),
                    0,
                );
                if view.is_null() {
                    return Err(PlatformError::new(
                        "could not allocate the AppKit content view class",
                    ));
                }
                let accepts_first_responder_added = class_addMethod(
                    view,
                    selector(c"acceptsFirstResponder"),
                    accepts_first_responder as *const c_void,
                    c"B@:".as_ptr(),
                );
                let key_down_added = class_addMethod(
                    view,
                    selector(c"keyDown:"),
                    consume_key_event as *const c_void,
                    c"v@:@".as_ptr(),
                );
                let key_up_added = class_addMethod(
                    view,
                    selector(c"keyUp:"),
                    consume_key_event as *const c_void,
                    c"v@:@".as_ptr(),
                );
                let flags_changed_added = class_addMethod(
                    view,
                    selector(c"flagsChanged:"),
                    consume_key_event as *const c_void,
                    c"v@:@".as_ptr(),
                );
                if !accepts_first_responder_added
                    || !key_down_added
                    || !key_up_added
                    || !flags_changed_added
                {
                    objc_disposeClassPair(view);
                    return Err(PlatformError::new(
                        "could not install AppKit content view input methods",
                    ));
                }
                objc_registerClassPair(view);
                Ok(view as usize)
            }
        })
        .clone()
        .map(|view| view as Object)
}

unsafe extern "C" fn accepts_first_responder(_view: Object, _selector: Selector) -> bool {
    true
}

unsafe extern "C" fn consume_key_event(_view: Object, _selector: Selector, _event: Object) {}

fn window_delegate_class() -> Result<Object, PlatformError> {
    static CLASS: OnceLock<Result<usize, PlatformError>> = OnceLock::new();
    CLASS
        .get_or_init(|| {
            // SAFETY: Registration runs once. The superclass and selectors have process lifetime,
            // and each method implementation matches the registered Objective-C type encoding.
            unsafe {
                let existing = objc_getClass(c"MulciberPlatformWindowDelegate_v1".as_ptr());
                if !existing.is_null() {
                    return Ok(existing as usize);
                }
                let delegate = objc_allocateClassPair(
                    class(c"NSObject")?,
                    c"MulciberPlatformWindowDelegate_v1".as_ptr(),
                    0,
                );
                if delegate.is_null() {
                    return Err(PlatformError::new(
                        "could not allocate the AppKit window delegate class",
                    ));
                }
                let should_close_added = class_addMethod(
                    delegate,
                    selector(c"windowShouldClose:"),
                    window_should_close as *const c_void,
                    c"B@:@".as_ptr(),
                );
                let will_close_added = class_addMethod(
                    delegate,
                    selector(c"windowWillClose:"),
                    window_will_close as *const c_void,
                    c"v@:@".as_ptr(),
                );
                let did_become_key_added = class_addMethod(
                    delegate,
                    selector(c"windowDidBecomeKey:"),
                    window_did_become_key as *const c_void,
                    c"v@:@".as_ptr(),
                );
                let did_resign_key_added = class_addMethod(
                    delegate,
                    selector(c"windowDidResignKey:"),
                    window_did_resign_key as *const c_void,
                    c"v@:@".as_ptr(),
                );
                if !should_close_added
                    || !will_close_added
                    || !did_become_key_added
                    || !did_resign_key_added
                {
                    objc_disposeClassPair(delegate);
                    return Err(PlatformError::new(
                        "could not install AppKit window delegate methods",
                    ));
                }
                objc_registerClassPair(delegate);
                Ok(delegate as usize)
            }
        })
        .clone()
        .map(|class| class as Object)
}

unsafe extern "C" fn window_should_close(
    delegate: Object,
    _selector: Selector,
    _window: Object,
) -> bool {
    unsafe { mark_close_requested(delegate) };
    true
}

unsafe extern "C" fn window_will_close(
    delegate: Object,
    _selector: Selector,
    _notification: Object,
) {
    unsafe { mark_close_requested(delegate) };
}

unsafe extern "C" fn window_did_become_key(
    delegate: Object,
    _selector: Selector,
    _notification: Object,
) {
    if let Some(state) = unsafe { delegate_state(delegate) } {
        // SAFETY: Window keeps the associated state alive until after detaching this delegate.
        unsafe { state.as_ref() }.focused.set(true);
    }
}

unsafe extern "C" fn window_did_resign_key(
    delegate: Object,
    _selector: Selector,
    _notification: Object,
) {
    if let Some(state) = unsafe { delegate_state(delegate) } {
        // SAFETY: Window keeps the associated state alive until after detaching this delegate.
        unsafe { state.as_ref() }.focused.set(false);
    }
}

unsafe fn mark_close_requested(delegate: Object) {
    if let Some(state) = unsafe { delegate_state(delegate) } {
        // SAFETY: Window keeps the associated state alive until after detaching this delegate.
        unsafe { state.as_ref() }.close_requested.set(true);
    }
}

unsafe fn delegate_state(delegate: Object) -> Option<NonNull<WindowDelegateState>> {
    let state = unsafe {
        objc_getAssociatedObject(delegate, (&raw const WINDOW_STATE_KEY).cast())
            .cast::<WindowDelegateState>()
    };
    NonNull::new(state)
}

/// Internal bridge used by Mulciber's Metal backend.
///
/// This module is public only because the platform and graphics layers are separate crates. Games
/// should pass [`SurfaceTarget`] to `mulciber` rather than inspect native pointers.
#[doc(hidden)]
pub mod integration {
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use super::SurfaceTarget;

    /// Extracts the borrowed `AppKit` view for the Metal backend.
    ///
    /// # Safety
    ///
    /// The pointer must not be retained beyond the target's lifetime, messaged from another thread,
    /// released, or used to replace ownership established by `mulciber-platform`.
    #[must_use]
    pub unsafe fn appkit_view(target: &SurfaceTarget<'_>) -> NonNull<c_void> {
        target.appkit_view
    }
}

struct AutoreleasePool(NonNull<c_void>);

impl AutoreleasePool {
    fn new() -> Result<Self, PlatformError> {
        // SAFETY: `NSAutoreleasePool::new` returns one owned pool for the current thread.
        unsafe {
            required(
                object(class(c"NSAutoreleasePool")?, c"new"),
                "NSAutoreleasePool",
            )
            .map(Self)
        }
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        // SAFETY: The pool is owned and drained exactly once on its creating thread.
        unsafe { void(self.0.as_ptr(), c"drain") };
    }
}

fn class(name: &CStr) -> Result<Object, PlatformError> {
    // SAFETY: The name is NUL-terminated and Objective-C class objects have process lifetime.
    let value = unsafe { objc_getClass(name.as_ptr()) };
    if value.is_null() {
        Err(PlatformError::with_kind(
            PlatformErrorKind::Unsupported,
            format!("missing Objective-C class {name:?}"),
        ))
    } else {
        Ok(value)
    }
}

fn selector(name: &CStr) -> Selector {
    // SAFETY: The name is NUL-terminated and selectors are interned for process lifetime.
    unsafe { sel_registerName(name.as_ptr()) }
}

unsafe fn object(receiver: Object, name: &CStr) -> Object {
    let function: unsafe extern "C" fn(Object, Selector) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

unsafe fn object_window_init(
    receiver: Object,
    name: &CStr,
    rect: Rect,
    style: usize,
    backing: usize,
    deferred: bool,
) -> Object {
    let function: unsafe extern "C" fn(Object, Selector, Rect, usize, usize, bool) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), rect, style, backing, deferred) }
}

unsafe fn object_event(
    receiver: Object,
    name: &CStr,
    mask: usize,
    expiration: Object,
    mode: Object,
    dequeue: bool,
) -> Object {
    let function: unsafe extern "C" fn(Object, Selector, usize, Object, Object, bool) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), mask, expiration, mode, dequeue) }
}

unsafe fn object_bytes_length_usize(
    receiver: Object,
    name: &CStr,
    bytes: *const c_void,
    length: usize,
    value: usize,
) -> Object {
    let function: unsafe extern "C" fn(Object, Selector, *const c_void, usize, usize) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), bytes, length, value) }
}

unsafe fn bool_value(receiver: Object, name: &CStr) -> bool {
    let function: unsafe extern "C" fn(Object, Selector) -> bool =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

unsafe fn bool_isize(receiver: Object, name: &CStr, value: isize) -> bool {
    let function: unsafe extern "C" fn(Object, Selector, isize) -> bool =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), value) }
}

unsafe fn bool_object(receiver: Object, name: &CStr, value: Object) -> bool {
    let function: unsafe extern "C" fn(Object, Selector, Object) -> bool =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), value) }
}

unsafe fn object_rect_init(receiver: Object, name: &CStr, rect: Rect) -> Object {
    let function: unsafe extern "C" fn(Object, Selector, Rect) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), rect) }
}

unsafe fn usize_value(receiver: Object, name: &CStr) -> usize {
    let function: unsafe extern "C" fn(Object, Selector) -> usize =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

unsafe fn u16_value(receiver: Object, name: &CStr) -> u16 {
    let function: unsafe extern "C" fn(Object, Selector) -> u16 =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

unsafe fn f64_value(receiver: Object, name: &CStr) -> f64 {
    let function: unsafe extern "C" fn(Object, Selector) -> f64 =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

unsafe fn rect_value(receiver: Object, name: &CStr) -> Rect {
    let function: unsafe extern "C" fn(Object, Selector) -> Rect =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

unsafe fn point_value(receiver: Object, name: &CStr) -> Point {
    let function: unsafe extern "C" fn(Object, Selector) -> Point =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

unsafe fn point_object(receiver: Object, name: &CStr, value: Point, object: Object) -> Point {
    let function: unsafe extern "C" fn(Object, Selector, Point, Object) -> Point =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), value, object) }
}

unsafe fn rect_rect(receiver: Object, name: &CStr, value: Rect) -> Rect {
    let function: unsafe extern "C" fn(Object, Selector, Rect) -> Rect =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), value) }
}

unsafe fn void(receiver: Object, name: &CStr) {
    let function: unsafe extern "C" fn(Object, Selector) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) };
}

unsafe fn void_object(receiver: Object, name: &CStr, value: Object) {
    let function: unsafe extern "C" fn(Object, Selector, Object) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), value) };
}

unsafe fn void_usize(receiver: Object, name: &CStr, value: usize) {
    let function: unsafe extern "C" fn(Object, Selector, usize) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), value) };
}

unsafe fn void_bool(receiver: Object, name: &CStr, value: bool) {
    let function: unsafe extern "C" fn(Object, Selector, bool) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), value) };
}

unsafe fn owned_string(value: &str) -> Result<NonNull<c_void>, PlatformError> {
    let allocated = unsafe { object(class(c"NSString")?, c"alloc") };
    let string = unsafe {
        object_bytes_length_usize(
            allocated,
            c"initWithBytes:length:encoding:",
            value.as_ptr().cast(),
            value.len(),
            UTF8_STRING_ENCODING,
        )
    };
    required(string, "window title NSString")
}

fn required(value: Object, label: &str) -> Result<NonNull<c_void>, PlatformError> {
    NonNull::new(value).ok_or_else(|| PlatformError::new(format!("{label} is unavailable")))
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
        (None, Some(info)) => Some(WindowEvent::RenderingResumed(info)),
        _ => None,
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn physical_dimension(value: f64) -> Option<u32> {
    let rounded = value.round();
    if !rounded.is_finite() || rounded <= 0.0 || rounded > f64::from(u32::MAX) {
        return None;
    }
    // The finite, positive, in-range checks above make this conversion exact for AppKit's integral
    // backing-pixel dimensions.
    Some(rounded as u32)
}

fn appkit_modifiers(flags: usize) -> Modifiers {
    let mut bits = 0;
    if flags & MODIFIER_SHIFT != 0 {
        bits |= Modifiers::SHIFT;
    }
    if flags & MODIFIER_CONTROL != 0 {
        bits |= Modifiers::CONTROL;
    }
    if flags & MODIFIER_OPTION != 0 {
        bits |= Modifiers::ALT;
    }
    if flags & MODIFIER_COMMAND != 0 {
        bits |= Modifiers::SUPER;
    }
    if flags & MODIFIER_CAPS_LOCK != 0 {
        bits |= Modifiers::CAPS_LOCK;
    }
    if flags & MODIFIER_FUNCTION != 0 {
        bits |= Modifiers::FUNCTION;
    }
    Modifiers::from_bits(bits)
}

#[allow(clippy::too_many_lines)]
fn appkit_key_code(code: u16) -> KeyCode {
    match code {
        0 => KeyCode::KeyA,
        1 => KeyCode::KeyS,
        2 => KeyCode::KeyD,
        3 => KeyCode::KeyF,
        4 => KeyCode::KeyH,
        5 => KeyCode::KeyG,
        6 => KeyCode::KeyZ,
        7 => KeyCode::KeyX,
        8 => KeyCode::KeyC,
        9 => KeyCode::KeyV,
        11 => KeyCode::KeyB,
        12 => KeyCode::KeyQ,
        13 => KeyCode::KeyW,
        14 => KeyCode::KeyE,
        15 => KeyCode::KeyR,
        16 => KeyCode::KeyY,
        17 => KeyCode::KeyT,
        18 => KeyCode::Digit1,
        19 => KeyCode::Digit2,
        20 => KeyCode::Digit3,
        21 => KeyCode::Digit4,
        22 => KeyCode::Digit6,
        23 => KeyCode::Digit5,
        24 => KeyCode::Equal,
        25 => KeyCode::Digit9,
        26 => KeyCode::Digit7,
        27 => KeyCode::Minus,
        28 => KeyCode::Digit8,
        29 => KeyCode::Digit0,
        30 => KeyCode::BracketRight,
        31 => KeyCode::KeyO,
        32 => KeyCode::KeyU,
        33 => KeyCode::BracketLeft,
        34 => KeyCode::KeyI,
        35 => KeyCode::KeyP,
        36 => KeyCode::Enter,
        37 => KeyCode::KeyL,
        38 => KeyCode::KeyJ,
        39 => KeyCode::Quote,
        40 => KeyCode::KeyK,
        41 => KeyCode::Semicolon,
        42 => KeyCode::Backslash,
        43 => KeyCode::Comma,
        44 => KeyCode::Slash,
        45 => KeyCode::KeyN,
        46 => KeyCode::KeyM,
        47 => KeyCode::Period,
        48 => KeyCode::Tab,
        49 => KeyCode::Space,
        50 => KeyCode::Backquote,
        51 => KeyCode::Backspace,
        53 => KeyCode::Escape,
        64 => KeyCode::F17,
        65 => KeyCode::NumpadDecimal,
        67 => KeyCode::NumpadMultiply,
        69 => KeyCode::NumpadAdd,
        71 => KeyCode::NumpadClear,
        75 => KeyCode::NumpadDivide,
        76 => KeyCode::NumpadEnter,
        78 => KeyCode::NumpadSubtract,
        79 => KeyCode::F18,
        80 => KeyCode::F19,
        81 => KeyCode::NumpadEqual,
        82 => KeyCode::Numpad0,
        83 => KeyCode::Numpad1,
        84 => KeyCode::Numpad2,
        85 => KeyCode::Numpad3,
        86 => KeyCode::Numpad4,
        87 => KeyCode::Numpad5,
        88 => KeyCode::Numpad6,
        89 => KeyCode::Numpad7,
        90 => KeyCode::F20,
        91 => KeyCode::Numpad8,
        92 => KeyCode::Numpad9,
        96 => KeyCode::F5,
        97 => KeyCode::F6,
        98 => KeyCode::F7,
        99 => KeyCode::F3,
        100 => KeyCode::F8,
        101 => KeyCode::F9,
        103 => KeyCode::F11,
        105 => KeyCode::F13,
        106 => KeyCode::F16,
        107 => KeyCode::F14,
        109 => KeyCode::F10,
        111 => KeyCode::F12,
        113 => KeyCode::F15,
        114 => KeyCode::Insert,
        115 => KeyCode::Home,
        116 => KeyCode::PageUp,
        117 => KeyCode::Delete,
        118 => KeyCode::F4,
        119 => KeyCode::End,
        120 => KeyCode::F2,
        121 => KeyCode::PageDown,
        122 => KeyCode::F1,
        123 => KeyCode::ArrowLeft,
        124 => KeyCode::ArrowRight,
        125 => KeyCode::ArrowDown,
        126 => KeyCode::ArrowUp,
        _ => KeyCode::Unidentified(u32::from(code)),
    }
}

fn appkit_pointer_button(number: usize) -> PointerButton {
    match number {
        0 => PointerButton::Primary,
        1 => PointerButton::Secondary,
        2 => PointerButton::Middle,
        _ => PointerButton::Other(u16::try_from(number).unwrap_or(u16::MAX)),
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use crate::{
        KeyCode, PhysicalExtent, PointerButton, WindowEvent, WindowMetrics, WindowRevision,
    };

    use super::{
        MODIFIER_COMMAND, MODIFIER_CONTROL, MODIFIER_OPTION, MODIFIER_SHIFT, Size,
        WindowDelegateState, WindowSlot, appkit_key_code, appkit_modifiers, appkit_pointer_button,
        bool_object, bool_value, create_content_view, create_window_delegate, metrics_transition,
        physical_dimension, void, void_object,
    };

    fn metrics(revision: WindowRevision) -> WindowMetrics {
        WindowMetrics::new(PhysicalExtent::new(1920, 1080), 2.0, revision)
    }

    #[test]
    fn lifecycle_transition_reports_metric_revision_changes() {
        let first = metrics(WindowRevision::INITIAL);
        let resized = metrics(WindowRevision::INITIAL.next());
        assert_eq!(
            metrics_transition(Some(first), Some(resized)),
            Some(WindowEvent::MetricsChanged(resized))
        );
    }

    #[test]
    fn lifecycle_transition_reports_suspend_and_resume() {
        let info = metrics(WindowRevision::INITIAL);
        assert_eq!(
            metrics_transition(Some(info), None),
            Some(WindowEvent::RenderingSuspended)
        );
        assert_eq!(
            metrics_transition(None, Some(info)),
            Some(WindowEvent::RenderingResumed(info))
        );
    }

    #[test]
    fn physical_dimensions_are_positive_finite_and_rounded() {
        assert_eq!(physical_dimension(1279.6), Some(1280));
        assert_eq!(physical_dimension(0.0), None);
        assert_eq!(physical_dimension(f64::NAN), None);
        assert_eq!(physical_dimension(f64::INFINITY), None);
    }

    #[test]
    fn window_slot_allows_one_live_window_and_releases_on_drop() {
        let slot = WindowSlot::new();
        let lease = slot.claim().expect("first window should claim the slot");
        assert!(slot.claim().is_err());
        drop(lease);
        assert!(slot.claim().is_ok());
    }

    #[test]
    fn window_delegate_records_a_close_request() {
        let state = Rc::new(WindowDelegateState {
            close_requested: false.into(),
            focused: false.into(),
        });
        let delegate = create_window_delegate(&state).expect("delegate creation should succeed");

        // SAFETY: The dynamically registered method accepts an unused object argument. The test then
        // balances the delegate's owned allocation.
        unsafe {
            assert!(bool_object(
                delegate.as_ptr(),
                c"windowShouldClose:",
                core::ptr::null_mut(),
            ));
            void(delegate.as_ptr(), c"release");
        }

        assert!(state.close_requested.get());
    }

    #[test]
    fn window_delegate_records_focus_transitions() {
        let state = Rc::new(WindowDelegateState {
            close_requested: false.into(),
            focused: false.into(),
        });
        let delegate = create_window_delegate(&state).expect("delegate creation should succeed");

        // SAFETY: Both dynamically registered methods accept an unused notification object. The
        // test balances the delegate's owned allocation after exercising both callbacks.
        unsafe {
            void_object(
                delegate.as_ptr(),
                c"windowDidBecomeKey:",
                core::ptr::null_mut(),
            );
            assert!(state.focused.get());
            void_object(
                delegate.as_ptr(),
                c"windowDidResignKey:",
                core::ptr::null_mut(),
            );
            assert!(!state.focused.get());
            void(delegate.as_ptr(), c"release");
        }
    }

    #[test]
    fn platform_content_view_accepts_first_responder() {
        let view = create_content_view(Size {
            width: 640.0,
            height: 480.0,
        })
        .expect("content view creation should succeed");
        // SAFETY: The test owns the initialized NSView and balances it after calling a registered
        // no-argument method.
        unsafe {
            assert!(bool_value(view.as_ptr(), c"acceptsFirstResponder"));
            void(view.as_ptr(), c"release");
        }
    }

    #[test]
    fn appkit_key_codes_map_by_physical_position() {
        assert_eq!(appkit_key_code(0), KeyCode::KeyA);
        assert_eq!(appkit_key_code(13), KeyCode::KeyW);
        assert_eq!(appkit_key_code(123), KeyCode::ArrowLeft);
        assert_eq!(appkit_key_code(999), KeyCode::Unidentified(999));
    }

    #[test]
    fn appkit_modifiers_preserve_gameplay_flags() {
        let modifiers = appkit_modifiers(
            MODIFIER_SHIFT | MODIFIER_CONTROL | MODIFIER_OPTION | MODIFIER_COMMAND,
        );
        assert!(modifiers.shift());
        assert!(modifiers.control());
        assert!(modifiers.alt());
        assert!(modifiers.super_key());
    }

    #[test]
    fn appkit_pointer_buttons_preserve_extra_button_numbers() {
        assert_eq!(appkit_pointer_button(0), PointerButton::Primary);
        assert_eq!(appkit_pointer_button(1), PointerButton::Secondary);
        assert_eq!(appkit_pointer_button(2), PointerButton::Middle);
        assert_eq!(appkit_pointer_button(7), PointerButton::Other(7));
    }
}
