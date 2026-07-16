//! Reports the native Metal device capabilities relevant to Mulciber's backend design.

#![allow(clippy::missing_errors_doc)]

#[cfg(target_os = "macos")]
mod macos {
    use std::env;
    use std::ffi::{CStr, c_char, c_void};
    use std::fmt::{self, Write as _};
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

    struct Family {
        json_name: &'static str,
        display_name: &'static str,
        supported: bool,
    }

    struct Capability {
        json_name: &'static str,
        display_name: &'static str,
        supported: Option<bool>,
    }

    struct Report {
        name: String,
        registry_id: Option<u64>,
        unified_memory: Option<bool>,
        recommended_working_set_size: Option<u64>,
        maximum_transfer_rate: Option<u64>,
        maximum_buffer_length: Option<u64>,
        maximum_threadgroup_memory_length: Option<u64>,
        argument_buffers_tier: Option<u64>,
        read_write_texture_tier: Option<u64>,
        families: [Family; 6],
        capabilities: [Capability; 5],
    }

    impl Report {
        fn collect(device: Device) -> Self {
            Self {
                name: device.string(c"name").unwrap_or_else(|| "unknown".into()),
                registry_id: device.u64(c"registryID"),
                unified_memory: device.bool(c"hasUnifiedMemory"),
                recommended_working_set_size: device.u64(c"recommendedMaxWorkingSetSize"),
                maximum_transfer_rate: device.u64(c"maxTransferRate"),
                maximum_buffer_length: device.u64(c"maxBufferLength"),
                maximum_threadgroup_memory_length: device.u64(c"maxThreadgroupMemoryLength"),
                argument_buffers_tier: device.u64(c"argumentBuffersSupport"),
                read_write_texture_tier: device.u64(c"readWriteTextureSupport"),
                families: [
                    Family::new("apple_7", "Apple 7", device.supports_family(1007)),
                    Family::new("apple_8", "Apple 8", device.supports_family(1008)),
                    Family::new("apple_9", "Apple 9", device.supports_family(1009)),
                    Family::new("mac_2", "Mac 2", device.supports_family(2002)),
                    Family::new("common_3", "Common 3", device.supports_family(3003)),
                    Family::new("metal_3", "Metal 3", device.supports_family(5001)),
                ],
                capabilities: [
                    Capability::new(
                        "ray_tracing",
                        "ray tracing",
                        device.bool(c"supportsRaytracing"),
                    ),
                    Capability::new(
                        "ray_tracing_from_render",
                        "ray tracing in render",
                        device.bool(c"supportsRaytracingFromRender"),
                    ),
                    Capability::new(
                        "function_pointers",
                        "function pointers",
                        device.bool(c"supportsFunctionPointers"),
                    ),
                    Capability::new(
                        "function_pointers_from_render",
                        "function pointers in render",
                        device.bool(c"supportsFunctionPointersFromRender"),
                    ),
                    Capability::new(
                        "dynamic_libraries",
                        "dynamic libraries",
                        device.bool(c"supportsDynamicLibraries"),
                    ),
                ],
            }
        }

        fn print_human(&self) {
            println!("Mulciber Metal capability probe");
            println!("device: {}", self.name);
            print_u64("registry id", self.registry_id, Unit::Integer);
            print_bool("unified memory", self.unified_memory);
            print_u64(
                "recommended working set",
                self.recommended_working_set_size,
                Unit::Bytes,
            );
            print_u64(
                "maximum transfer rate",
                self.maximum_transfer_rate,
                Unit::BytesPerSecond,
            );
            print_u64(
                "maximum buffer length",
                self.maximum_buffer_length,
                Unit::Bytes,
            );
            print_u64(
                "maximum threadgroup memory",
                self.maximum_threadgroup_memory_length,
                Unit::Bytes,
            );
            print_tier("argument buffers tier", self.argument_buffers_tier, 1);
            print_tier("read-write texture tier", self.read_write_texture_tier, 0);

            println!("families:");
            for family in &self.families {
                println!("  {:<12} {}", family.display_name, yes_no(family.supported));
            }

            println!("advanced selectors:");
            for capability in &self.capabilities {
                match capability.supported {
                    Some(value) => println!("  {:<28} {}", capability.display_name, yes_no(value)),
                    None => println!("  {:<28} unavailable", capability.display_name),
                }
            }

            println!("Metal 4 SDK symbols: unavailable in this build (requires a newer Xcode SDK)");
        }

