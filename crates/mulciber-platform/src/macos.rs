use std::cell::Cell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::marker::PhantomData;
use std::mem;
use std::ptr::NonNull;
use std::rc::Rc;

use crate::{
    PhysicalExtent, PlatformError, PumpStatus, WindowDescriptor, WindowEvent, WindowMetrics,
    WindowRevision,
};

type Object = *mut c_void;
type Selector = *mut c_void;

const OCCLUSION_STATE_VISIBLE: usize = 1 << 1;
const UTF8_STRING_ENCODING: usize = 4;

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
    fn objc_getClass(name: *const c_char) -> Object;
    fn objc_msgSend();
    fn sel_registerName(name: *const c_char) -> Selector;
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    static NSDefaultRunLoopMode: Object;
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
            return Err(PlatformError::new(
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
            return Err(PlatformError::new(
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

            let view = required(object(window.as_ptr(), c"contentView"), "NSView")?;
            void_bool(view.as_ptr(), c"setWantsLayer:", true);

            void(window.as_ptr(), c"center");
            void_object(
                window.as_ptr(),
                c"makeKeyAndOrderFront:",
                core::ptr::null_mut(),
            );
            void(self.raw.as_ptr(), c"activate");

            let result = Window {
                raw: window,
                view,
                revision: Cell::new(WindowRevision::INITIAL),
                last_extent: Cell::new(PhysicalExtent::default()),
                last_scale_factor: Cell::new(0.0),
                last_metrics: Cell::new(None),
                close_reported: Cell::new(false),
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
    /// # Errors
    ///
    /// Returns an error when a required `AppKit` event object or autorelease pool is unavailable.
    pub fn pump_events(
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
                void_object(self.raw.as_ptr(), c"sendEvent:", event);
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
    close_reported: Cell<bool>,
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

    fn is_open(&self) -> bool {
        // SAFETY: The window is alive and queried on its creating main thread.
        unsafe {
            bool_value(self.raw.as_ptr(), c"isVisible")
                || bool_value(self.raw.as_ptr(), c"isMiniaturized")
        }
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
        if let Ok(_pool) = AutoreleasePool::new() {
            // SAFETY: This value owns the `alloc`/`init` window retain and drops on the creating main
            // thread because its `Rc` marker prevents transfer. Closing twice is permitted by AppKit.
            unsafe {
                void(self.raw.as_ptr(), c"close");
                void(self.raw.as_ptr(), c"release");
            }
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
        Err(PlatformError::new(format!(
            "missing Objective-C class {name:?}"
        )))
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

unsafe fn usize_value(receiver: Object, name: &CStr) -> usize {
    let function: unsafe extern "C" fn(Object, Selector) -> usize =
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

#[cfg(test)]
mod tests {
    use crate::{PhysicalExtent, WindowEvent, WindowMetrics, WindowRevision};

    use super::{WindowSlot, metrics_transition, physical_dimension};

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
}
