//! Reports the native Metal device capabilities relevant to Zinc's backend design.

#![allow(clippy::missing_errors_doc)]

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, c_char, c_void};
    use std::fmt;
    use std::mem;
    use std::ptr::NonNull;

    type Object = *mut c_void;
    type Selector = *mut c_void;

    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        fn MTLCreateSystemDefaultDevice() -> Object;
    }

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_msgSend();
        fn sel_registerName(name: *const c_char) -> Selector;
    }

    #[derive(Clone, Copy)]
    struct Device(NonNull<c_void>);

    impl Device {
        fn system_default() -> Result<Self, ProbeError> {
            // SAFETY: Metal owns the returned Objective-C object. A non-null object remains valid
            // for the duration of this short-lived process.
            NonNull::new(unsafe { MTLCreateSystemDefaultDevice() })
                .map(Self)
                .ok_or(ProbeError::NoDevice)
        }

        fn object(self) -> Object {
            self.0.as_ptr()
        }

        fn responds_to(self, name: &CStr) -> bool {
            let query = selector(c"respondsToSelector:");
            let requested = selector(name);
            // SAFETY: `respondsToSelector:` accepts a selector and returns Objective-C BOOL.
            unsafe { send_bool_selector(self.object(), query, requested) }
        }

        fn string(self, name: &CStr) -> Option<String> {
            if !self.responds_to(name) {
                return None;
            }
            // SAFETY: The queried property returns an NSString object for the lifetime of device.
            let value = unsafe { send_object(self.object(), selector(name)) };
            if value.is_null() {
                return None;
            }
            // SAFETY: NSString's UTF8String is either null or a valid NUL-terminated byte string.
            let bytes = unsafe { send_c_string(value, selector(c"UTF8String")) };
            (!bytes.is_null()).then(|| {
                // SAFETY: `bytes` was checked for null and follows the UTF8String contract.
                unsafe { CStr::from_ptr(bytes) }
                    .to_string_lossy()
                    .into_owned()
            })
        }

        fn bool(self, name: &CStr) -> Option<bool> {
            self.responds_to(name).then(|| {
                // SAFETY: Selector availability was checked and the listed properties return BOOL.
                unsafe { send_bool(self.object(), selector(name)) }
            })
        }

        fn u64(self, name: &CStr) -> Option<u64> {
            self.responds_to(name).then(|| {
                // SAFETY: Selector availability was checked and the listed properties return u64.
                unsafe { send_u64(self.object(), selector(name)) }
            })
        }

        fn supports_family(self, family: usize) -> bool {
            let method = c"supportsFamily:";
            self.responds_to(method) && {
                // SAFETY: `supportsFamily:` accepts an MTLGPUFamily/NSInteger and returns BOOL.
                unsafe { send_bool_usize(self.object(), selector(method), family) }
            }
        }
    }

    #[derive(Debug)]
    pub enum ProbeError {
        NoDevice,
    }

    impl fmt::Display for ProbeError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::NoDevice => formatter.write_str("Metal returned no system device"),
            }
        }
    }

    impl std::error::Error for ProbeError {}

    pub fn run() -> Result<(), ProbeError> {
        let device = Device::system_default()?;

        println!("Zinc Metal capability probe");
        println!(
            "device: {}",
            device.string(c"name").as_deref().unwrap_or("unknown")
        );
        print_u64(device, "registry id", c"registryID", Unit::Integer);
        print_bool(device, "unified memory", c"hasUnifiedMemory");
        print_u64(
            device,
            "recommended working set",
            c"recommendedMaxWorkingSetSize",
            Unit::Bytes,
        );
        print_u64(
            device,
            "maximum transfer rate",
            c"maxTransferRate",
            Unit::BytesPerSecond,
        );

        println!("families:");
        for (name, value) in [
            ("Apple 7", 1007),
            ("Apple 8", 1008),
            ("Apple 9", 1009),
            ("Mac 2", 2002),
            ("Common 3", 3003),
            ("Metal 3", 5001),
        ] {
            println!("  {name:<12} {}", yes_no(device.supports_family(value)));
        }

        println!("advanced selectors:");
        for (name, method) in [
            ("ray tracing", c"supportsRaytracing"),
            ("ray tracing in render", c"supportsRaytracingFromRender"),
            ("function pointers", c"supportsFunctionPointers"),
            (
                "function pointers in render",
                c"supportsFunctionPointersFromRender",
            ),
            ("dynamic libraries", c"supportsDynamicLibraries"),
        ] {
            match device.bool(method) {
                Some(value) => println!("  {name:<28} {}", yes_no(value)),
                None => println!("  {name:<28} unavailable"),
            }
        }

        println!("Metal 4 SDK symbols: unavailable in this build (requires a newer Xcode SDK)");
        Ok(())
    }

    enum Unit {
        Integer,
        Bytes,
        BytesPerSecond,
    }

    fn print_bool(device: Device, label: &str, method: &CStr) {
        match device.bool(method) {
            Some(value) => println!("{label}: {}", yes_no(value)),
            None => println!("{label}: unavailable"),
        }
    }

    fn print_u64(device: Device, label: &str, method: &CStr, unit: Unit) {
        match (device.u64(method), unit) {
            (Some(value), Unit::Integer) => println!("{label}: {value}"),
            (Some(value), Unit::Bytes) => print_gib(label, value, "GiB"),
            (Some(0), Unit::BytesPerSecond) => println!("{label}: not reported"),
            (Some(value), Unit::BytesPerSecond) => print_gib(label, value, "GiB/s"),
            (None, _) => println!("{label}: unavailable"),
        }
    }

    fn print_gib(label: &str, value: u64, unit: &str) {
        const GIB: u64 = 1 << 30;
        let whole = value / GIB;
        let hundredths = (value % GIB) * 100 / GIB;
        println!("{label}: {whole}.{hundredths:02} {unit}");
    }

    fn yes_no(value: bool) -> &'static str {
        if value { "yes" } else { "no" }
    }

    fn selector(name: &CStr) -> Selector {
        // SAFETY: `name` is NUL-terminated and Objective-C interns selector names permanently.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    unsafe fn send_object(receiver: Object, selector: Selector) -> Object {
        // SAFETY: The caller supplies a selector whose ABI matches this function signature.
        let function: unsafe extern "C" fn(Object, Selector) -> Object =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        // SAFETY: Signature correctness is the caller's contract.
        unsafe { function(receiver, selector) }
    }

    unsafe fn send_c_string(receiver: Object, selector: Selector) -> *const c_char {
        // SAFETY: The caller supplies a selector whose ABI matches this function signature.
        let function: unsafe extern "C" fn(Object, Selector) -> *const c_char =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        // SAFETY: Signature correctness is the caller's contract.
        unsafe { function(receiver, selector) }
    }

    unsafe fn send_bool(receiver: Object, selector: Selector) -> bool {
        // SAFETY: The caller supplies a selector whose ABI matches this function signature.
        let function: unsafe extern "C" fn(Object, Selector) -> bool =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        // SAFETY: Signature correctness is the caller's contract.
        unsafe { function(receiver, selector) }
    }

    unsafe fn send_u64(receiver: Object, selector: Selector) -> u64 {
        // SAFETY: The caller supplies a selector whose ABI matches this function signature.
        let function: unsafe extern "C" fn(Object, Selector) -> u64 =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        // SAFETY: Signature correctness is the caller's contract.
        unsafe { function(receiver, selector) }
    }

    unsafe fn send_bool_selector(receiver: Object, selector: Selector, argument: Selector) -> bool {
        // SAFETY: The caller supplies a selector whose ABI matches this function signature.
        let function: unsafe extern "C" fn(Object, Selector, Selector) -> bool =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        // SAFETY: Signature correctness is the caller's contract.
        unsafe { function(receiver, selector, argument) }
    }

    unsafe fn send_bool_usize(receiver: Object, selector: Selector, argument: usize) -> bool {
        // SAFETY: The caller supplies a selector whose ABI matches this function signature.
        let function: unsafe extern "C" fn(Object, Selector, usize) -> bool =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        // SAFETY: Signature correctness is the caller's contract.
        unsafe { function(receiver, selector, argument) }
    }
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    macos::run().map_err(Into::into)
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("zinc-metal-info is available only on macOS");
}
