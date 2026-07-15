use std::ffi::{CStr, c_char, c_void};
use std::mem;

pub type Object = *mut c_void;
type Selector = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClearColor {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Object;
    fn objc_msgSend();
    fn sel_registerName(name: *const c_char) -> Selector;
}

pub fn class(name: &CStr) -> Object {
    // SAFETY: The name is NUL-terminated. Objective-C class objects have process lifetime.
    let value = unsafe { objc_getClass(name.as_ptr()) };
    assert!(!value.is_null(), "missing Objective-C class: {name:?}");
    value
}

fn selector(name: &CStr) -> Selector {
    // SAFETY: The name is NUL-terminated and selectors are interned for process lifetime.
    unsafe { sel_registerName(name.as_ptr()) }
}

pub unsafe fn object(receiver: Object, name: &CStr) -> Object {
    let function: unsafe extern "C" fn(Object, Selector) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

pub unsafe fn object_object(receiver: Object, name: &CStr, argument: Object) -> Object {
    let function: unsafe extern "C" fn(Object, Selector, Object) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) }
}

pub unsafe fn object_c_string(receiver: Object, name: &CStr, argument: *const c_char) -> Object {
    let function: unsafe extern "C" fn(Object, Selector, *const c_char) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) }
}

pub unsafe fn object_usize(receiver: Object, name: &CStr, argument: usize) -> Object {
    let function: unsafe extern "C" fn(Object, Selector, usize) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) }
}

pub unsafe fn object_window_init(
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

pub unsafe fn object_bytes(
    receiver: Object,
    name: &CStr,
    bytes: *const c_void,
    length: usize,
    options: usize,
) -> Object {
    let function: unsafe extern "C" fn(Object, Selector, *const c_void, usize, usize) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), bytes, length, options) }
}

pub unsafe fn object_two_objects_out(
    receiver: Object,
    name: &CStr,
    first: Object,
    second: Object,
    output: *mut Object,
) -> Object {
    let function: unsafe extern "C" fn(Object, Selector, Object, Object, *mut Object) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), first, second, output) }
}

pub unsafe fn object_object_out(
    receiver: Object,
    name: &CStr,
    argument: Object,
    output: *mut Object,
) -> Object {
    let function: unsafe extern "C" fn(Object, Selector, Object, *mut Object) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument, output) }
}

pub unsafe fn object_event(
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

pub unsafe fn bool_value(receiver: Object, name: &CStr) -> bool {
    let function: unsafe extern "C" fn(Object, Selector) -> bool =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

pub unsafe fn bool_isize(receiver: Object, name: &CStr, argument: isize) -> bool {
    let function: unsafe extern "C" fn(Object, Selector, isize) -> bool =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) }
}

pub unsafe fn usize_value(receiver: Object, name: &CStr) -> usize {
    let function: unsafe extern "C" fn(Object, Selector) -> usize =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

pub unsafe fn f64_value(receiver: Object, name: &CStr) -> f64 {
    let function: unsafe extern "C" fn(Object, Selector) -> f64 =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

pub unsafe fn rect_value(receiver: Object, name: &CStr) -> Rect {
    let function: unsafe extern "C" fn(Object, Selector) -> Rect =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

pub unsafe fn rect_rect(receiver: Object, name: &CStr, argument: Rect) -> Rect {
    let function: unsafe extern "C" fn(Object, Selector, Rect) -> Rect =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) }
}

pub unsafe fn void(receiver: Object, name: &CStr) {
    let function: unsafe extern "C" fn(Object, Selector) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) };
}

pub unsafe fn void_object(receiver: Object, name: &CStr, argument: Object) {
    let function: unsafe extern "C" fn(Object, Selector, Object) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) };
}

pub unsafe fn void_bool(receiver: Object, name: &CStr, argument: bool) {
    let function: unsafe extern "C" fn(Object, Selector, bool) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) };
}

pub unsafe fn void_usize(receiver: Object, name: &CStr, argument: usize) {
    let function: unsafe extern "C" fn(Object, Selector, usize) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) };
}

pub unsafe fn void_f64(receiver: Object, name: &CStr, argument: f64) {
    let function: unsafe extern "C" fn(Object, Selector, f64) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) };
}

pub unsafe fn void_size(receiver: Object, name: &CStr, argument: Size) {
    let function: unsafe extern "C" fn(Object, Selector, Size) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) };
}

pub unsafe fn void_clear_color(receiver: Object, name: &CStr, argument: ClearColor) {
    let function: unsafe extern "C" fn(Object, Selector, ClearColor) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) };
}

pub unsafe fn void_object_two_usizes(
    receiver: Object,
    name: &CStr,
    object: Object,
    first: usize,
    second: usize,
) {
    let function: unsafe extern "C" fn(Object, Selector, Object, usize, usize) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), object, first, second) };
}

pub unsafe fn void_three_usizes(
    receiver: Object,
    name: &CStr,
    first: usize,
    second: usize,
    third: usize,
) {
    let function: unsafe extern "C" fn(Object, Selector, usize, usize, usize) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), first, second, third) };
}

pub fn ns_string(value: &CStr) -> Object {
    // SAFETY: NSString copies the NUL-terminated bytes into an autoreleased immutable string.
    unsafe { object_c_string(class(c"NSString"), c"stringWithUTF8String:", value.as_ptr()) }
}

pub fn ns_string_bytes(value: &[u8]) -> Object {
    assert_eq!(value.last(), Some(&0));
    // SAFETY: The assertion establishes a NUL-terminated byte string for NSString.
    unsafe {
        object_c_string(
            class(c"NSString"),
            c"stringWithUTF8String:",
            value.as_ptr().cast(),
        )
    }
}

pub fn description(value: Object) -> String {
    if value.is_null() {
        return "unknown Objective-C error".to_owned();
    }
    // SAFETY: NSError and NSObject expose description as NSString; UTF8String follows NSString ABI.
    let string = unsafe { object(value, c"localizedDescription") };
    // SAFETY: The returned pointer is valid while the autorelease pool and NSString are alive.
    let bytes = unsafe {
        let function: unsafe extern "C" fn(Object, Selector) -> *const c_char =
            mem::transmute(objc_msgSend as *const ());
        function(string, selector(c"UTF8String"))
    };
    if bytes.is_null() {
        return "unknown Objective-C error".to_owned();
    }
    // SAFETY: NSString guarantees a NUL-terminated UTF-8 representation.
    unsafe { CStr::from_ptr(bytes) }
        .to_string_lossy()
        .into_owned()
}

pub struct AutoreleasePool(Object);

impl AutoreleasePool {
    pub fn new() -> Self {
        // SAFETY: NSAutoreleasePool's class `new` method returns an owned pool object.
        Self(unsafe { object(class(c"NSAutoreleasePool"), c"new") })
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        // SAFETY: The pool is owned by this value and is drained exactly once on its creating thread.
        unsafe { void(self.0, c"drain") };
    }
}