        fn json(&self) -> String {
            let mut output = String::new();
            output.push_str("{\n  \"schema_version\": 1,\n  \"backend\": \"metal\",\n  \"device\": {\n    \"name\": ");
            push_json_string(&mut output, &self.name);
            push_field(&mut output, "registry_id", self.registry_id);
            push_field(&mut output, "unified_memory", self.unified_memory);
            push_field(
                &mut output,
                "recommended_working_set_size_bytes",
                self.recommended_working_set_size,
            );
            push_field(
                &mut output,
                "maximum_transfer_rate_bytes_per_second",
                self.maximum_transfer_rate,
            );
            push_field(
                &mut output,
                "maximum_buffer_length_bytes",
                self.maximum_buffer_length,
            );
            push_field(
                &mut output,
                "maximum_threadgroup_memory_length_bytes",
                self.maximum_threadgroup_memory_length,
            );
            push_field(
                &mut output,
                "argument_buffers_tier",
                self.argument_buffers_tier.map(|tier| tier + 1),
            );
            push_field(
                &mut output,
                "read_write_texture_tier",
                self.read_write_texture_tier,
            );
            output.push_str("\n  },\n  \"families\": {");
            for (index, family) in self.families.iter().enumerate() {
                let separator = if index == 0 { "\n" } else { ",\n" };
                write!(
                    output,
                    "{separator}    \"{}\": {}",
                    family.json_name, family.supported
                )
                .expect("writing to a String cannot fail");
            }
            output.push_str("\n  },\n  \"capabilities\": {");
            for (index, capability) in self.capabilities.iter().enumerate() {
                let separator = if index == 0 { "\n" } else { ",\n" };
                write!(output, "{separator}    \"{}\": ", capability.json_name)
                    .expect("writing to a String cannot fail");
                push_json_value(&mut output, capability.supported);
            }
            output.push_str("\n  },\n  \"build\": {\n    \"metal_4_sdk_symbols\": false\n  }\n}");
            output
        }
    }

    impl Family {
        const fn new(json_name: &'static str, display_name: &'static str, supported: bool) -> Self {
            Self {
                json_name,
                display_name,
                supported,
            }
        }
    }

    impl Capability {
        const fn new(
            json_name: &'static str,
            display_name: &'static str,
            supported: Option<bool>,
        ) -> Self {
            Self {
                json_name,
                display_name,
                supported,
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
        let report = Report::collect(device);
        if env::args_os().skip(1).any(|argument| argument == "--json") {
            println!("{}", report.json());
        } else {
            report.print_human();
        }
        Ok(())
    }

    enum Unit {
        Integer,
        Bytes,
        BytesPerSecond,
    }

    fn print_bool(label: &str, value: Option<bool>) {
        match value {
            Some(value) => println!("{label}: {}", yes_no(value)),
            None => println!("{label}: unavailable"),
        }
    }

    fn print_u64(label: &str, value: Option<u64>, unit: Unit) {
        match (value, unit) {
            (Some(value), Unit::Integer) => println!("{label}: {value}"),
            (Some(value), Unit::Bytes) => print_gib(label, value, "GiB"),
            (Some(0), Unit::BytesPerSecond) => println!("{label}: not reported"),
            (Some(value), Unit::BytesPerSecond) => print_gib(label, value, "GiB/s"),
            (None, _) => println!("{label}: unavailable"),
        }
    }

    fn print_tier(label: &str, value: Option<u64>, display_offset: u64) {
        match value {
            Some(value) => println!("{label}: {}", value + display_offset),
            None => println!("{label}: unavailable"),
        }
    }

    fn print_gib(label: &str, value: u64, unit: &str) {
        const KIB: u64 = 1 << 10;
        const MIB: u64 = 1 << 20;
        const GIB: u64 = 1 << 30;
        if unit == "GiB" && value < MIB {
            let whole = value / KIB;
            let hundredths = (value % KIB) * 100 / KIB;
            println!("{label}: {whole}.{hundredths:02} KiB");
            return;
        }
        let whole = value / GIB;
        let hundredths = (value % GIB) * 100 / GIB;
        println!("{label}: {whole}.{hundredths:02} {unit}");
    }

    fn yes_no(value: bool) -> &'static str {
        if value { "yes" } else { "no" }
    }

    fn push_field<T: fmt::Display>(output: &mut String, name: &str, value: Option<T>) {
        write!(output, ",\n    \"{name}\": ").expect("writing to a String cannot fail");
        push_json_value(output, value);
    }

    fn push_json_value<T: fmt::Display>(output: &mut String, value: Option<T>) {
        match value {
            Some(value) => write!(output, "{value}").expect("writing to a String cannot fail"),
            None => output.push_str("null"),
        }
    }

    fn push_json_string(output: &mut String, value: &str) {
        output.push('"');
        for character in value.chars() {
            match character {
                '"' => output.push_str("\\\""),
                '\\' => output.push_str("\\\\"),
                '\u{08}' => output.push_str("\\b"),
                '\u{0c}' => output.push_str("\\f"),
                '\n' => output.push_str("\\n"),
                '\r' => output.push_str("\\r"),
                '\t' => output.push_str("\\t"),
                control if control <= '\u{1f}' => {
                    write!(output, "\\u{:04x}", u32::from(control))
                        .expect("writing to a String cannot fail");
                }
                character => output.push(character),
            }
        }
        output.push('"');
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
    eprintln!("mulciber-metal-info is available only on macOS");
}
