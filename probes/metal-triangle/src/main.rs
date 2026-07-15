//! Renders an indexed, textured, depth-tested scene through native Apple APIs.

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

    use crate::objc::{
        self, AutoreleasePool, ClearColor, Object, Origin3, Point, Rect, Size, Size3,
    };

    const PIXEL_FORMAT_BGRA8_UNORM: usize = 80;
    const PIXEL_FORMAT_RGBA8_UNORM: usize = 70;
    const PIXEL_FORMAT_DEPTH32_FLOAT: usize = 252;
    const LOAD_ACTION_CLEAR: usize = 2;
    const STORE_ACTION_STORE: usize = 1;
    const STORE_ACTION_DONT_CARE: usize = 0;
    const PRIMITIVE_TYPE_TRIANGLE: usize = 3;
    const INDEX_TYPE_UINT16: usize = 0;
    const STORAGE_MODE_PRIVATE: usize = 2;
    const TEXTURE_USAGE_SHADER_READ: usize = 1;
    const TEXTURE_USAGE_SHADER_WRITE: usize = 2;
    const TEXTURE_USAGE_RENDER_TARGET: usize = 4;
    const COMPARE_FUNCTION_LESS: usize = 1;
    const SAMPLER_FILTER_LINEAR: usize = 1;
    const SAMPLER_ADDRESS_REPEAT: usize = 2;
    const OCCLUSION_STATE_VISIBLE: usize = 1 << 1;
    const CHECKER_WIDTH: usize = 8;
    const CHECKER_HEIGHT: usize = 8;
    const READBACK_BYTES_PER_ROW: usize = 256;
    const METALLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shader.metallib"));

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

    #[link(name = "System")]
    unsafe extern "C" {
        fn dispatch_data_create(
            buffer: *const c_void,
            size: usize,
            queue: Object,
            destructor: Object,
        ) -> Object;
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Vertex {
        position: [f32; 4],
        color: [f32; 4],
        texture_coordinate: [f32; 4],
    }

    const VERTICES: [Vertex; 8] = [
        Vertex {
            position: [-0.82, 0.72, 0.65, 1.0],
            color: [0.45, 0.58, 0.95, 1.0],
            texture_coordinate: [0.0, 0.0, 0.0, 0.0],
        },
        Vertex {
            position: [-0.82, -0.72, 0.65, 1.0],
            color: [0.45, 0.58, 0.95, 1.0],
            texture_coordinate: [0.0, 2.0, 0.0, 0.0],
        },
        Vertex {
            position: [0.82, -0.72, 0.65, 1.0],
            color: [0.45, 0.58, 0.95, 1.0],
            texture_coordinate: [2.0, 2.0, 0.0, 0.0],
        },
        Vertex {
            position: [0.82, 0.72, 0.65, 1.0],
            color: [0.45, 0.58, 0.95, 1.0],
            texture_coordinate: [2.0, 0.0, 0.0, 0.0],
        },
        Vertex {
            position: [-0.48, 0.44, 0.2, 1.0],
            color: [1.0, 0.72, 0.3, 1.0],
            texture_coordinate: [0.0, 0.0, 0.0, 0.0],
        },
        Vertex {
            position: [-0.48, -0.44, 0.2, 1.0],
            color: [1.0, 0.72, 0.3, 1.0],
            texture_coordinate: [0.0, 1.0, 0.0, 0.0],
        },
        Vertex {
            position: [0.48, -0.44, 0.2, 1.0],
            color: [1.0, 0.72, 0.3, 1.0],
            texture_coordinate: [1.0, 1.0, 0.0, 0.0],
        },
        Vertex {
            position: [0.48, 0.44, 0.2, 1.0],
            color: [1.0, 0.72, 0.3, 1.0],
            texture_coordinate: [1.0, 0.0, 0.0, 0.0],
        },
    ];

    const INDICES: [u16; 12] = [0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7];
    const CHECKER_PIXELS: [u8; CHECKER_WIDTH * CHECKER_HEIGHT * 4] = checker_pixels();

    const fn checker_pixels() -> [u8; CHECKER_WIDTH * CHECKER_HEIGHT * 4] {
        let mut pixels = [0; CHECKER_WIDTH * CHECKER_HEIGHT * 4];
        let mut y = 0;
        while y < CHECKER_HEIGHT {
            let mut x = 0;
            while x < CHECKER_WIDTH {
                let offset = (y * CHECKER_WIDTH + x) * 4;
                let bright = (x + y) % 2 == 0;
                pixels[offset] = if bright { 235 } else { 34 };
                pixels[offset + 1] = if bright { 242 } else { 42 };
                pixels[offset + 2] = if bright { 255 } else { 67 };
                pixels[offset + 3] = 255;
                x += 1;
            }
            y += 1;
        }
        pixels
    }

    #[derive(Debug)]
    pub struct ProbeError(String);

    impl fmt::Display for ProbeError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    impl std::error::Error for ProbeError {}

    struct PipelineStates {
        render: Object,
        compute: Object,
    }

    struct Probe {
        application: Object,
        window: Object,
        view: Object,
        device: Object,
        layer: Object,
        queue: Object,
        pipeline: Object,
        depth_state: Object,
        vertices: Object,
        indices: Object,
        texture: Object,
        sampler: Object,
        depth_texture: Object,
        depth_extent: (usize, usize),
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
                let pipelines = create_pipelines(device)?;
                let depth_state = create_depth_state(device)?;
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
                let indices = required(
                    objc::object_bytes(
                        device,
                        c"newBufferWithBytes:length:options:",
                        INDICES.as_ptr().cast::<c_void>(),
                        size_of_val(&INDICES),
                        0,
                    ),
                    "Metal index buffer",
                )?;
                let texture = create_texture(device, queue, pipelines.compute)?;
                let sampler = create_sampler(device)?;

                objc::void(window, c"center");
                objc::void_object(window, c"makeKeyAndOrderFront:", ptr::null_mut());
                objc::void(application, c"activate");

                Ok(Self {
                    application,
                    window,
                    view,
                    device,
                    layer,
                    queue,
                    pipeline: pipelines.render,
                    depth_state,
                    vertices,
                    indices,
                    texture,
                    sampler,
                    depth_texture: ptr::null_mut(),
                    depth_extent: (0, 0),
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
                let drawable_texture =
                    required(objc::object(drawable, c"texture"), "drawable texture")?;
                let drawable_extent = (
                    objc::usize_value(drawable_texture, c"width"),
                    objc::usize_value(drawable_texture, c"height"),
                );
                if drawable_extent != self.depth_extent {
                    self.depth_texture =
                        create_depth_texture(self.device, drawable_extent.0, drawable_extent.1)?;
                    self.depth_extent = drawable_extent;
                }

                let pass = self.render_pass(drawable_texture)?;

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
                objc::void_object(encoder, c"setDepthStencilState:", self.depth_state);
                objc::void_object_two_usizes(
                    encoder,
                    c"setVertexBuffer:offset:atIndex:",
                    self.vertices,
                    0,
                    0,
                );
                objc::void_object_usize(encoder, c"setFragmentTexture:atIndex:", self.texture, 0);
                objc::void_object_usize(
                    encoder,
                    c"setFragmentSamplerState:atIndex:",
                    self.sampler,
                    0,
                );
                objc::void_three_usizes_object_usize(
                    encoder,
                    c"drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:",
                    PRIMITIVE_TYPE_TRIANGLE,
                    INDICES.len(),
                    INDEX_TYPE_UINT16,
                    self.indices,
                    0,
                );
                objc::void(encoder, c"endEncoding");
                objc::void_object(command_buffer, c"presentDrawable:", drawable);
                objc::void(command_buffer, c"commit");
                Ok(true)
            }
        }

        fn render_pass(&self, drawable_texture: Object) -> Result<Object, ProbeError> {
            // SAFETY: Attachment objects are alive for the autorelease-pool scope and all selectors
            // match the Metal SDK ABI.
            unsafe {
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
                objc::void_object(color, c"setTexture:", drawable_texture);
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
                let depth = required(objc::object(pass, c"depthAttachment"), "depth attachment")?;
                objc::void_object(depth, c"setTexture:", self.depth_texture);
                objc::void_usize(depth, c"setLoadAction:", LOAD_ACTION_CLEAR);
                objc::void_usize(depth, c"setStoreAction:", STORE_ACTION_DONT_CARE);
                objc::void_f64(depth, c"setClearDepth:", 1.0);
                Ok(pass)
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

    fn create_pipelines(device: Object) -> Result<PipelineStates, ProbeError> {
        // SAFETY: Library, function, descriptor, and pipeline selectors match the Metal SDK ABI.
        unsafe {
            let library = create_library(device)?;

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
            let compute = required(
                objc::object_object(
                    library,
                    c"newFunctionWithName:",
                    objc::ns_string(c"copy_texture"),
                ),
                "compute function",
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
            objc::void_usize(
                descriptor,
                c"setDepthAttachmentPixelFormat:",
                PIXEL_FORMAT_DEPTH32_FLOAT,
            );

            let mut error = ptr::null_mut();
            let render = objc::object_object_out(
                device,
                c"newRenderPipelineStateWithDescriptor:error:",
                descriptor,
                &raw mut error,
            );
            if render.is_null() {
                return Err(ProbeError(format!(
                    "Metal render pipeline creation failed: {}",
                    objc::description(error)
                )));
            }

            error = ptr::null_mut();
            let compute = objc::object_object_out(
                device,
                c"newComputePipelineStateWithFunction:error:",
                compute,
                &raw mut error,
            );
            if compute.is_null() {
                return Err(ProbeError(format!(
                    "Metal compute pipeline creation failed: {}",
                    objc::description(error)
                )));
            }
            Ok(PipelineStates { render, compute })
        }
    }

    fn create_library(device: Object) -> Result<Object, ProbeError> {
        // DISPATCH_DATA_DESTRUCTOR_DEFAULT is null, so dispatch copies the embedded metallib bytes.
        // SAFETY: The byte slice is valid for the call and `dispatch_data_create` copies it.
        let data = unsafe {
            dispatch_data_create(
                METALLIB.as_ptr().cast::<c_void>(),
                METALLIB.len(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        let data = required(data, "embedded metallib dispatch data")?;
        let mut error = ptr::null_mut();
        // SAFETY: `data` is a dispatch_data_t and the selector matches `newLibraryWithData:error:`.
        let library = unsafe {
            objc::object_object_out(device, c"newLibraryWithData:error:", data, &raw mut error)
        };
        if library.is_null() {
            Err(ProbeError(format!(
                "loading embedded Metal library failed: {}",
                objc::description(error)
            )))
        } else {
            Ok(library)
        }
    }

    fn create_depth_state(device: Object) -> Result<Object, ProbeError> {
        // SAFETY: The descriptor setters and device constructor match the Metal SDK ABI.
        unsafe {
            let descriptor = required(
                objc::object(objc::class(c"MTLDepthStencilDescriptor"), c"new"),
                "depth-stencil descriptor",
            )?;
            objc::void_usize(
                descriptor,
                c"setDepthCompareFunction:",
                COMPARE_FUNCTION_LESS,
            );
            objc::void_bool(descriptor, c"setDepthWriteEnabled:", true);
            required(
                objc::object_object(device, c"newDepthStencilStateWithDescriptor:", descriptor),
                "depth-stencil state",
            )
        }
    }

    fn create_sampler(device: Object) -> Result<Object, ProbeError> {
        // SAFETY: The descriptor setters and device constructor match the Metal SDK ABI.
        unsafe {
            let descriptor = required(
                objc::object(objc::class(c"MTLSamplerDescriptor"), c"new"),
                "sampler descriptor",
            )?;
            objc::void_usize(descriptor, c"setMinFilter:", SAMPLER_FILTER_LINEAR);
            objc::void_usize(descriptor, c"setMagFilter:", SAMPLER_FILTER_LINEAR);
            objc::void_usize(descriptor, c"setSAddressMode:", SAMPLER_ADDRESS_REPEAT);
            objc::void_usize(descriptor, c"setTAddressMode:", SAMPLER_ADDRESS_REPEAT);
            required(
                objc::object_object(device, c"newSamplerStateWithDescriptor:", descriptor),
                "sampler state",
            )
        }
    }

    fn create_texture(
        device: Object,
        queue: Object,
        compute_pipeline: Object,
    ) -> Result<Object, ProbeError> {
        // SAFETY: Resource creation, compute dispatch, and blits use exact Metal SDK layouts and
        // ABIs. Command-buffer completion makes the shared readback bytes CPU-visible.
        unsafe {
            let source_texture =
                create_rgba_texture(device, TEXTURE_USAGE_SHADER_READ, "compute source texture")?;
            let output_texture = create_rgba_texture(
                device,
                TEXTURE_USAGE_SHADER_READ | TEXTURE_USAGE_SHADER_WRITE,
                "compute output texture",
            )?;
            let upload = required(
                objc::object_bytes(
                    device,
                    c"newBufferWithBytes:length:options:",
                    CHECKER_PIXELS.as_ptr().cast::<c_void>(),
                    CHECKER_PIXELS.len(),
                    0,
                ),
                "texture upload buffer",
            )?;
            let (readback, readback_length) = create_readback_buffer(device)?;
            let command_buffer = required(
                objc::object(queue, c"commandBuffer"),
                "texture preparation command buffer",
            )?;
            let upload_blit = required(
                objc::object(command_buffer, c"blitCommandEncoder"),
                "texture upload blit encoder",
            )?;
            objc::void_copy_buffer_to_texture(
                upload_blit,
                c"copyFromBuffer:sourceOffset:sourceBytesPerRow:sourceBytesPerImage:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:",
                upload,
                0,
                CHECKER_WIDTH * 4,
                CHECKER_PIXELS.len(),
                Size3 {
                    width: CHECKER_WIDTH,
                    height: CHECKER_HEIGHT,
                    depth: 1,
                },
                source_texture,
                0,
                0,
                Origin3::default(),
            );
            objc::void(upload_blit, c"endEncoding");

            let compute = required(
                objc::object(command_buffer, c"computeCommandEncoder"),
                "texture compute encoder",
            )?;
            objc::void_object(compute, c"setComputePipelineState:", compute_pipeline);
            objc::void_object_usize(compute, c"setTexture:atIndex:", source_texture, 0);
            objc::void_object_usize(compute, c"setTexture:atIndex:", output_texture, 1);
            objc::void_two_sizes(
                compute,
                c"dispatchThreads:threadsPerThreadgroup:",
                Size3 {
                    width: CHECKER_WIDTH,
                    height: CHECKER_HEIGHT,
                    depth: 1,
                },
                Size3 {
                    width: CHECKER_WIDTH,
                    height: CHECKER_HEIGHT,
                    depth: 1,
                },
            );
            objc::void(compute, c"endEncoding");

            let readback_blit = required(
                objc::object(command_buffer, c"blitCommandEncoder"),
                "texture readback blit encoder",
            )?;
            objc::void_copy_texture_to_buffer(
                readback_blit,
                c"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:",
                output_texture,
                0,
                0,
                Origin3::default(),
                Size3 {
                    width: CHECKER_WIDTH,
                    height: CHECKER_HEIGHT,
                    depth: 1,
                },
                readback,
                0,
                READBACK_BYTES_PER_ROW,
                readback_length,
            );
            objc::void(readback_blit, c"endEncoding");
            objc::void(command_buffer, c"commit");
            objc::void(command_buffer, c"waitUntilCompleted");
            required_command_buffer_success(
                command_buffer,
                "texture upload, compute, and readback",
            )?;
            validate_texture_readback(readback, readback_length)?;
            Ok(output_texture)
        }
    }

    fn create_readback_buffer(device: Object) -> Result<(Object, usize), ProbeError> {
        let length = READBACK_BYTES_PER_ROW * CHECKER_HEIGHT;
        // SAFETY: The selector accepts a byte length and MTLResourceOptions value.
        let buffer =
            unsafe { objc::object_two_usizes(device, c"newBufferWithLength:options:", length, 0) };
        required(buffer, "texture readback buffer").map(|buffer| (buffer, length))
    }

    fn create_rgba_texture(
        device: Object,
        usage: usize,
        label: &str,
    ) -> Result<Object, ProbeError> {
        // SAFETY: The descriptor factory and setters match the Metal SDK ABI.
        unsafe {
            let descriptor = required(
                objc::object_three_usizes_bool(
                    objc::class(c"MTLTextureDescriptor"),
                    c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
                    PIXEL_FORMAT_RGBA8_UNORM,
                    CHECKER_WIDTH,
                    CHECKER_HEIGHT,
                    false,
                ),
                "RGBA texture descriptor",
            )?;
            objc::void_usize(descriptor, c"setStorageMode:", STORAGE_MODE_PRIVATE);
            objc::void_usize(descriptor, c"setUsage:", usage);
            required(
                objc::object_object(device, c"newTextureWithDescriptor:", descriptor),
                label,
            )
        }
    }

    fn validate_texture_readback(
        readback: Object,
        readback_length: usize,
    ) -> Result<(), ProbeError> {
        // SAFETY: The command buffer completed, `contents` points to `readback_length` shared bytes,
        // and the buffer object remains alive for the duration of the slice.
        let contents = unsafe { objc::pointer_value(readback, c"contents") }.cast::<u8>();
        if contents.is_null() {
            return Err(ProbeError(
                "texture readback buffer has no CPU address".into(),
            ));
        }
        // SAFETY: The buffer was allocated with exactly `readback_length` bytes above.
        let bytes = unsafe { std::slice::from_raw_parts(contents, readback_length) };
        for y in 0..CHECKER_HEIGHT {
            for x in 0..CHECKER_WIDTH {
                let expected_offset = (y * CHECKER_WIDTH + x) * 4;
                let actual_offset = y * READBACK_BYTES_PER_ROW + x * 4;
                for channel in 0..4 {
                    let expected = CHECKER_PIXELS[expected_offset + channel];
                    let actual = bytes[actual_offset + channel];
                    if actual != expected {
                        return Err(ProbeError(format!(
                            "texture readback mismatch at ({x}, {y}) channel {channel}: expected {expected}, got {actual}"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn create_depth_texture(
        device: Object,
        width: usize,
        height: usize,
    ) -> Result<Object, ProbeError> {
        // SAFETY: The descriptor factory and setters match the Metal SDK ABI.
        unsafe {
            let descriptor = required(
                objc::object_three_usizes_bool(
                    objc::class(c"MTLTextureDescriptor"),
                    c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
                    PIXEL_FORMAT_DEPTH32_FLOAT,
                    width,
                    height,
                    false,
                ),
                "depth texture descriptor",
            )?;
            objc::void_usize(descriptor, c"setStorageMode:", STORAGE_MODE_PRIVATE);
            objc::void_usize(descriptor, c"setUsage:", TEXTURE_USAGE_RENDER_TARGET);
            required(
                objc::object_object(device, c"newTextureWithDescriptor:", descriptor),
                "depth texture",
            )
        }
    }

    fn required_command_buffer_success(
        command_buffer: Object,
        label: &str,
    ) -> Result<(), ProbeError> {
        // MTLCommandBufferStatusCompleted is 4; status 5 is an error.
        let status = unsafe { objc::usize_value(command_buffer, c"status") };
        if status == 4 {
            Ok(())
        } else {
            // SAFETY: `error` is an Objective-C object or nil after command-buffer completion.
            let error = unsafe { objc::object(command_buffer, c"error") };
            Err(ProbeError(format!(
                "{label} failed with command-buffer status {status}: {}",
                objc::description(error)
            )))
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
