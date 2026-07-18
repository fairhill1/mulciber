#![allow(
    clippy::missing_panics_doc,
    clippy::missing_safety_doc,
    clippy::must_use_candidate,
    clippy::new_without_default,
    clippy::not_unsafe_ptr_arg_deref
)]

use std::borrow::ToOwned;
use std::ffi::{CStr, c_char, c_void};
use std::mem;
use std::string::String;

pub type Object = *mut c_void;
type Selector = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClearColor {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Origin3 {
    pub x: usize,
    pub y: usize,
    pub z: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Region3 {
    pub origin: Origin3,
    pub size: Size3,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size3 {
    pub width: usize,
    pub height: usize,
    pub depth: usize,
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

pub unsafe fn object_two_usizes(
    receiver: Object,
    name: &CStr,
    first: usize,
    second: usize,
) -> Object {
    let function: unsafe extern "C" fn(Object, Selector, usize, usize) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), first, second) }
}

pub unsafe fn object_three_usizes_bool(
    receiver: Object,
    name: &CStr,
    first: usize,
    second: usize,
    third: usize,
    fourth: bool,
) -> Object {
    let function: unsafe extern "C" fn(Object, Selector, usize, usize, usize, bool) -> Object =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), first, second, third, fourth) }
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

pub unsafe fn object_object_usize_two_out(
    receiver: Object,
    name: &CStr,
    argument: Object,
    options: usize,
    first_output: *mut Object,
    second_output: *mut Object,
) -> Object {
    let function: unsafe extern "C" fn(
        Object,
        Selector,
        Object,
        usize,
        *mut Object,
        *mut Object,
    ) -> Object = unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe {
        function(
            receiver,
            selector(name),
            argument,
            options,
            first_output,
            second_output,
        )
    }
}

pub unsafe fn bool_value(receiver: Object, name: &CStr) -> bool {
    let function: unsafe extern "C" fn(Object, Selector) -> bool =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

pub unsafe fn bool_usize(receiver: Object, name: &CStr, argument: usize) -> bool {
    let function: unsafe extern "C" fn(Object, Selector, usize) -> bool =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) }
}

pub unsafe fn bool_object_out(
    receiver: Object,
    name: &CStr,
    argument: Object,
    output: *mut Object,
) -> bool {
    let function: unsafe extern "C" fn(Object, Selector, Object, *mut Object) -> bool =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument, output) }
}

pub unsafe fn usize_value(receiver: Object, name: &CStr) -> usize {
    let function: unsafe extern "C" fn(Object, Selector) -> usize =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

pub unsafe fn pointer_value(receiver: Object, name: &CStr) -> *mut c_void {
    let function: unsafe extern "C" fn(Object, Selector) -> *mut c_void =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
}

pub unsafe fn f64_value(receiver: Object, name: &CStr) -> f64 {
    let function: unsafe extern "C" fn(Object, Selector) -> f64 =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name)) }
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

pub unsafe fn void_object_usize(receiver: Object, name: &CStr, object: Object, index: usize) {
    let function: unsafe extern "C" fn(Object, Selector, Object, usize) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), object, index) };
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

pub unsafe fn void_two_sizes(receiver: Object, name: &CStr, first: Size3, second: Size3) {
    let function: unsafe extern "C" fn(Object, Selector, Size3, Size3) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), first, second) };
}

pub unsafe fn void_clear_color(receiver: Object, name: &CStr, argument: ClearColor) {
    let function: unsafe extern "C" fn(Object, Selector, ClearColor) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector(name), argument) };
}

pub unsafe fn void_region_usize_bytes_usize(
    receiver: Object,
    name: &CStr,
    region: Region3,
    level: usize,
    bytes: *const c_void,
    bytes_per_row: usize,
) {
    let function: unsafe extern "C" fn(Object, Selector, Region3, usize, *const c_void, usize) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe {
        function(
            receiver,
            selector(name),
            region,
            level,
            bytes,
            bytes_per_row,
        );
    }
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

#[allow(clippy::too_many_arguments)]
pub unsafe fn void_three_usizes_object_two_usizes(
    receiver: Object,
    name: &CStr,
    first: usize,
    second: usize,
    third: usize,
    object: Object,
    fourth: usize,
    fifth: usize,
) {
    let function: unsafe extern "C" fn(
        Object,
        Selector,
        usize,
        usize,
        usize,
        Object,
        usize,
        usize,
    ) = unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe {
        function(
            receiver,
            selector(name),
            first,
            second,
            third,
            object,
            fourth,
            fifth,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn void_two_usizes_object_usize_object_usize(
    receiver: Object,
    name: &CStr,
    first: usize,
    second: usize,
    first_object: Object,
    third: usize,
    second_object: Object,
    fourth: usize,
) {
    let function: unsafe extern "C" fn(
        Object,
        Selector,
        usize,
        usize,
        Object,
        usize,
        Object,
        usize,
    ) = unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe {
        function(
            receiver,
            selector(name),
            first,
            second,
            first_object,
            third,
            second_object,
            fourth,
        );
    };
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn void_copy_buffer_to_texture(
    receiver: Object,
    name: &CStr,
    source: Object,
    source_offset: usize,
    source_bytes_per_row: usize,
    source_bytes_per_image: usize,
    source_size: Size3,
    destination: Object,
    destination_slice: usize,
    destination_level: usize,
    destination_origin: Origin3,
) {
    let function: unsafe extern "C" fn(
        Object,
        Selector,
        Object,
        usize,
        usize,
        usize,
        Size3,
        Object,
        usize,
        usize,
        Origin3,
    ) = unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe {
        function(
            receiver,
            selector(name),
            source,
            source_offset,
            source_bytes_per_row,
            source_bytes_per_image,
            source_size,
            destination,
            destination_slice,
            destination_level,
            destination_origin,
        );
    };
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn void_copy_texture_to_buffer(
    receiver: Object,
    name: &CStr,
    source: Object,
    source_slice: usize,
    source_level: usize,
    source_origin: Origin3,
    source_size: Size3,
    destination: Object,
    destination_offset: usize,
    destination_bytes_per_row: usize,
    destination_bytes_per_image: usize,
) {
    let function: unsafe extern "C" fn(
        Object,
        Selector,
        Object,
        usize,
        usize,
        Origin3,
        Size3,
        Object,
        usize,
        usize,
        usize,
    ) = unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe {
        function(
            receiver,
            selector(name),
            source,
            source_slice,
            source_level,
            source_origin,
            source_size,
            destination,
            destination_offset,
            destination_bytes_per_row,
            destination_bytes_per_image,
        );
    };
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn void_copy_buffer(
    receiver: Object,
    name: &CStr,
    source: Object,
    source_offset: usize,
    destination: Object,
    destination_offset: usize,
    size: usize,
) {
    let function: unsafe extern "C" fn(Object, Selector, Object, usize, Object, usize, usize) =
        unsafe { mem::transmute(objc_msgSend as *const ()) };
    unsafe {
        function(
            receiver,
            selector(name),
            source,
            source_offset,
            destination,
            destination_offset,
            size,
        );
    };
}

pub fn ns_string(value: &CStr) -> Object {
    // SAFETY: NSString copies the NUL-terminated bytes into an autoreleased immutable string.
    unsafe { object_c_string(class(c"NSString"), c"stringWithUTF8String:", value.as_ptr()) }
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
