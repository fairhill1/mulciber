//! Renders an indexed, textured, depth-tested scene through native Apple APIs.

#[cfg(target_os = "macos")]
mod objc;

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, CString, c_void};
    use std::fmt;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::ptr;
    use std::thread;
    use std::time::{Duration, Instant};
    use std::{env, num::NonZeroU64};

    use crate::objc::{
        self, AutoreleasePool, ClearColor, Object, Origin3, Point, Rect, Size, Size3,
    };

    const PIXEL_FORMAT_INVALID: usize = 0;
    const PIXEL_FORMAT_BGRA8_UNORM: usize = 80;
    const PIXEL_FORMAT_RGBA8_UNORM: usize = 70;
    const PIXEL_FORMAT_BC1_RGBA: usize = 130;
    const PIXEL_FORMAT_DEPTH32_FLOAT: usize = 252;
    const LOAD_ACTION_DONT_CARE: usize = 0;
    const LOAD_ACTION_CLEAR: usize = 2;
    const STORE_ACTION_STORE: usize = 1;
    const STORE_ACTION_DONT_CARE: usize = 0;
    const STORE_ACTION_MULTISAMPLE_RESOLVE: usize = 2;
    const PRIMITIVE_TYPE_TRIANGLE: usize = 3;
    const INDEX_TYPE_UINT16: usize = 0;
    const STORAGE_MODE_PRIVATE: usize = 2;
    const STORAGE_MODE_MEMORYLESS: usize = 3;
    const TEXTURE_TYPE_2D_MULTISAMPLE: usize = 4;
    const RESOURCE_STORAGE_MODE_PRIVATE: usize = STORAGE_MODE_PRIVATE << 4;
    const TEXTURE_USAGE_SHADER_READ: usize = 1;
    const TEXTURE_USAGE_SHADER_WRITE: usize = 2;
    const TEXTURE_USAGE_RENDER_TARGET: usize = 4;
    const COMPARE_FUNCTION_LESS: usize = 1;
    const SAMPLER_FILTER_LINEAR: usize = 1;
    const SAMPLER_ADDRESS_REPEAT: usize = 2;
    const OCCLUSION_STATE_VISIBLE: usize = 1 << 1;
    const CHECKER_WIDTH: usize = 8;
    const CHECKER_HEIGHT: usize = 8;
    const BC1_BLOCK_WIDTH: usize = 4;
    const BC1_BYTES_PER_BLOCK: usize = 8;
    const BC1_BYTES_PER_ROW: usize = CHECKER_WIDTH / BC1_BLOCK_WIDTH * BC1_BYTES_PER_BLOCK;
    const READBACK_BYTES_PER_ROW: usize = 256;
    const TEXTURE_READBACK_LENGTH: usize = READBACK_BYTES_PER_ROW * CHECKER_HEIGHT;
    const MIP_READBACK_OFFSET: usize = TEXTURE_READBACK_LENGTH;
    const MIP_READBACK_LENGTH: usize = READBACK_BYTES_PER_ROW;
    const STORAGE_READBACK_OFFSET: usize = MIP_READBACK_OFFSET + MIP_READBACK_LENGTH;
    const STORAGE_BUFFER_LENGTH: usize = CHECKER_WIDTH * CHECKER_HEIGHT * size_of::<u32>();
    const TOTAL_READBACK_LENGTH: usize = STORAGE_READBACK_OFFSET + STORAGE_BUFFER_LENGTH;
    const EXPECTED_MIP_TAIL: [u8; 4] = [144, 154, 148, 255];
    const IN_FLIGHT_FRAMES: usize = 3;
    const SAMPLE_COUNT: usize = 4;
    const SHADOW_MAP_SIZE: usize = 1024;
    const PIPELINE_OPTION_FAIL_ON_BINARY_ARCHIVE_MISS: usize = 1 << 2;
    const DEFAULT_BINARY_ARCHIVE_PATH: &str = "target/mulciber-metal-pipelines.metalarc";
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

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct FrameUniforms {
        offset: [f32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct IndexedIndirectArguments {
        index_count: u32,
        instance_count: u32,
        index_start: u32,
        base_vertex: i32,
        base_instance: u32,
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
    static INDIRECT_ARGUMENTS: IndexedIndirectArguments = IndexedIndirectArguments {
        index_count: 12,
        instance_count: 1,
        index_start: 0,
        base_vertex: 0,
        base_instance: 0,
    };
    const BC1_BLOCKS: [u8; 32] = bc1_blocks();
    const CHECKER_PIXELS: [u8; CHECKER_WIDTH * CHECKER_HEIGHT * 4] = checker_pixels();

    const fn bc1_blocks() -> [u8; 32] {
        const BRIGHT: u16 = (30 << 11) | (60 << 5) | 31;
        const DARK: u16 = (5 << 11) | (16 << 5) | 5;
        let mut blocks = [0; 32];
        let mut block = 0;
        while block < 4 {
            let endpoint = if block == 0 || block == 3 {
                BRIGHT
            } else {
                DARK
            };
            let offset = block * BC1_BYTES_PER_BLOCK;
            let bytes = endpoint.to_le_bytes();
            blocks[offset] = bytes[0];
            blocks[offset + 1] = bytes[1];
            block += 1;
        }
        blocks
    }

    const fn checker_pixels() -> [u8; CHECKER_WIDTH * CHECKER_HEIGHT * 4] {
        let mut pixels = [0; CHECKER_WIDTH * CHECKER_HEIGHT * 4];
        let mut y = 0;
        while y < CHECKER_HEIGHT {
            let mut x = 0;
            while x < CHECKER_WIDTH {
                let offset = (y * CHECKER_WIDTH + x) * 4;
                let bright = (x / BC1_BLOCK_WIDTH + y / BC1_BLOCK_WIDTH).is_multiple_of(2);
                pixels[offset] = if bright { 247 } else { 41 };
                pixels[offset + 1] = if bright { 243 } else { 65 };
                pixels[offset + 2] = if bright { 255 } else { 41 };
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
        shadow: Object,
        main: Object,
        post: Object,
        compute: Object,
    }

    struct PipelineDescriptors {
        shadow: Object,
        main: Object,
        post: Object,
        compute: Object,
    }

    struct BinaryArchive {
        object: Object,
        array: Object,
        url: Object,
        path: PathBuf,
        loaded: bool,
    }

    struct Options {
        frame_limit: Option<NonZeroU64>,
        binary_archive_path: PathBuf,
        rebuild_binary_archive: bool,
        abandon_acquired_frame_once: bool,
    }

    struct FrameAbandonment {
        requested: bool,
        pending: bool,
        abandoned_drawables: u32,
        submitted_after: bool,
    }

    impl FrameAbandonment {
        const fn new(requested: bool) -> Self {
            Self {
                requested,
                pending: requested,
                abandoned_drawables: 0,
                submitted_after: false,
            }
        }
    }

    #[derive(Clone, Copy)]
    struct RenderPipelineSpec {
        vertex: &'static CStr,
        fragment: Option<&'static CStr>,
        color_format: usize,
        depth_format: usize,
        sample_count: usize,
        label: &'static CStr,
    }

    struct BufferResources {
        vertices: Object,
        indices: Object,
        indirect_arguments: Object,
        uniforms: [Object; IN_FLIGHT_FRAMES],
    }

    struct TextureResources {
        source: Object,
        output: Object,
        storage: Object,
        upload: Object,
        readback: Object,
        readback_length: usize,
    }

    struct Probe {
        application: Object,
        window: Object,
        view: Object,
        device: Object,
        layer: Object,
        queue: Object,
        shadow_pipeline: Object,
        main_pipeline: Object,
        post_pipeline: Object,
        depth_state: Object,
        vertices: Object,
        indices: Object,
        indirect_arguments: Object,
        uniform_buffers: [Object; IN_FLIGHT_FRAMES],
        in_flight: [Object; IN_FLIGHT_FRAMES],
        frame_slot: usize,
        phase: f32,
        texture: Object,
        sampler: Object,
        shadow_texture: Object,
        multisample_color: Object,
        scene_color: Object,
        depth_texture: Object,
        depth_extent: (usize, usize),
        drawable_size: Size,
        gpu_time_seconds: f64,
        gpu_timed_frames: u32,
        frame_abandonment: FrameAbandonment,
    }

    impl Probe {
        fn new(options: &Options) -> Result<Self, ProbeError> {
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
                    objc::ns_string(c"Mulciber — native Metal"),
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
                set_label(queue, c"Mulciber graphics queue");
                let pipelines = create_pipelines(
                    device,
                    &options.binary_archive_path,
                    options.rebuild_binary_archive,
                )?;
                let depth_state = create_depth_state(device)?;
                let buffers = create_buffer_resources(device)?;
                let texture = create_texture(device, queue, pipelines.compute)?;
                let sampler = create_sampler(device)?;
                let shadow_texture = create_shadow_texture(device)?;

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
                    shadow_pipeline: pipelines.shadow,
                    main_pipeline: pipelines.main,
                    post_pipeline: pipelines.post,
                    depth_state,
                    vertices: buffers.vertices,
                    indices: buffers.indices,
                    indirect_arguments: buffers.indirect_arguments,
                    uniform_buffers: buffers.uniforms,
                    in_flight: [ptr::null_mut(); IN_FLIGHT_FRAMES],
                    frame_slot: 0,
                    phase: 0.0,
                    texture,
                    sampler,
                    shadow_texture,
                    multisample_color: ptr::null_mut(),
                    scene_color: ptr::null_mut(),
                    depth_texture: ptr::null_mut(),
                    depth_extent: (0, 0),
                    drawable_size: Size::default(),
                    gpu_time_seconds: 0.0,
                    gpu_timed_frames: 0,
                    frame_abandonment: FrameAbandonment::new(options.abandon_acquired_frame_once),
                })
            }
        }

        fn run(mut self, frame_limit: Option<NonZeroU64>) -> Result<(), ProbeError> {
            let render_result = self.render_loop(frame_limit);
            let lifecycle_result = render_result.and_then(|()| self.validate_frame_abandonment());
            let cleanup_result = self.finish_gpu();
            self.print_gpu_timing();
            lifecycle_result.and(cleanup_result)
        }

        fn render_loop(&mut self, frame_limit: Option<NonZeroU64>) -> Result<(), ProbeError> {
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
            Ok(())
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
                if self.frame_abandonment.pending {
                    self.frame_abandonment.pending = false;
                    self.frame_abandonment.abandoned_drawables += 1;
                    println!(
                        "abandoned one acquired Metal drawable before command-buffer submission"
                    );
                    // The drawable is autoreleased. Returning to `render_loop` drains the
                    // iteration's pool before another drawable is requested.
                    return Ok(false);
                }
                let drawable_extent = (
                    objc::usize_value(drawable_texture, c"width"),
                    objc::usize_value(drawable_texture, c"height"),
                );
                if drawable_extent != self.depth_extent {
                    self.multisample_color = create_multisample_texture(
                        self.device,
                        PIXEL_FORMAT_BGRA8_UNORM,
                        drawable_extent.0,
                        drawable_extent.1,
                        "memoryless multisample color texture",
                    )?;
                    self.depth_texture = create_multisample_texture(
                        self.device,
                        PIXEL_FORMAT_DEPTH32_FLOAT,
                        drawable_extent.0,
                        drawable_extent.1,
                        "memoryless multisample depth texture",
                    )?;
                    self.scene_color = create_scene_color_texture(
                        self.device,
                        drawable_extent.0,
                        drawable_extent.1,
                    )?;
                    self.depth_extent = drawable_extent;
                }

                let uniforms = self.prepare_uniforms()?;

                let command_buffer = required(
                    objc::object(self.queue, c"commandBuffer"),
                    "Metal command buffer",
                )?;
                set_label(command_buffer, c"Mulciber multipass frame");
                self.encode_shadow_pass(command_buffer, uniforms)?;
                self.encode_main_pass(command_buffer, uniforms)?;
                self.encode_post_pass(command_buffer, drawable_texture)?;
                objc::void_object(command_buffer, c"presentDrawable:", drawable);
                objc::void(command_buffer, c"commit");
                self.track_submission(command_buffer)?;
                if self.frame_abandonment.abandoned_drawables != 0 {
                    self.frame_abandonment.submitted_after = true;
                }
                Ok(true)
            }
        }

        fn prepare_uniforms(&mut self) -> Result<Object, ProbeError> {
            let previous = self.in_flight[self.frame_slot];
            if !previous.is_null() {
                // SAFETY: This slot retains its command buffer until it completes and is released.
                unsafe { objc::void(previous, c"waitUntilCompleted") };
                let completion = required_command_buffer_success(previous, "in-flight frame");
                if completion.is_ok()
                    && let Some(seconds) = command_buffer_gpu_time(previous)
                {
                    self.gpu_time_seconds += seconds;
                    self.gpu_timed_frames += 1;
                }
                // SAFETY: `track_submission` added exactly one retain for this slot.
                unsafe { objc::void(previous, c"release") };
                self.in_flight[self.frame_slot] = ptr::null_mut();
                completion?;
            }

            let buffer = self.uniform_buffers[self.frame_slot];
            // SAFETY: This shared buffer is no longer in use by the GPU, has FrameUniforms size, and
            // remains alive as a Probe field.
            let contents =
                unsafe { objc::pointer_value(buffer, c"contents") }.cast::<FrameUniforms>();
            if contents.is_null() {
                return Err(ProbeError("uniform buffer has no CPU address".into()));
            }
            let uniforms = FrameUniforms {
                offset: [self.phase.sin() * 0.055, 0.0, 0.0, 0.0],
            };
            // SAFETY: `contents` is non-null, aligned by Metal, and points to a full FrameUniforms.
            unsafe { contents.write(uniforms) };
            Ok(buffer)
        }

        fn track_submission(&mut self, command_buffer: Object) -> Result<(), ProbeError> {
            // SAFETY: Objective-C retain keeps this autoreleased command buffer alive across pools.
            let retained = unsafe { objc::object(command_buffer, c"retain") };
            self.in_flight[self.frame_slot] = required(retained, "retained command buffer")?;
            self.frame_slot = (self.frame_slot + 1) % IN_FLIGHT_FRAMES;
            self.phase = (self.phase + 0.025) % std::f32::consts::TAU;
            Ok(())
        }

        fn encode_shadow_pass(
            &self,
            command_buffer: Object,
            uniforms: Object,
        ) -> Result<(), ProbeError> {
            // SAFETY: The pass, resources, and pipeline share the same device and remain alive.
            unsafe {
                let encoder = required(
                    objc::object_object(
                        command_buffer,
                        c"renderCommandEncoderWithDescriptor:",
                        self.shadow_pass()?,
                    ),
                    "shadow render encoder",
                )?;
                set_label(encoder, c"Mulciber shadow pass");
                objc::void_object(encoder, c"setRenderPipelineState:", self.shadow_pipeline);
                objc::void_object(encoder, c"setDepthStencilState:", self.depth_state);
                self.bind_geometry(encoder, uniforms);
                self.draw_geometry(encoder);
                objc::void(encoder, c"endEncoding");
            }
            Ok(())
        }

        fn encode_main_pass(
            &self,
            command_buffer: Object,
            uniforms: Object,
        ) -> Result<(), ProbeError> {
            // SAFETY: The pass, resources, and pipeline share the same device and remain alive.
            unsafe {
                let encoder = required(
                    objc::object_object(
                        command_buffer,
                        c"renderCommandEncoderWithDescriptor:",
                        self.main_pass()?,
                    ),
                    "main render encoder",
                )?;
                set_label(encoder, c"Mulciber main pass");
                objc::void_object(encoder, c"setRenderPipelineState:", self.main_pipeline);
                objc::void_object(encoder, c"setDepthStencilState:", self.depth_state);
                self.bind_geometry(encoder, uniforms);
                objc::void_object_usize(encoder, c"setFragmentTexture:atIndex:", self.texture, 0);
                objc::void_object_usize(
                    encoder,
                    c"setFragmentTexture:atIndex:",
                    self.shadow_texture,
                    1,
                );
                objc::void_object_usize(
                    encoder,
                    c"setFragmentSamplerState:atIndex:",
                    self.sampler,
                    0,
                );
                self.draw_geometry(encoder);
                objc::void(encoder, c"endEncoding");
            }
            Ok(())
        }

        fn encode_post_pass(
            &self,
            command_buffer: Object,
            drawable_texture: Object,
        ) -> Result<(), ProbeError> {
            // SAFETY: The post pipeline is single-sampled and the full-screen draw needs no buffers.
            unsafe {
                let encoder = required(
                    objc::object_object(
                        command_buffer,
                        c"renderCommandEncoderWithDescriptor:",
                        Self::post_pass(drawable_texture)?,
                    ),
                    "post render encoder",
                )?;
                set_label(encoder, c"Mulciber post-process pass");
                objc::void_object(encoder, c"setRenderPipelineState:", self.post_pipeline);
                objc::void_object_usize(
                    encoder,
                    c"setFragmentTexture:atIndex:",
                    self.scene_color,
                    0,
                );
                objc::void_three_usizes(
                    encoder,
                    c"drawPrimitives:vertexStart:vertexCount:",
                    PRIMITIVE_TYPE_TRIANGLE,
                    0,
                    3,
                );
                objc::void(encoder, c"endEncoding");
            }
            Ok(())
        }

        unsafe fn bind_geometry(&self, encoder: Object, uniforms: Object) {
            unsafe {
                objc::void_object_two_usizes(
                    encoder,
                    c"setVertexBuffer:offset:atIndex:",
                    self.vertices,
                    0,
                    0,
                );
                objc::void_object_two_usizes(
                    encoder,
                    c"setVertexBuffer:offset:atIndex:",
                    uniforms,
                    0,
                    1,
                );
            }
        }

        unsafe fn draw_geometry(&self, encoder: Object) {
            unsafe {
                objc::void_two_usizes_object_usize_object_usize(
                    encoder,
                    c"drawIndexedPrimitives:indexType:indexBuffer:indexBufferOffset:indirectBuffer:indirectBufferOffset:",
                    PRIMITIVE_TYPE_TRIANGLE,
                    INDEX_TYPE_UINT16,
                    self.indices,
                    0,
                    self.indirect_arguments,
                    0,
                );
            }
        }

        fn shadow_pass(&self) -> Result<Object, ProbeError> {
            // SAFETY: Attachment objects remain alive and selectors match the Metal SDK ABI.
            unsafe {
                let pass = required(
                    objc::object(
                        objc::class(c"MTLRenderPassDescriptor"),
                        c"renderPassDescriptor",
                    ),
                    "shadow pass descriptor",
                )?;
                let depth = required(objc::object(pass, c"depthAttachment"), "depth attachment")?;
                objc::void_object(depth, c"setTexture:", self.shadow_texture);
                objc::void_usize(depth, c"setLoadAction:", LOAD_ACTION_CLEAR);
                objc::void_usize(depth, c"setStoreAction:", STORE_ACTION_STORE);
                objc::void_f64(depth, c"setClearDepth:", 1.0);
                Ok(pass)
            }
        }

        fn main_pass(&self) -> Result<Object, ProbeError> {
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
                objc::void_object(color, c"setTexture:", self.multisample_color);
                objc::void_object(color, c"setResolveTexture:", self.scene_color);
                objc::void_usize(color, c"setLoadAction:", LOAD_ACTION_CLEAR);
                objc::void_usize(color, c"setStoreAction:", STORE_ACTION_MULTISAMPLE_RESOLVE);
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

        fn post_pass(drawable_texture: Object) -> Result<Object, ProbeError> {
            // SAFETY: The drawable texture is valid through command-buffer presentation.
            unsafe {
                let pass = required(
                    objc::object(
                        objc::class(c"MTLRenderPassDescriptor"),
                        c"renderPassDescriptor",
                    ),
                    "post pass descriptor",
                )?;
                let attachments = required(
                    objc::object(pass, c"colorAttachments"),
                    "post color attachments",
                )?;
                let color = required(
                    objc::object_usize(attachments, c"objectAtIndexedSubscript:", 0),
                    "post color attachment",
                )?;
                objc::void_object(color, c"setTexture:", drawable_texture);
                objc::void_usize(color, c"setLoadAction:", LOAD_ACTION_DONT_CARE);
                objc::void_usize(color, c"setStoreAction:", STORE_ACTION_STORE);
                Ok(pass)
            }
        }

        fn finish_gpu(&mut self) -> Result<(), ProbeError> {
            let mut first_error = None;
            let mut gpu_time_seconds = 0.0;
            let mut gpu_timed_frames = 0;
            for command_buffer in &mut self.in_flight {
                if command_buffer.is_null() {
                    continue;
                }
                // SAFETY: Each slot owns one retained, committed command buffer.
                unsafe { objc::void(*command_buffer, c"waitUntilCompleted") };
                let completion = required_command_buffer_success(*command_buffer, "frame shutdown");
                if completion.is_ok()
                    && let Some(seconds) = command_buffer_gpu_time(*command_buffer)
                {
                    gpu_time_seconds += seconds;
                    gpu_timed_frames += 1;
                }
                // SAFETY: The slot's retain is balanced after GPU completion.
                unsafe { objc::void(*command_buffer, c"release") };
                *command_buffer = ptr::null_mut();
                if first_error.is_none()
                    && let Err(error) = completion
                {
                    first_error = Some(error);
                }
            }
            self.gpu_time_seconds += gpu_time_seconds;
            self.gpu_timed_frames += gpu_timed_frames;
            first_error.map_or(Ok(()), Err)
        }

        fn print_gpu_timing(&self) {
            if self.gpu_timed_frames == 0 {
                return;
            }
            let average_ms = self.gpu_time_seconds * 1_000.0 / f64::from(self.gpu_timed_frames);
            println!(
                "GPU frame time: {average_ms:.3} ms average over {} frames",
                self.gpu_timed_frames
            );
        }

        fn validate_frame_abandonment(&self) -> Result<(), ProbeError> {
            if !self.frame_abandonment.requested {
                return Ok(());
            }
            if self.frame_abandonment.abandoned_drawables != 1 {
                return Err(ProbeError(format!(
                    "requested one acquired-frame abandonment, observed {}",
                    self.frame_abandonment.abandoned_drawables
                )));
            }
            if !self.frame_abandonment.submitted_after {
                return Err(ProbeError(
                    "no frame was submitted after the acquired drawable was abandoned".into(),
                ));
            }
            println!(
                "acquired-frame abandonment recovered: one drawable abandoned and later rendering submitted"
            );
            Ok(())
        }
    }

    fn create_pipelines(
        device: Object,
        archive_path: &Path,
        rebuild_archive: bool,
    ) -> Result<PipelineStates, ProbeError> {
        let library = create_library(device)?;
        let archive = create_binary_archive(device, archive_path, rebuild_archive)?;
        let descriptors = create_pipeline_descriptors(library, archive.array)?;
        if !archive.loaded {
            populate_binary_archive(archive.object, &descriptors)?;
            serialize_binary_archive(archive.object, archive.url, &archive.path)?;
        }

        let start = Instant::now();
        let shadow =
            create_render_pipeline(device, descriptors.shadow, c"Mulciber shadow pipeline")?;
        let main = create_render_pipeline(device, descriptors.main, c"Mulciber main pipeline")?;
        let post = create_render_pipeline(device, descriptors.post, c"Mulciber post pipeline")?;
        let compute = create_compute_pipeline(device, descriptors.compute)?;
        let pipeline_creation_ms = start.elapsed().as_secs_f64() * 1_000.0;
        let action = if archive.loaded {
            "loaded"
        } else {
            "generated"
        };
        println!(
            "Metal binary archive: {action} {} (4 strict hits, pipeline creation {pipeline_creation_ms:.3} ms)",
            archive.path.display()
        );
        Ok(PipelineStates {
            shadow,
            main,
            post,
            compute,
        })
    }

    fn create_pipeline_descriptors(
        library: Object,
        binary_archives: Object,
    ) -> Result<PipelineDescriptors, ProbeError> {
        let shadow = create_render_pipeline_descriptor(
            library,
            RenderPipelineSpec {
                vertex: c"shadow_vertex",
                fragment: None,
                color_format: PIXEL_FORMAT_INVALID,
                depth_format: PIXEL_FORMAT_DEPTH32_FLOAT,
                sample_count: 1,
                label: c"Mulciber shadow pipeline",
            },
            binary_archives,
        )?;
        let main = create_render_pipeline_descriptor(
            library,
            RenderPipelineSpec {
                vertex: c"vertex_main",
                fragment: Some(c"fragment_main"),
                color_format: PIXEL_FORMAT_BGRA8_UNORM,
                depth_format: PIXEL_FORMAT_DEPTH32_FLOAT,
                sample_count: SAMPLE_COUNT,
                label: c"Mulciber main pipeline",
            },
            binary_archives,
        )?;
        let post = create_render_pipeline_descriptor(
            library,
            RenderPipelineSpec {
                vertex: c"post_vertex",
                fragment: Some(c"post_fragment"),
                color_format: PIXEL_FORMAT_BGRA8_UNORM,
                depth_format: PIXEL_FORMAT_INVALID,
                sample_count: 1,
                label: c"Mulciber post pipeline",
            },
            binary_archives,
        )?;
        // SAFETY: The descriptor setters match the Metal SDK ABI and the archive array remains alive
        // through pipeline creation.
        let compute = unsafe {
            let descriptor = required(
                objc::object(objc::class(c"MTLComputePipelineDescriptor"), c"new"),
                "compute pipeline descriptor",
            )?;
            set_label(descriptor, c"Mulciber texture compute pipeline");
            objc::void_object(
                descriptor,
                c"setComputeFunction:",
                load_function(library, c"copy_texture")?,
            );
            objc::void_object(descriptor, c"setBinaryArchives:", binary_archives);
            descriptor
        };
        Ok(PipelineDescriptors {
            shadow,
            main,
            post,
            compute,
        })
    }

    fn create_render_pipeline_descriptor(
        library: Object,
        spec: RenderPipelineSpec,
        binary_archives: Object,
    ) -> Result<Object, ProbeError> {
        // SAFETY: Function and descriptor selectors match the Metal SDK ABI.
        unsafe {
            let descriptor = required(
                objc::object(objc::class(c"MTLRenderPipelineDescriptor"), c"new"),
                "render pipeline descriptor",
            )?;
            set_label(descriptor, spec.label);
            objc::void_object(
                descriptor,
                c"setVertexFunction:",
                load_function(library, spec.vertex)?,
            );
            if let Some(fragment) = spec.fragment {
                objc::void_object(
                    descriptor,
                    c"setFragmentFunction:",
                    load_function(library, fragment)?,
                );
            }
            if spec.color_format != PIXEL_FORMAT_INVALID {
                let attachments = required(
                    objc::object(descriptor, c"colorAttachments"),
                    "pipeline color attachments",
                )?;
                let color = required(
                    objc::object_usize(attachments, c"objectAtIndexedSubscript:", 0),
                    "pipeline color attachment",
                )?;
                objc::void_usize(color, c"setPixelFormat:", spec.color_format);
            }
            objc::void_usize(
                descriptor,
                c"setDepthAttachmentPixelFormat:",
                spec.depth_format,
            );
            objc::void_usize(descriptor, c"setRasterSampleCount:", spec.sample_count);
            objc::void_object(descriptor, c"setBinaryArchives:", binary_archives);
            Ok(descriptor)
        }
    }

    fn create_render_pipeline(
        device: Object,
        descriptor: Object,
        label: &CStr,
    ) -> Result<Object, ProbeError> {
        let mut error = ptr::null_mut();
        // SAFETY: The descriptor includes the binary archive and the strict option is available on
        // Mulciber's macOS 13 baseline. Reflection is intentionally not requested.
        let pipeline = unsafe {
            objc::object_object_usize_two_out(
                device,
                c"newRenderPipelineStateWithDescriptor:options:reflection:error:",
                descriptor,
                PIPELINE_OPTION_FAIL_ON_BINARY_ARCHIVE_MISS,
                ptr::null_mut(),
                &raw mut error,
            )
        };
        if pipeline.is_null() {
            Err(ProbeError(format!(
                "Metal render pipeline creation failed for {label:?}; the binary archive may need --rebuild-binary-archive: {}",
                objc::description(error)
            )))
        } else {
            Ok(pipeline)
        }
    }

    fn create_compute_pipeline(device: Object, descriptor: Object) -> Result<Object, ProbeError> {
        let mut error = ptr::null_mut();
        // SAFETY: The descriptor includes the binary archive and reflection is intentionally absent.
        let pipeline = unsafe {
            objc::object_object_usize_two_out(
                device,
                c"newComputePipelineStateWithDescriptor:options:reflection:error:",
                descriptor,
                PIPELINE_OPTION_FAIL_ON_BINARY_ARCHIVE_MISS,
                ptr::null_mut(),
                &raw mut error,
            )
        };
        if pipeline.is_null() {
            Err(ProbeError(format!(
                "Metal compute pipeline creation failed; the binary archive may need --rebuild-binary-archive: {}",
                objc::description(error)
            )))
        } else {
            Ok(pipeline)
        }
    }

    fn create_binary_archive(
        device: Object,
        path: &Path,
        rebuild: bool,
    ) -> Result<BinaryArchive, ProbeError> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            env::current_dir()
                .map_err(|error| ProbeError(format!("could not resolve archive path: {error}")))?
                .join(path)
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                ProbeError(format!(
                    "could not create binary archive directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        if rebuild && path.exists() {
            fs::remove_file(&path).map_err(|error| {
                ProbeError(format!(
                    "could not remove binary archive {}: {error}",
                    path.display()
                ))
            })?;
        }
        let loaded = path.exists();
        let url = file_url(&path)?;
        let mut error = ptr::null_mut();
        // SAFETY: Descriptor, URL, archive constructor, and NSArray factory match Foundation/Metal
        // ABIs. A nil descriptor URL creates an empty archive.
        unsafe {
            let descriptor = required(
                objc::object(objc::class(c"MTLBinaryArchiveDescriptor"), c"new"),
                "binary archive descriptor",
            )?;
            if loaded {
                objc::void_object(descriptor, c"setUrl:", url);
            }
            let object = objc::object_object_out(
                device,
                c"newBinaryArchiveWithDescriptor:error:",
                descriptor,
                &raw mut error,
            );
            if object.is_null() {
                return Err(ProbeError(format!(
                    "could not {} Metal binary archive {}{}: {}",
                    if loaded { "load" } else { "create" },
                    path.display(),
                    if loaded {
                        "; use --rebuild-binary-archive if it is stale or corrupt"
                    } else {
                        ""
                    },
                    objc::description(error)
                )));
            }
            set_label(object, c"Mulciber pipeline binary archive");
            let array = required(
                objc::object_object(objc::class(c"NSArray"), c"arrayWithObject:", object),
                "binary archive array",
            )?;
            Ok(BinaryArchive {
                object,
                array,
                url,
                path,
                loaded,
            })
        }
    }

    fn file_url(path: &Path) -> Result<Object, ProbeError> {
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            ProbeError(format!(
                "binary archive path contains a NUL byte: {}",
                path.display()
            ))
        })?;
        // SAFETY: `fileURLWithPath:` accepts the temporary NSString and returns an autoreleased URL.
        unsafe {
            required(
                objc::object_object(
                    objc::class(c"NSURL"),
                    c"fileURLWithPath:",
                    objc::ns_string(&path),
                ),
                "binary archive file URL",
            )
        }
    }

    fn populate_binary_archive(
        archive: Object,
        descriptors: &PipelineDescriptors,
    ) -> Result<(), ProbeError> {
        add_pipeline_functions(
            archive,
            c"addRenderPipelineFunctionsWithDescriptor:error:",
            descriptors.shadow,
            "shadow render pipeline",
        )?;
        add_pipeline_functions(
            archive,
            c"addRenderPipelineFunctionsWithDescriptor:error:",
            descriptors.main,
            "main render pipeline",
        )?;
        add_pipeline_functions(
            archive,
            c"addRenderPipelineFunctionsWithDescriptor:error:",
            descriptors.post,
            "post render pipeline",
        )?;
        add_pipeline_functions(
            archive,
            c"addComputePipelineFunctionsWithDescriptor:error:",
            descriptors.compute,
            "texture compute pipeline",
        )
    }

    fn add_pipeline_functions(
        archive: Object,
        selector: &CStr,
        descriptor: Object,
        label: &str,
    ) -> Result<(), ProbeError> {
        let mut error = ptr::null_mut();
        // SAFETY: Both archive addition methods share the `(descriptor, NSError **) -> BOOL` ABI.
        let added = unsafe { objc::bool_object_out(archive, selector, descriptor, &raw mut error) };
        if added {
            Ok(())
        } else {
            Err(ProbeError(format!(
                "could not add {label} to Metal binary archive: {}",
                objc::description(error)
            )))
        }
    }

    fn serialize_binary_archive(
        archive: Object,
        url: Object,
        path: &Path,
    ) -> Result<(), ProbeError> {
        let mut error = ptr::null_mut();
        // SAFETY: `serializeToURL:error:` accepts the file URL and NSError output pointer.
        let serialized = unsafe {
            objc::bool_object_out(archive, c"serializeToURL:error:", url, &raw mut error)
        };
        if serialized {
            Ok(())
        } else {
            Err(ProbeError(format!(
                "could not serialize Metal binary archive {}: {}",
                path.display(),
                objc::description(error)
            )))
        }
    }

    fn load_function(library: Object, name: &CStr) -> Result<Object, ProbeError> {
        // SAFETY: `newFunctionWithName:` accepts NSString and returns a retained Metal function.
        let function =
            unsafe { objc::object_object(library, c"newFunctionWithName:", objc::ns_string(name)) };
        required(function, "offline Metal function")
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
            objc::void_usize(descriptor, c"setMipFilter:", SAMPLER_FILTER_LINEAR);
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
            let TextureResources {
                source: source_texture,
                output: output_texture,
                storage: storage_buffer,
                upload,
                readback,
                readback_length,
            } = create_texture_resources(device)?;
            let command_buffer = required(
                objc::object(queue, c"commandBuffer"),
                "texture preparation command buffer",
            )?;
            set_label(command_buffer, c"Mulciber texture preparation");
            let upload_blit = required(
                objc::object(command_buffer, c"blitCommandEncoder"),
                "texture upload blit encoder",
            )?;
            set_label(upload_blit, c"Mulciber BC1 upload");
            objc::void_copy_buffer_to_texture(
                upload_blit,
                c"copyFromBuffer:sourceOffset:sourceBytesPerRow:sourceBytesPerImage:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:",
                upload,
                0,
                BC1_BYTES_PER_ROW,
                BC1_BLOCKS.len(),
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
            set_label(compute, c"Mulciber texture decompression");
            objc::void_object(compute, c"setComputePipelineState:", compute_pipeline);
            objc::void_object_usize(compute, c"setTexture:atIndex:", source_texture, 0);
            objc::void_object_usize(compute, c"setTexture:atIndex:", output_texture, 1);
            objc::void_object_two_usizes(
                compute,
                c"setBuffer:offset:atIndex:",
                storage_buffer,
                0,
                0,
            );
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

            encode_texture_readback(command_buffer, output_texture, storage_buffer, readback)?;
            finish_texture_preparation(command_buffer, readback, readback_length)?;
            Ok(output_texture)
        }
    }

    fn encode_texture_readback(
        command_buffer: Object,
        texture: Object,
        storage: Object,
        readback: Object,
    ) -> Result<(), ProbeError> {
        // SAFETY: All resources remain alive through command completion and copy layouts match their
        // allocated ranges.
        unsafe {
            let blit = required(
                objc::object(command_buffer, c"blitCommandEncoder"),
                "texture readback blit encoder",
            )?;
            set_label(blit, c"Mulciber mip generation and readback");
            objc::void_object(blit, c"generateMipmapsForTexture:", texture);
            objc::void_copy_texture_to_buffer(
                blit,
                c"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:",
                texture,
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
                TEXTURE_READBACK_LENGTH,
            );
            objc::void_copy_texture_to_buffer(
                blit,
                c"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:",
                texture,
                0,
                3,
                Origin3::default(),
                Size3 {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
                readback,
                MIP_READBACK_OFFSET,
                READBACK_BYTES_PER_ROW,
                MIP_READBACK_LENGTH,
            );
            objc::void_copy_buffer(
                blit,
                c"copyFromBuffer:sourceOffset:toBuffer:destinationOffset:size:",
                storage,
                0,
                readback,
                STORAGE_READBACK_OFFSET,
                STORAGE_BUFFER_LENGTH,
            );
            objc::void(blit, c"endEncoding");
        }
        Ok(())
    }

    fn finish_texture_preparation(
        command_buffer: Object,
        readback: Object,
        readback_length: usize,
    ) -> Result<(), ProbeError> {
        // SAFETY: All encoders have ended and the command buffer has not previously been committed.
        unsafe {
            objc::void(command_buffer, c"commit");
            objc::void(command_buffer, c"waitUntilCompleted");
        }
        required_command_buffer_success(command_buffer, "texture and buffer preparation")?;
        validate_texture_readback(readback, readback_length)
    }

    fn create_texture_resources(device: Object) -> Result<TextureResources, ProbeError> {
        // SAFETY: This macOS 11+ device property returns Objective-C BOOL.
        if !unsafe { objc::bool_value(device, c"supportsBCTextureCompression") } {
            return Err(ProbeError(
                "the Metal device does not support required BC texture compression".into(),
            ));
        }
        let source = create_bc1_texture(device)?;
        let output = create_rgba_texture(
            device,
            TEXTURE_USAGE_SHADER_READ | TEXTURE_USAGE_SHADER_WRITE,
            true,
            "compute output texture",
        )?;
        // SAFETY: Buffer constructors accept the given lengths, byte pointers, and options.
        let storage = unsafe {
            objc::object_two_usizes(
                device,
                c"newBufferWithLength:options:",
                STORAGE_BUFFER_LENGTH,
                RESOURCE_STORAGE_MODE_PRIVATE,
            )
        };
        let storage = required(storage, "private compute storage buffer")?;
        set_label(storage, c"Mulciber compute storage output");
        let upload = unsafe {
            objc::object_bytes(
                device,
                c"newBufferWithBytes:length:options:",
                BC1_BLOCKS.as_ptr().cast::<c_void>(),
                BC1_BLOCKS.len(),
                0,
            )
        };
        let upload = required(upload, "texture upload buffer")?;
        set_label(upload, c"Mulciber BC1 staging buffer");
        let (readback, readback_length) = create_readback_buffer(device)?;
        set_label(readback, c"Mulciber texture and storage readback");
        set_label(source, c"Mulciber BC1 source");
        set_label(output, c"Mulciber computed mipmapped texture");
        Ok(TextureResources {
            source,
            output,
            storage,
            upload,
            readback,
            readback_length,
        })
    }

    fn create_readback_buffer(device: Object) -> Result<(Object, usize), ProbeError> {
        let length = TOTAL_READBACK_LENGTH;
        // SAFETY: The selector accepts a byte length and MTLResourceOptions value.
        let buffer =
            unsafe { objc::object_two_usizes(device, c"newBufferWithLength:options:", length, 0) };
        required(buffer, "texture readback buffer").map(|buffer| (buffer, length))
    }

    fn create_buffer_resources(device: Object) -> Result<BufferResources, ProbeError> {
        // SAFETY: Each constructor copies the provided bytes into a new Metal buffer.
        unsafe {
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
            let indirect_arguments = required(
                objc::object_bytes(
                    device,
                    c"newBufferWithBytes:length:options:",
                    (&raw const INDIRECT_ARGUMENTS).cast::<c_void>(),
                    size_of::<IndexedIndirectArguments>(),
                    0,
                ),
                "Metal indexed-indirect argument buffer",
            )?;
            set_label(vertices, c"Mulciber scene vertices");
            set_label(indices, c"Mulciber scene indices");
            set_label(indirect_arguments, c"Mulciber indexed-indirect arguments");
            let uniforms = create_uniform_buffers(device)?;
            Ok(BufferResources {
                vertices,
                indices,
                indirect_arguments,
                uniforms,
            })
        }
    }

    fn create_uniform_buffers(device: Object) -> Result<[Object; IN_FLIGHT_FRAMES], ProbeError> {
        let mut buffers = [ptr::null_mut(); IN_FLIGHT_FRAMES];
        for buffer in &mut buffers {
            // SAFETY: The selector accepts a byte length and MTLResourceOptions value. Zero selects
            // shared storage on Apple silicon, making the contents CPU-updatable.
            let value = unsafe {
                objc::object_two_usizes(
                    device,
                    c"newBufferWithLength:options:",
                    size_of::<FrameUniforms>(),
                    0,
                )
            };
            *buffer = required(value, "per-frame uniform buffer")?;
            set_label(*buffer, c"Mulciber per-frame uniforms");
        }
        Ok(buffers)
    }

    fn create_rgba_texture(
        device: Object,
        usage: usize,
        mipmapped: bool,
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
                    mipmapped,
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

    fn create_bc1_texture(device: Object) -> Result<Object, ProbeError> {
        // SAFETY: BC1 support is queried before this descriptor is created.
        unsafe {
            let descriptor = required(
                objc::object_three_usizes_bool(
                    objc::class(c"MTLTextureDescriptor"),
                    c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
                    PIXEL_FORMAT_BC1_RGBA,
                    CHECKER_WIDTH,
                    CHECKER_HEIGHT,
                    false,
                ),
                "BC1 texture descriptor",
            )?;
            objc::void_usize(descriptor, c"setStorageMode:", STORAGE_MODE_PRIVATE);
            objc::void_usize(descriptor, c"setUsage:", TEXTURE_USAGE_SHADER_READ);
            required(
                objc::object_object(device, c"newTextureWithDescriptor:", descriptor),
                "private BC1 source texture",
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

                let storage_offset = STORAGE_READBACK_OFFSET + expected_offset;
                let actual_word = u32::from_le_bytes(
                    bytes[storage_offset..storage_offset + 4]
                        .try_into()
                        .expect("four-byte storage word"),
                );
                let expected_word = u32::from_le_bytes(
                    CHECKER_PIXELS[expected_offset..expected_offset + 4]
                        .try_into()
                        .expect("four-byte checker texel"),
                );
                if actual_word != expected_word {
                    return Err(ProbeError(format!(
                        "storage buffer mismatch at ({x}, {y}): expected {expected_word:#010x}, got {actual_word:#010x}"
                    )));
                }
            }
        }

        let mip_tail = &bytes[MIP_READBACK_OFFSET..MIP_READBACK_OFFSET + 4];
        if mip_tail != EXPECTED_MIP_TAIL {
            return Err(ProbeError(format!(
                "mip-tail readback mismatch: expected {EXPECTED_MIP_TAIL:?}, got {mip_tail:?}"
            )));
        }
        Ok(())
    }

    fn create_multisample_texture(
        device: Object,
        pixel_format: usize,
        width: usize,
        height: usize,
        label: &str,
    ) -> Result<Object, ProbeError> {
        // SAFETY: The descriptor factory and setters match the Metal SDK ABI. Memoryless storage is
        // part of Mulciber's Apple-silicon/macOS 13 baseline for transient render attachments.
        unsafe {
            let descriptor = required(
                objc::object_three_usizes_bool(
                    objc::class(c"MTLTextureDescriptor"),
                    c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
                    pixel_format,
                    width,
                    height,
                    false,
                ),
                "multisample texture descriptor",
            )?;
            objc::void_usize(descriptor, c"setTextureType:", TEXTURE_TYPE_2D_MULTISAMPLE);
            objc::void_usize(descriptor, c"setSampleCount:", SAMPLE_COUNT);
            objc::void_usize(descriptor, c"setStorageMode:", STORAGE_MODE_MEMORYLESS);
            objc::void_usize(descriptor, c"setUsage:", TEXTURE_USAGE_RENDER_TARGET);
            let texture = required(
                objc::object_object(device, c"newTextureWithDescriptor:", descriptor),
                label,
            )?;
            if pixel_format == PIXEL_FORMAT_DEPTH32_FLOAT {
                set_label(texture, c"Mulciber transient MSAA depth");
            } else {
                set_label(texture, c"Mulciber transient MSAA color");
            }
            Ok(texture)
        }
    }

    fn create_scene_color_texture(
        device: Object,
        width: usize,
        height: usize,
    ) -> Result<Object, ProbeError> {
        // SAFETY: This private texture is resolved into, then sampled by the next encoder.
        unsafe {
            let descriptor = required(
                objc::object_three_usizes_bool(
                    objc::class(c"MTLTextureDescriptor"),
                    c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
                    PIXEL_FORMAT_BGRA8_UNORM,
                    width,
                    height,
                    false,
                ),
                "scene color descriptor",
            )?;
            objc::void_usize(descriptor, c"setStorageMode:", STORAGE_MODE_PRIVATE);
            objc::void_usize(
                descriptor,
                c"setUsage:",
                TEXTURE_USAGE_RENDER_TARGET | TEXTURE_USAGE_SHADER_READ,
            );
            let texture = required(
                objc::object_object(device, c"newTextureWithDescriptor:", descriptor),
                "scene color texture",
            )?;
            set_label(texture, c"Mulciber resolved scene color");
            Ok(texture)
        }
    }

    fn create_shadow_texture(device: Object) -> Result<Object, ProbeError> {
        // SAFETY: The depth texture is rendered once per frame and sampled by the following pass.
        unsafe {
            let descriptor = required(
                objc::object_three_usizes_bool(
                    objc::class(c"MTLTextureDescriptor"),
                    c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
                    PIXEL_FORMAT_DEPTH32_FLOAT,
                    SHADOW_MAP_SIZE,
                    SHADOW_MAP_SIZE,
                    false,
                ),
                "shadow texture descriptor",
            )?;
            objc::void_usize(descriptor, c"setStorageMode:", STORAGE_MODE_PRIVATE);
            objc::void_usize(
                descriptor,
                c"setUsage:",
                TEXTURE_USAGE_RENDER_TARGET | TEXTURE_USAGE_SHADER_READ,
            );
            let texture = required(
                objc::object_object(device, c"newTextureWithDescriptor:", descriptor),
                "shadow depth texture",
            )?;
            set_label(texture, c"Mulciber reusable shadow depth");
            Ok(texture)
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

    fn command_buffer_gpu_time(command_buffer: Object) -> Option<f64> {
        // SAFETY: These read-only timestamp properties are available on the macOS 13 baseline and
        // are queried only after command-buffer completion.
        let start = unsafe { objc::f64_value(command_buffer, c"GPUStartTime") };
        let end = unsafe { objc::f64_value(command_buffer, c"GPUEndTime") };
        (start > 0.0 && end >= start).then_some(end - start)
    }

    fn set_label(object: Object, label: &CStr) {
        // SAFETY: Metal and descriptor objects implement `setLabel:` with NSString input.
        unsafe { objc::void_object(object, c"setLabel:", objc::ns_string(label)) };
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
        let options = parse_options()?;
        Probe::new(&options)?.run(options.frame_limit)
    }

    fn parse_options() -> Result<Options, ProbeError> {
        let mut frame_limit = None;
        let mut binary_archive_path = None;
        let mut rebuild_binary_archive = false;
        let mut abandon_acquired_frame_once = false;
        let mut arguments = env::args_os().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--frames") => {
                    if frame_limit.is_some() {
                        return Err(ProbeError("--frames was provided more than once".into()));
                    }
                    let value = arguments
                        .next()
                        .ok_or_else(|| ProbeError("--frames requires a positive integer".into()))?;
                    let value = value
                        .to_string_lossy()
                        .parse::<NonZeroU64>()
                        .map_err(|error| ProbeError(format!("invalid --frames value: {error}")))?;
                    frame_limit = Some(value);
                }
                Some("--binary-archive") => {
                    if binary_archive_path.is_some() {
                        return Err(ProbeError(
                            "--binary-archive was provided more than once".into(),
                        ));
                    }
                    binary_archive_path =
                        Some(PathBuf::from(arguments.next().ok_or_else(|| {
                            ProbeError("--binary-archive requires a file path".into())
                        })?));
                }
                Some("--rebuild-binary-archive") => rebuild_binary_archive = true,
                Some("--abandon-acquired-frame-once") => {
                    if abandon_acquired_frame_once {
                        return Err(ProbeError(
                            "--abandon-acquired-frame-once was provided more than once".into(),
                        ));
                    }
                    abandon_acquired_frame_once = true;
                }
                _ => {
                    return Err(ProbeError(format!(
                        "unknown argument: {}",
                        argument.to_string_lossy()
                    )));
                }
            }
        }
        Ok(Options {
            frame_limit,
            binary_archive_path: binary_archive_path
                .unwrap_or_else(|| PathBuf::from(DEFAULT_BINARY_ARCHIVE_PATH)),
            rebuild_binary_archive,
            abandon_acquired_frame_once,
        })
    }
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    macos::run().map_err(Into::into)
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("mulciber-metal-triangle is available only on macOS");
}
