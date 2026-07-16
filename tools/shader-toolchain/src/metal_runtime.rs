//! Minimal native Metal pipeline-creation proof for linked evaluation libraries.

#[derive(Clone, Copy)]
pub enum PipelineSpec<'a> {
    Render { vertex: &'a str, fragment: &'a str },
    Compute { function: &'a str },
}

#[cfg(not(target_os = "macos"))]
pub fn create_pipeline(_library: &std::path::Path, _spec: PipelineSpec<'_>) -> Result<(), String> {
    Err("native Metal pipeline creation requires macOS".into())
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::mem;
    use std::path::Path;
    use std::ptr;

    use super::PipelineSpec;

    type Object = *mut c_void;
    type Selector = *mut c_void;

    const PIXEL_FORMAT_BGRA8_UNORM: usize = 80;
    const VERTEX_FORMAT_FLOAT2: usize = 29;
    const VERTEX_FORMAT_FLOAT3: usize = 30;

    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        fn MTLCreateSystemDefaultDevice() -> Object;
    }

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Object;
        fn sel_registerName(name: *const c_char) -> Selector;
        fn objc_msgSend();
    }

    pub fn create_pipeline(library_path: &Path, spec: PipelineSpec<'_>) -> Result<(), String> {
        let _pool = AutoreleasePool::new()?;
        let path = CString::new(library_path.to_string_lossy().as_bytes())
            .map_err(|_| "metallib path contains an interior NUL byte".to_owned())?;

        // SAFETY: The Metal and Objective-C selectors below match their SDK declarations, and
        // every owned object remains alive through pipeline creation.
        unsafe {
            let device = required(MTLCreateSystemDefaultDevice(), "default Metal device")?;
            let path_string = message_object_pointer(
                class(c"NSString")?,
                c"stringWithUTF8String:",
                path.as_ptr(),
            );
            let path_string = required(path_string, "metallib path string")?;
            let mut error = ptr::null_mut();
            let library = message_object_object_out(
                device,
                c"newLibraryWithFile:error:",
                path_string,
                &raw mut error,
            );
            let library = required_with_error(library, error, "load linked metallib")?;

            let result = match spec {
                PipelineSpec::Render { vertex, fragment } => {
                    create_render_pipeline(device, library, vertex, fragment)
                }
                PipelineSpec::Compute { function } => {
                    create_compute_pipeline(device, library, function)
                }
            };
            message_void(library, c"release");
            result
        }
    }

    unsafe fn create_render_pipeline(
        device: Object,
        library: Object,
        vertex_name: &str,
        fragment_name: &str,
    ) -> Result<(), String> {
        // SAFETY: The caller holds valid Metal device and library objects.
        unsafe {
            let vertex = load_function(library, vertex_name)?;
            let fragment = load_function(library, fragment_name)?;
            let descriptor = required(
                message_object(class(c"MTLRenderPipelineDescriptor")?, c"new"),
                "render pipeline descriptor",
            )?;
            message_void_object(descriptor, c"setVertexFunction:", vertex);
            message_void_object(descriptor, c"setFragmentFunction:", fragment);

            let attachments = required(
                message_object(descriptor, c"colorAttachments"),
                "render pipeline color attachments",
            )?;
            let color = required(
                message_object_usize(attachments, c"objectAtIndexedSubscript:", 0),
                "render pipeline color attachment zero",
            )?;
            message_void_usize(color, c"setPixelFormat:", PIXEL_FORMAT_BGRA8_UNORM);
            configure_vertex_descriptor(descriptor)?;

            let mut error = ptr::null_mut();
            let pipeline = message_object_object_out(
                device,
                c"newRenderPipelineStateWithDescriptor:error:",
                descriptor,
                &raw mut error,
            );
            let pipeline = required_with_error(pipeline, error, "create render pipeline")?;
            message_void(pipeline, c"release");
            message_void(descriptor, c"release");
            message_void(fragment, c"release");
            message_void(vertex, c"release");
            Ok(())
        }
    }

    unsafe fn configure_vertex_descriptor(descriptor: Object) -> Result<(), String> {
        // Both scene corpora use location 0 as float3 position and location 1 as float2 UV.
        // Buffer slot 1 avoids the scene uniform's vertex-stage buffer slot 0.
        unsafe {
            let vertex = required(
                message_object(class(c"MTLVertexDescriptor")?, c"vertexDescriptor"),
                "vertex descriptor",
            )?;
            let attributes = required(
                message_object(vertex, c"attributes"),
                "vertex descriptor attributes",
            )?;
            let position = required(
                message_object_usize(attributes, c"objectAtIndexedSubscript:", 0),
                "position vertex attribute",
            )?;
            message_void_usize(position, c"setFormat:", VERTEX_FORMAT_FLOAT3);
            message_void_usize(position, c"setOffset:", 0);
            message_void_usize(position, c"setBufferIndex:", 1);

            let uv = required(
                message_object_usize(attributes, c"objectAtIndexedSubscript:", 1),
                "UV vertex attribute",
            )?;
            message_void_usize(uv, c"setFormat:", VERTEX_FORMAT_FLOAT2);
            message_void_usize(uv, c"setOffset:", 12);
            message_void_usize(uv, c"setBufferIndex:", 1);

            let layouts = required(
                message_object(vertex, c"layouts"),
                "vertex descriptor layouts",
            )?;
            let layout = required(
                message_object_usize(layouts, c"objectAtIndexedSubscript:", 1),
                "vertex buffer layout one",
            )?;
            message_void_usize(layout, c"setStride:", 20);
            message_void_object(descriptor, c"setVertexDescriptor:", vertex);
            Ok(())
        }
    }

    unsafe fn create_compute_pipeline(
        device: Object,
        library: Object,
        function_name: &str,
    ) -> Result<(), String> {
        // SAFETY: The caller holds valid Metal device and library objects.
        unsafe {
            let function = load_function(library, function_name)?;
            let mut error = ptr::null_mut();
            let pipeline = message_object_object_out(
                device,
                c"newComputePipelineStateWithFunction:error:",
                function,
                &raw mut error,
            );
            let pipeline = required_with_error(pipeline, error, "create compute pipeline")?;
            message_void(pipeline, c"release");
            message_void(function, c"release");
            Ok(())
        }
    }

    unsafe fn load_function(library: Object, name: &str) -> Result<Object, String> {
        let name =
            CString::new(name).map_err(|_| "function name contains an interior NUL".to_owned())?;
        // SAFETY: NSString copies the function name, and MTLLibrary owns the returned function.
        unsafe {
            let name = required(
                message_object_pointer(
                    class(c"NSString")?,
                    c"stringWithUTF8String:",
                    name.as_ptr(),
                ),
                "Metal function name string",
            )?;
            required(
                message_object_object(library, c"newFunctionWithName:", name),
                "function from linked metallib",
            )
        }
    }

    fn required(object: Object, context: &str) -> Result<Object, String> {
        if object.is_null() {
            Err(format!("Metal returned null while attempting to {context}"))
        } else {
            Ok(object)
        }
    }

    fn required_with_error(object: Object, error: Object, context: &str) -> Result<Object, String> {
        if object.is_null() {
            Err(format!("failed to {context}: {}", error_description(error)))
        } else {
            Ok(object)
        }
    }

    fn error_description(error: Object) -> String {
        if error.is_null() {
            return "unknown Objective-C error".into();
        }
        // SAFETY: NSError's localizedDescription is an NSString whose UTF8String remains valid
        // through the current autorelease pool.
        unsafe {
            let description = message_object(error, c"localizedDescription");
            if description.is_null() {
                return "unknown Objective-C error".into();
            }
            let bytes = message_pointer(description, c"UTF8String");
            if bytes.is_null() {
                "unknown Objective-C error".into()
            } else {
                CStr::from_ptr(bytes).to_string_lossy().into_owned()
            }
        }
    }

    unsafe fn class(name: &CStr) -> Result<Object, String> {
        // SAFETY: The class name is a stable NUL-terminated string.
        let class = unsafe { objc_getClass(name.as_ptr()) };
        required(class, &format!("find Objective-C class {name:?}"))
    }

    unsafe fn selector(name: &CStr) -> Selector {
        // SAFETY: The selector name is a stable NUL-terminated string.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    unsafe fn message_object(receiver: Object, name: &CStr) -> Object {
        // SAFETY: The caller guarantees that the selector returns an Objective-C object.
        let function: unsafe extern "C" fn(Object, Selector) -> Object =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        unsafe { function(receiver, selector(name)) }
    }

    unsafe fn message_object_object(receiver: Object, name: &CStr, argument: Object) -> Object {
        // SAFETY: The caller guarantees that the selector takes and returns Objective-C objects.
        let function: unsafe extern "C" fn(Object, Selector, Object) -> Object =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        unsafe { function(receiver, selector(name), argument) }
    }

    unsafe fn message_object_pointer(
        receiver: Object,
        name: &CStr,
        argument: *const c_char,
    ) -> Object {
        // SAFETY: The caller guarantees the selector accepts a NUL-terminated C string.
        let function: unsafe extern "C" fn(Object, Selector, *const c_char) -> Object =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        unsafe { function(receiver, selector(name), argument) }
    }

    unsafe fn message_object_usize(receiver: Object, name: &CStr, argument: usize) -> Object {
        // SAFETY: The caller guarantees the selector accepts an NSUInteger and returns an object.
        let function: unsafe extern "C" fn(Object, Selector, usize) -> Object =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        unsafe { function(receiver, selector(name), argument) }
    }

    unsafe fn message_object_object_out(
        receiver: Object,
        name: &CStr,
        argument: Object,
        error: *mut Object,
    ) -> Object {
        // SAFETY: The caller guarantees the selector takes an object and NSError out-pointer.
        let function: unsafe extern "C" fn(Object, Selector, Object, *mut Object) -> Object =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        unsafe { function(receiver, selector(name), argument, error) }
    }

    unsafe fn message_pointer(receiver: Object, name: &CStr) -> *const c_char {
        // SAFETY: The caller guarantees that the selector returns a C string pointer.
        let function: unsafe extern "C" fn(Object, Selector) -> *const c_char =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        unsafe { function(receiver, selector(name)) }
    }

    unsafe fn message_void(receiver: Object, name: &CStr) {
        // SAFETY: The caller guarantees that the selector has no arguments or return value.
        let function: unsafe extern "C" fn(Object, Selector) =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        unsafe { function(receiver, selector(name)) };
    }

    unsafe fn message_void_object(receiver: Object, name: &CStr, argument: Object) {
        // SAFETY: The caller guarantees that the selector accepts one Objective-C object.
        let function: unsafe extern "C" fn(Object, Selector, Object) =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        unsafe { function(receiver, selector(name), argument) };
    }

    unsafe fn message_void_usize(receiver: Object, name: &CStr, argument: usize) {
        // SAFETY: The caller guarantees that the selector accepts one NSUInteger.
        let function: unsafe extern "C" fn(Object, Selector, usize) =
            unsafe { mem::transmute(objc_msgSend as *const ()) };
        unsafe { function(receiver, selector(name), argument) };
    }

    struct AutoreleasePool(Object);

    impl AutoreleasePool {
        fn new() -> Result<Self, String> {
            // SAFETY: NSAutoreleasePool's `new` method returns an owned pool.
            unsafe {
                Ok(Self(required(
                    message_object(class(c"NSAutoreleasePool")?, c"new"),
                    "create autorelease pool",
                )?))
            }
        }
    }

    impl Drop for AutoreleasePool {
        fn drop(&mut self) {
            // SAFETY: This value owns the pool and drains it once on its creating thread.
            unsafe { message_void(self.0, c"drain") };
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::create_pipeline;
