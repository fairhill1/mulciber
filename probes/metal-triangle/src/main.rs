//! Renders a triangle through native `AppKit`, `CAMetalLayer`, and Metal APIs.

#[cfg(target_os = "macos")]
mod objc;

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::fmt;
    use std::ptr;
    use std::thread;
    use std::time::Duration;
    use std::{env, num::NonZeroU64};

    use crate::objc::{self, AutoreleasePool, ClearColor, Object, Point, Rect, Size};

    const PIXEL_FORMAT_BGRA8_UNORM: usize = 80;
    const LOAD_ACTION_CLEAR: usize = 2;
    const STORE_ACTION_STORE: usize = 1;
    const PRIMITIVE_TYPE_TRIANGLE: usize = 3;
    const OCCLUSION_STATE_VISIBLE: usize = 1 << 1;

    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        fn MTLCreateSystemDefaultDevice() -> Object;
    }

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSDefaultRunLoopMode: Object;
    }

    #[link(name = "QuartzCore", kind = "framework")]
    unsafe extern "C" {}

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Vertex {
        position: [f32; 4],
        color: [f32; 4],
    }

    const VERTICES: [Vertex; 3] = [
        Vertex {
            position: [0.0, 0.65, 0.0, 1.0],
            color: [1.0, 0.2, 0.15, 1.0],
        },
        Vertex {
            position: [-0.62, -0.45, 0.0, 1.0],
            color: [0.15, 0.85, 0.35, 1.0],
        },
        Vertex {
            position: [0.62, -0.45, 0.0, 1.0],
            color: [0.2, 0.4, 1.0, 1.0],
        },
    ];

    #[derive(Debug)]
    pub struct ProbeError(String);

    impl fmt::Display for ProbeError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    impl std::error::Error for ProbeError {}

    struct Probe {
        application: Object,
        window: Object,
        view: Object,
        layer: Object,
        queue: Object,
        pipeline: Object,
        vertices: Object,
        drawable_size: Size,
    }

    impl Probe {
        fn new() -> Result<Self, ProbeError> {
            // SAFETY: The program runs on the AppKit main thread and all selectors match SDK ABIs.
            unsafe {
                let application = required(
                    objc::object(objc::class(c"NSApplication"), c"sharedApplication"),
                    "NSApplication",
                )?;
                if !objc::bool_isize(application, c"setActivationPolicy:", 0) {
                    return Err(ProbeError(
                        "could not activate as a regular application".into(),
                    ));
                }
                objc::void(application, c"finishLaunching");

                let style = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3);
                let initial_rect = Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size {
                        width: 960.0,
                        height: 540.0,
                    },
                };
                let allocated_window = objc::object(objc::class(c"NSWindow"), c"alloc");
                let window = required(
                    objc::object_window_init(
                        allocated_window,
                        c"initWithContentRect:styleMask:backing:defer:",
                        initial_rect,
                        style,
                        2,
                        false,
                    ),
                    "NSWindow",
                )?;
                objc::void_object(
                    window,
                    c"setTitle:",
                    objc::ns_string(c"Zinc — native Metal"),
                );
                objc::void_bool(window, c"setReleasedWhenClosed:", false);

                let view = required(objc::object(window, c"contentView"), "NSView")?;
                objc::void_bool(view, c"setWantsLayer:", true);

                let device = required(MTLCreateSystemDefaultDevice(), "Metal device")?;
                let layer = required(
                    objc::object(objc::class(c"CAMetalLayer"), c"layer"),
                    "CAMetalLayer",
                )?;
                objc::void_object(layer, c"setDevice:", device);
                objc::void_usize(layer, c"setPixelFormat:", PIXEL_FORMAT_BGRA8_UNORM);
                objc::void_bool(layer, c"setFramebufferOnly:", true);
                objc::void_usize(layer, c"setMaximumDrawableCount:", 3);
                objc::void_bool(layer, c"setDisplaySyncEnabled:", true);
                objc::void_bool(layer, c"setAllowsNextDrawableTimeout:", true);
                objc::void_object(view, c"setLayer:", layer);

                let queue = required(
                    objc::object(device, c"newCommandQueue"),
                    "Metal command queue",
                )?;
                let pipeline = create_pipeline(device)?;
                let vertices = required(
                    objc::object_bytes(
                        device,
                        c"newBufferWithBytes:length:options:",
                        VERTICES.as_ptr().cast::<c_void>(),
                        size_of_val(&VERTICES),
                        0,
                    ),
                    "Metal vertex buffer",
                )?;

                objc::void(window, c"center");
                objc::void_object(window, c"makeKeyAndOrderFront:", ptr::null_mut());
                objc::void(application, c"activate");

                Ok(Self {
                    application,
                    window,
                    view,
                    layer,
                    queue,
                    pipeline,
                    vertices,
                    drawable_size: Size::default(),
                })
            }
        }

        fn run(mut self, frame_limit: Option<NonZeroU64>) -> Result<(), ProbeError> {
            let mut rendered_frames = 0;
            while self.pump_events() {
                let _pool = AutoreleasePool::new();
                if self.render()? {
                    rendered_frames += 1;
                    if frame_limit.is_some_and(|limit| rendered_frames >= limit.get()) {
                        break;
                    }
                } else {
                    thread::sleep(Duration::from_millis(16));
                }
            }
            self.finish_gpu()
        }

        fn pump_events(&self) -> bool {
            // SAFETY: Events are polled and dispatched on AppKit's main thread.
            unsafe {
                let date = objc::object(objc::class(c"NSDate"), c"distantPast");
                loop {
                    let event = objc::object_event(
                        self.application,
                        c"nextEventMatchingMask:untilDate:inMode:dequeue:",
                        usize::MAX,
                        date,
                        NSDefaultRunLoopMode,
                        true,
                    );
                    if event.is_null() {
                        break;
                    }
                    objc::void_object(self.application, c"sendEvent:", event);
                }
                objc::void(self.application, c"updateWindows");

                let visible = objc::bool_value(self.window, c"isVisible");
                let minimized = objc::bool_value(self.window, c"isMiniaturized");
                visible || minimized
            }
        }

        fn render(&mut self) -> Result<bool, ProbeError> {
            // SAFETY: Metal and AppKit objects are alive and each selector matches the SDK ABI.
            unsafe {
                if objc::bool_value(self.window, c"isMiniaturized")
                    || objc::usize_value(self.window, c"occlusionState") & OCCLUSION_STATE_VISIBLE
                        == 0
                {
                    return Ok(false);
                }

                let logical = objc::rect_value(self.view, c"bounds");
                let backing = objc::rect_rect(self.view, c"convertRectToBacking:", logical);
                if backing.size.width <= 0.0 || backing.size.height <= 0.0 {
                    return Ok(false);
                }
                if backing.size != self.drawable_size {
                    self.drawable_size = backing.size;
                    objc::void_size(self.layer, c"setDrawableSize:", backing.size);
                    let scale = objc::f64_value(self.window, c"backingScaleFactor");
                    objc::void_f64(self.layer, c"setContentsScale:", scale);
                }

                let drawable = objc::object(self.layer, c"nextDrawable");
                if drawable.is_null() {
                    return Ok(false);
                }
                let texture = required(objc::object(drawable, c"texture"), "drawable texture")?;

                let pass = required(
                    objc::object(
                        objc::class(c"MTLRenderPassDescriptor"),
                        c"renderPassDescriptor",
                    ),
                    "render pass descriptor",
                )?;
                let attachments = required(
                    objc::object(pass, c"colorAttachments"),
                    "color attachment array",
                )?;
                let color = required(
                    objc::object_usize(attachments, c"objectAtIndexedSubscript:", 0),
                    "color attachment",
                )?;
                objc::void_object(color, c"setTexture:", texture);
                objc::void_usize(color, c"setLoadAction:", LOAD_ACTION_CLEAR);
                objc::void_usize(color, c"setStoreAction:", STORE_ACTION_STORE);
                objc::void_clear_color(
                    color,
                    c"setClearColor:",
                    ClearColor {
                        red: 0.015,
                        green: 0.02,
                        blue: 0.035,
                        alpha: 1.0,
                    },
                );

                let command_buffer = required(
                    objc::object(self.queue, c"commandBuffer"),
                    "Metal command buffer",
                )?;
                let encoder = required(
                    objc::object_object(
                        command_buffer,
                        c"renderCommandEncoderWithDescriptor:",
                        pass,
                    ),
                    "Metal render encoder",
                )?;
                objc::void_object(encoder, c"setRenderPipelineState:", self.pipeline);
                objc::void_object_two_usizes(
                    encoder,
                    c"setVertexBuffer:offset:atIndex:",
                    self.vertices,
                    0,
                    0,
                );
                objc::void_three_usizes(
                    encoder,
                    c"drawPrimitives:vertexStart:vertexCount:",
                    PRIMITIVE_TYPE_TRIANGLE,
                    0,
                    VERTICES.len(),
                );
                objc::void(encoder, c"endEncoding");
                objc::void_object(command_buffer, c"presentDrawable:", drawable);
                objc::void(command_buffer, c"commit");
                Ok(true)
            }
        }

        fn finish_gpu(&self) -> Result<(), ProbeError> {
            // SAFETY: A final empty command buffer drains all earlier work on this serial queue.
            unsafe {
                let command_buffer = required(
                    objc::object(self.queue, c"commandBuffer"),
                    "shutdown command buffer",
                )?;
                objc::void(command_buffer, c"commit");
                objc::void(command_buffer, c"waitUntilCompleted");
            }
            Ok(())
        }
    }

    fn create_pipeline(device: Object) -> Result<Object, ProbeError> {
        // SAFETY: Synchronous Metal compilation retains all inputs for the duration of the calls.
        unsafe {
            let mut error = ptr::null_mut();
            let mut source = include_bytes!("shader.metal").to_vec();
            source.push(0);
            let library = objc::object_two_objects_out(
                device,
                c"newLibraryWithSource:options:error:",
                objc::ns_string_bytes(&source),
                ptr::null_mut(),
                &raw mut error,
            );
            if library.is_null() {
                return Err(ProbeError(format!(
                    "Metal shader compilation failed: {}",
                    objc::description(error)
                )));
            }

            let vertex = required(
                objc::object_object(
                    library,
                    c"newFunctionWithName:",
                    objc::ns_string(c"vertex_main"),
                ),
                "vertex function",
            )?;
            let fragment = required(
                objc::object_object(
                    library,
                    c"newFunctionWithName:",
                    objc::ns_string(c"fragment_main"),
                ),
                "fragment function",
            )?;

            let descriptor = required(
                objc::object(objc::class(c"MTLRenderPipelineDescriptor"), c"new"),
                "render pipeline descriptor",
            )?;
            objc::void_object(descriptor, c"setVertexFunction:", vertex);
            objc::void_object(descriptor, c"setFragmentFunction:", fragment);
            let attachments = required(
                objc::object(descriptor, c"colorAttachments"),
                "pipeline color attachments",
            )?;
            let color = required(
                objc::object_usize(attachments, c"objectAtIndexedSubscript:", 0),
                "pipeline color attachment",
            )?;
            objc::void_usize(color, c"setPixelFormat:", PIXEL_FORMAT_BGRA8_UNORM);

            error = ptr::null_mut();
            let pipeline = objc::object_object_out(
                device,
                c"newRenderPipelineStateWithDescriptor:error:",
                descriptor,
                &raw mut error,
            );
            if pipeline.is_null() {
                return Err(ProbeError(format!(
                    "Metal pipeline creation failed: {}",
                    objc::description(error)
                )));
            }
            Ok(pipeline)
        }
    }

    fn required(value: Object, label: &str) -> Result<Object, ProbeError> {
        if value.is_null() {
            Err(ProbeError(format!("{label} is unavailable")))
        } else {
            Ok(value)
        }
    }

    pub fn run() -> Result<(), ProbeError> {
        let _pool = AutoreleasePool::new();
        Probe::new()?.run(parse_frame_limit()?)
    }

    fn parse_frame_limit() -> Result<Option<NonZeroU64>, ProbeError> {
        let mut arguments = env::args().skip(1);
        let Some(argument) = arguments.next() else {
            return Ok(None);
        };
        if argument != "--frames" {
            return Err(ProbeError(format!("unknown argument: {argument}")));
        }
        let value = arguments
            .next()
            .ok_or_else(|| ProbeError("--frames requires a positive integer".into()))?
            .parse::<NonZeroU64>()
            .map_err(|error| ProbeError(format!("invalid --frames value: {error}")))?;
        if let Some(extra) = arguments.next() {
            return Err(ProbeError(format!("unexpected argument: {extra}")));
        }
        Ok(Some(value))
    }
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    macos::run().map_err(Into::into)
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("zinc-metal-triangle is available only on macOS");
}
