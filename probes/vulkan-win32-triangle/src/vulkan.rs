use std::env;
use std::ffi::{CStr, c_char, c_void};
use std::fmt;
use std::mem;
use std::num::NonZeroU64;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::vk;
use crate::win32::Window;

const API_VERSION_1_4: u32 = make_api_version(0, 1, 4, 0);
const UINT32_MAX: u32 = u32::MAX;
const UINT64_MAX: u64 = u64::MAX;
static VALIDATION_MESSAGE_COUNT: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 3],
    uv: [f32; 2],
}

const TRIANGLE_VERTICES: [Vertex; 3] = [
    Vertex {
        position: [0.00, -0.65],
        color: [1.00, 0.20, 0.15],
        uv: [0.5, 1.0],
    },
    Vertex {
        position: [-0.62, 0.45],
        color: [0.15, 0.85, 0.35],
        uv: [0.0, 0.0],
    },
    Vertex {
        position: [0.62, 0.45],
        color: [0.20, 0.40, 1.00],
        uv: [1.0, 0.0],
    },
];
const TRIANGLE_INDICES: [u16; 3] = [0, 1, 2];
const TEXTURE_WIDTH: u32 = 4;
const TEXTURE_HEIGHT: u32 = 4;
const CHECKERBOARD_TEXELS: [u8; 64] = [
    255, 255, 255, 255, 72, 72, 72, 255, 255, 255, 255, 255, 72, 72, 72, 255, 72, 72, 72, 255, 255,
    255, 255, 255, 72, 72, 72, 255, 255, 255, 255, 255, 255, 255, 255, 255, 72, 72, 72, 255, 255,
    255, 255, 255, 72, 72, 72, 255, 72, 72, 72, 255, 255, 255, 255, 255, 72, 72, 72, 255, 255, 255,
    255, 255,
];

#[link(name = "kernel32")]
unsafe extern "system" {
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
}

#[derive(Debug)]
pub struct ProbeError(String);

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ProbeError {}

#[derive(Default)]
struct TimingSeries {
    samples: u32,
    total: Duration,
    maximum: Duration,
}

impl TimingSeries {
    fn record(&mut self, duration: Duration) {
        self.samples += 1;
        self.total += duration;
        self.maximum = self.maximum.max(duration);
    }

    fn average_ms(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            self.total.as_secs_f64() * 1_000.0 / f64::from(self.samples)
        }
    }

    fn maximum_ms(&self) -> f64 {
        self.maximum.as_secs_f64() * 1_000.0
    }
}

#[derive(Clone, Copy, Default)]
struct LiveResizeSample {
    frame_wait: Duration,
    recreate: Option<Duration>,
    acquire: Duration,
    record_submit: Duration,
    present: Duration,
}

struct LiveResizeTrace {
    enabled: bool,
    reported: bool,
    attempts: u64,
    rendered: u64,
    recreations: u64,
    last_attempt: Option<Instant>,
    callback_interval: TimingSeries,
    frame_total: TimingSeries,
    frame_wait: TimingSeries,
    recreate: TimingSeries,
    acquire: TimingSeries,
    record_submit: TimingSeries,
    present: TimingSeries,
}

impl LiveResizeTrace {
    fn from_environment() -> Self {
        Self {
            enabled: env::var_os("MULCIBER_VULKAN_RESIZE_TRACE").is_some(),
            reported: false,
            attempts: 0,
            rendered: 0,
            recreations: 0,
            last_attempt: None,
            callback_interval: TimingSeries::default(),
            frame_total: TimingSeries::default(),
            frame_wait: TimingSeries::default(),
            recreate: TimingSeries::default(),
            acquire: TimingSeries::default(),
            record_submit: TimingSeries::default(),
            present: TimingSeries::default(),
        }
    }

    fn begin(&mut self, live_resize: bool) -> Option<Instant> {
        if !self.enabled {
            return None;
        }
        if !live_resize {
            self.last_attempt = None;
            return None;
        }
        let now = Instant::now();
        if let Some(previous) = self.last_attempt.replace(now) {
            self.callback_interval.record(now.duration_since(previous));
        }
        self.attempts += 1;
        Some(now)
    }

    fn finish(&mut self, started: Option<Instant>, sample: LiveResizeSample, rendered: bool) {
        let Some(started) = started else {
            return;
        };
        if rendered {
            self.rendered += 1;
        }
        self.frame_total.record(started.elapsed());
        self.frame_wait.record(sample.frame_wait);
        if let Some(recreate) = sample.recreate {
            self.recreations += 1;
            self.recreate.record(recreate);
        }
        self.acquire.record(sample.acquire);
        self.record_submit.record(sample.record_submit);
        self.present.record(sample.present);
    }

    fn report(&mut self) {
        if !self.enabled || self.reported {
            return;
        }
        self.reported = true;
        println!(
            "Live resize trace: attempts={} rendered={} recreations={}",
            self.attempts, self.rendered, self.recreations
        );
        for (name, series) in [
            ("callback interval", &self.callback_interval),
            ("frame total", &self.frame_total),
            ("frame-fence wait", &self.frame_wait),
            ("swapchain recreation", &self.recreate),
            ("image acquisition", &self.acquire),
            ("record + submit", &self.record_submit),
            ("queue present", &self.present),
        ] {
            println!(
                "  {name}: samples={} avg={:.3} ms max={:.3} ms",
                series.samples,
                series.average_ms(),
                series.maximum_ms()
            );
        }
    }
}

pub fn run() -> Result<(), ProbeError> {
    VALIDATION_MESSAGE_COUNT.store(0, Ordering::Relaxed);
    let frame_limit = parse_frame_limit()?;
    let window = Window::new("Mulciber — native Vulkan 1.4", 960, 540, true)
        .map_err(|error| ProbeError(error.to_string()))?;
    let entry = Entry::load()?;
    let instance = InstanceContext::new(entry, &window)?;
    let device = DeviceContext::new(instance)?;
    let mut renderer = Renderer::new(device, &window)?;

    let render_result = (|| {
        let mut rendered_frames = 0;
        loop {
            let mut live_resize_error = None;
            let mut frame_limit_reached = false;
            let keep_running = window
                .pump_events(&mut || {
                    if live_resize_error.is_some() || frame_limit_reached {
                        return;
                    }
                    let result = window
                        .client_extent()
                        .map_err(|error| ProbeError(error.to_string()))
                        .and_then(|(width, height)| {
                            if width == 0 || height == 0 {
                                return Ok(false);
                            }
                            renderer.render(width, height, true)
                        });
                    match result {
                        Ok(true) => {
                            rendered_frames += 1;
                            frame_limit_reached =
                                frame_limit.is_some_and(|limit| rendered_frames >= limit.get());
                        }
                        Ok(false) => {}
                        Err(error) => live_resize_error = Some(error),
                    }
                })
                .map_err(|error| ProbeError(error.to_string()))?;
            if let Some(error) = live_resize_error {
                return Err(error);
            }
            if !keep_running || frame_limit_reached {
                break;
            }
            let (width, height) = window
                .client_extent()
                .map_err(|error| ProbeError(error.to_string()))?;
            if width == 0 || height == 0 {
                thread::sleep(Duration::from_millis(16));
                continue;
            }
            if renderer.render(width, height, false)? {
                rendered_frames += 1;
                if frame_limit.is_some_and(|limit| rendered_frames >= limit.get()) {
                    break;
                }
            }
        }
        Ok(())
    })();
    let finish_result = renderer.finish();
    drop(renderer);
    render_result.and(finish_result)?;

    let validation_messages = VALIDATION_MESSAGE_COUNT.load(Ordering::Relaxed);
    if validation_messages == 0 {
        Ok(())
    } else {
        Err(ProbeError(format!(
            "Vulkan validation reported {validation_messages} warning/error message(s)"
        )))
    }
}

const fn make_api_version(variant: u32, major: u32, minor: u32, patch: u32) -> u32 {
    (variant << 29) | (major << 22) | (minor << 12) | patch
}

fn parse_frame_limit() -> Result<Option<NonZeroU64>, ProbeError> {
    let mut arguments = env::args().skip(1);
    let Some(argument) = arguments.next() else {
        return Ok(None);
    };
    if argument != "--frames" {
        return Err(ProbeError(format!("unknown argument: {argument}")));
    }
    let count = arguments
        .next()
        .ok_or_else(|| ProbeError("--frames requires a positive integer".into()))?
        .parse::<NonZeroU64>()
        .map_err(|_| ProbeError("--frames requires a positive integer".into()))?;
    if let Some(extra) = arguments.next() {
        return Err(ProbeError(format!("unexpected argument: {extra}")));
    }
    Ok(Some(count))
}

struct Entry {
    library: *mut c_void,
    get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    enumerate_instance_version: vk::PFN_vkEnumerateInstanceVersion,
    enumerate_instance_layer_properties: vk::PFN_vkEnumerateInstanceLayerProperties,
    enumerate_instance_extension_properties: vk::PFN_vkEnumerateInstanceExtensionProperties,
    create_instance: vk::PFN_vkCreateInstance,
}

impl Entry {
    fn load() -> Result<Self, ProbeError> {
        let name: Vec<u16> = "vulkan-1.dll".encode_utf16().chain(Some(0)).collect();
        // SAFETY: The UTF-16 library name is NUL-terminated.
        let library = unsafe { LoadLibraryW(name.as_ptr()) };
        if library.is_null() {
            return Err(ProbeError(
                "could not load vulkan-1.dll; install a Vulkan 1.4 driver".into(),
            ));
        }
        // SAFETY: The loaded Vulkan loader exports vkGetInstanceProcAddr with the generated ABI.
        let get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr = unsafe {
            let address = GetProcAddress(library, c"vkGetInstanceProcAddr".as_ptr());
            cast_address(address, "vkGetInstanceProcAddr")?
        };
        let get = get_instance_proc_addr.expect("required function was checked");
        // SAFETY: Global functions are requested with a null instance as required by Vulkan.
        let enumerate_instance_version = unsafe {
            load_proc(
                get(ptr::null_mut(), c"vkEnumerateInstanceVersion".as_ptr()),
                "vkEnumerateInstanceVersion",
            )?
        };
        // SAFETY: This is a loader-global function with the generated ABI.
        let enumerate_instance_layer_properties = unsafe {
            load_proc(
                get(
                    ptr::null_mut(),
                    c"vkEnumerateInstanceLayerProperties".as_ptr(),
                ),
                "vkEnumerateInstanceLayerProperties",
            )?
        };
        // SAFETY: This is a loader-global function with the generated ABI.
        let enumerate_instance_extension_properties = unsafe {
            load_proc(
                get(
                    ptr::null_mut(),
                    c"vkEnumerateInstanceExtensionProperties".as_ptr(),
                ),
                "vkEnumerateInstanceExtensionProperties",
            )?
        };
        // SAFETY: This is a loader-global function with the generated ABI.
        let create_instance = unsafe {
            load_proc(
                get(ptr::null_mut(), c"vkCreateInstance".as_ptr()),
                "vkCreateInstance",
            )?
        };

        let entry = Self {
            library,
            get_instance_proc_addr,
            enumerate_instance_version,
            enumerate_instance_layer_properties,
            enumerate_instance_extension_properties,
            create_instance,
        };
        entry.require_version()?;
        Ok(entry)
    }

    fn require_version(&self) -> Result<(), ProbeError> {
        let mut version = 0;
        // SAFETY: The output pointer is writable and the loaded function has the generated ABI.
        check(
            unsafe { self.enumerate_instance_version.expect("loaded function")(&raw mut version) },
            "vkEnumerateInstanceVersion",
        )?;
        if version < API_VERSION_1_4 {
            return Err(ProbeError(format!(
                "Vulkan loader exposes {}.{}.{}, but Mulciber requires 1.4",
                version >> 22,
                (version >> 12) & 0x3ff,
                version & 0xfff
            )));
        }
        Ok(())
    }

    unsafe fn instance_proc<T: Copy>(
        &self,
        instance: vk::VkInstance,
        name: &CStr,
    ) -> Result<T, ProbeError> {
        let get = self.get_instance_proc_addr.expect("loaded function");
        // SAFETY: The caller supplies a live instance and the requested type matches `name`.
        unsafe {
            load_proc(
                get(instance, name.as_ptr()),
                name.to_string_lossy().as_ref(),
            )
        }
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        // SAFETY: The library was loaded by this value and all child Vulkan objects are gone.
        unsafe { FreeLibrary(self.library) };
    }
}

struct InstanceFns {
    destroy_instance: vk::PFN_vkDestroyInstance,
    create_debug_utils_messenger: vk::PFN_vkCreateDebugUtilsMessengerEXT,
    destroy_debug_utils_messenger: vk::PFN_vkDestroyDebugUtilsMessengerEXT,
    create_win32_surface: vk::PFN_vkCreateWin32SurfaceKHR,
    destroy_surface: vk::PFN_vkDestroySurfaceKHR,
    enumerate_physical_devices: vk::PFN_vkEnumeratePhysicalDevices,
    get_physical_device_properties: vk::PFN_vkGetPhysicalDeviceProperties,
    get_physical_device_format_properties: vk::PFN_vkGetPhysicalDeviceFormatProperties,
    get_physical_device_memory_properties: vk::PFN_vkGetPhysicalDeviceMemoryProperties,
    get_physical_device_features2: vk::PFN_vkGetPhysicalDeviceFeatures2,
    get_queue_family_properties: vk::PFN_vkGetPhysicalDeviceQueueFamilyProperties,
    get_surface_support: vk::PFN_vkGetPhysicalDeviceSurfaceSupportKHR,
    get_surface_capabilities: vk::PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR,
    get_surface_formats: vk::PFN_vkGetPhysicalDeviceSurfaceFormatsKHR,
    get_surface_present_modes: vk::PFN_vkGetPhysicalDeviceSurfacePresentModesKHR,
    enumerate_device_extensions: vk::PFN_vkEnumerateDeviceExtensionProperties,
    create_device: vk::PFN_vkCreateDevice,
    get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
}

impl InstanceFns {
    unsafe fn load(entry: &Entry, instance: vk::VkInstance) -> Result<Self, ProbeError> {
        macro_rules! load {
            ($name:literal) => {
                unsafe { entry.instance_proc(instance, $name) }?
            };
        }
        Ok(Self {
            destroy_instance: load!(c"vkDestroyInstance"),
            create_debug_utils_messenger: load!(c"vkCreateDebugUtilsMessengerEXT"),
            destroy_debug_utils_messenger: load!(c"vkDestroyDebugUtilsMessengerEXT"),
            create_win32_surface: load!(c"vkCreateWin32SurfaceKHR"),
            destroy_surface: load!(c"vkDestroySurfaceKHR"),
            enumerate_physical_devices: load!(c"vkEnumeratePhysicalDevices"),
            get_physical_device_properties: load!(c"vkGetPhysicalDeviceProperties"),
            get_physical_device_format_properties: load!(c"vkGetPhysicalDeviceFormatProperties"),
            get_physical_device_memory_properties: load!(c"vkGetPhysicalDeviceMemoryProperties"),
            get_physical_device_features2: load!(c"vkGetPhysicalDeviceFeatures2"),
            get_queue_family_properties: load!(c"vkGetPhysicalDeviceQueueFamilyProperties"),
            get_surface_support: load!(c"vkGetPhysicalDeviceSurfaceSupportKHR"),
            get_surface_capabilities: load!(c"vkGetPhysicalDeviceSurfaceCapabilitiesKHR"),
            get_surface_formats: load!(c"vkGetPhysicalDeviceSurfaceFormatsKHR"),
            get_surface_present_modes: load!(c"vkGetPhysicalDeviceSurfacePresentModesKHR"),
            enumerate_device_extensions: load!(c"vkEnumerateDeviceExtensionProperties"),
            create_device: load!(c"vkCreateDevice"),
            get_device_proc_addr: load!(c"vkGetDeviceProcAddr"),
        })
    }
}

struct InstanceContext {
    _entry: Entry,
    functions: InstanceFns,
    handle: vk::VkInstance,
    debug_messenger: vk::VkDebugUtilsMessengerEXT,
    surface: vk::VkSurfaceKHR,
    surface_maintenance1: bool,
}

impl InstanceContext {
    #[allow(clippy::too_many_lines)]
    fn new(entry: Entry, window: &Window) -> Result<Self, ProbeError> {
        require_name(
            &enumerate_instance_layers(&entry)?,
            c"VK_LAYER_KHRONOS_validation",
            "Vulkan validation layer",
        )?;
        let extensions = enumerate_instance_extensions(&entry)?;
        for (name, description) in [
            (c"VK_KHR_surface", "surface extension"),
            (c"VK_KHR_win32_surface", "Win32 surface extension"),
            (c"VK_EXT_debug_utils", "debug utilities extension"),
        ] {
            require_name(&extensions, name, description)?;
        }

        let application = vk::VkApplicationInfo {
            sType: vk::VK_STRUCTURE_TYPE_APPLICATION_INFO,
            pApplicationName: c"Mulciber Vulkan probe".as_ptr(),
            applicationVersion: 0,
            pEngineName: c"Mulciber".as_ptr(),
            engineVersion: 0,
            apiVersion: API_VERSION_1_4,
            ..Default::default()
        };
        let layers = [c"VK_LAYER_KHRONOS_validation".as_ptr()];
        let has_extension = |name: &'static [u8]| {
            extensions
                .iter()
                .any(|candidate| candidate == name.strip_suffix(&[0]).unwrap())
        };
        let surface_maintenance1 =
            has_extension(vk::VK_KHR_GET_SURFACE_CAPABILITIES_2_EXTENSION_NAME)
                && has_extension(vk::VK_KHR_SURFACE_MAINTENANCE_1_EXTENSION_NAME);
        let mut extensions = vec![
            c"VK_KHR_surface".as_ptr(),
            c"VK_KHR_win32_surface".as_ptr(),
            c"VK_EXT_debug_utils".as_ptr(),
        ];
        if surface_maintenance1 {
            extensions.push(
                vk::VK_KHR_GET_SURFACE_CAPABILITIES_2_EXTENSION_NAME
                    .as_ptr()
                    .cast(),
            );
            extensions.push(
                vk::VK_KHR_SURFACE_MAINTENANCE_1_EXTENSION_NAME
                    .as_ptr()
                    .cast(),
            );
        }
        let debug_info = debug_messenger_info();
        let create_info = vk::VkInstanceCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
            pNext: (&raw const debug_info).cast(),
            pApplicationInfo: &raw const application,
            enabledLayerCount: u32::try_from(layers.len()).expect("layer count fits u32"),
            ppEnabledLayerNames: layers.as_ptr(),
            enabledExtensionCount: u32::try_from(extensions.len())
                .expect("extension count fits u32"),
            ppEnabledExtensionNames: extensions.as_ptr(),
            ..Default::default()
        };
        let mut handle = ptr::null_mut();
        // SAFETY: All create-info pointers remain live for this call.
        check(
            unsafe {
                entry.create_instance.expect("loaded function")(
                    &raw const create_info,
                    ptr::null(),
                    &raw mut handle,
                )
            },
            "vkCreateInstance",
        )?;

        // SAFETY: `handle` is a live instance and names/types are paired in `load`.
        let functions = unsafe { InstanceFns::load(&entry, handle) }?;
        let mut context = Self {
            _entry: entry,
            functions,
            handle,
            debug_messenger: ptr::null_mut(),
            surface: ptr::null_mut(),
            surface_maintenance1,
        };

        // SAFETY: The callback and create info live for the duration of the call.
        check(
            unsafe {
                context
                    .functions
                    .create_debug_utils_messenger
                    .expect("loaded function")(
                    context.handle,
                    &raw const debug_info,
                    ptr::null(),
                    &raw mut context.debug_messenger,
                )
            },
            "vkCreateDebugUtilsMessengerEXT",
        )?;

        let surface_info = vk::VkWin32SurfaceCreateInfoKHR {
            sType: vk::VK_STRUCTURE_TYPE_WIN32_SURFACE_CREATE_INFO_KHR,
            hinstance: window.instance(),
            hwnd: window.handle(),
            ..Default::default()
        };
        // SAFETY: Window handles and instance are live; the output is writable.
        check(
            unsafe {
                context
                    .functions
                    .create_win32_surface
                    .expect("loaded function")(
                    context.handle,
                    &raw const surface_info,
                    ptr::null(),
                    &raw mut context.surface,
                )
            },
            "vkCreateWin32SurfaceKHR",
        )?;
        Ok(context)
    }
}

impl Drop for InstanceContext {
    fn drop(&mut self) {
        // SAFETY: Handles are owned, children are gone, and each is destroyed at most once.
        unsafe {
            if !self.surface.is_null() {
                self.functions.destroy_surface.expect("loaded function")(
                    self.handle,
                    self.surface,
                    ptr::null(),
                );
            }
            if !self.debug_messenger.is_null() {
                self.functions
                    .destroy_debug_utils_messenger
                    .expect("loaded function")(
                    self.handle, self.debug_messenger, ptr::null()
                );
            }
            self.functions.destroy_instance.expect("loaded function")(self.handle, ptr::null());
        }
    }
}

#[derive(Clone, Copy)]
struct Adapter {
    handle: vk::VkPhysicalDevice,
    queue_family: u32,
    swapchain_maintenance1: bool,
}

struct DeviceFns {
    destroy_device: vk::PFN_vkDestroyDevice,
    get_device_queue: vk::PFN_vkGetDeviceQueue,
    device_wait_idle: vk::PFN_vkDeviceWaitIdle,
    create_buffer: vk::PFN_vkCreateBuffer,
    destroy_buffer: vk::PFN_vkDestroyBuffer,
    get_buffer_memory_requirements: vk::PFN_vkGetBufferMemoryRequirements,
    create_image: vk::PFN_vkCreateImage,
    destroy_image: vk::PFN_vkDestroyImage,
    get_image_memory_requirements: vk::PFN_vkGetImageMemoryRequirements,
    allocate_memory: vk::PFN_vkAllocateMemory,
    free_memory: vk::PFN_vkFreeMemory,
    bind_buffer_memory: vk::PFN_vkBindBufferMemory,
    bind_image_memory: vk::PFN_vkBindImageMemory,
    map_memory: vk::PFN_vkMapMemory,
    unmap_memory: vk::PFN_vkUnmapMemory,
    create_swapchain: vk::PFN_vkCreateSwapchainKHR,
    destroy_swapchain: vk::PFN_vkDestroySwapchainKHR,
    get_swapchain_images: vk::PFN_vkGetSwapchainImagesKHR,
    acquire_next_image: vk::PFN_vkAcquireNextImageKHR,
    queue_present: vk::PFN_vkQueuePresentKHR,
    create_image_view: vk::PFN_vkCreateImageView,
    destroy_image_view: vk::PFN_vkDestroyImageView,
    create_sampler: vk::PFN_vkCreateSampler,
    destroy_sampler: vk::PFN_vkDestroySampler,
    create_descriptor_set_layout: vk::PFN_vkCreateDescriptorSetLayout,
    destroy_descriptor_set_layout: vk::PFN_vkDestroyDescriptorSetLayout,
    create_descriptor_pool: vk::PFN_vkCreateDescriptorPool,
    destroy_descriptor_pool: vk::PFN_vkDestroyDescriptorPool,
    allocate_descriptor_sets: vk::PFN_vkAllocateDescriptorSets,
    update_descriptor_sets: vk::PFN_vkUpdateDescriptorSets,
    create_shader_module: vk::PFN_vkCreateShaderModule,
    destroy_shader_module: vk::PFN_vkDestroyShaderModule,
    create_pipeline_layout: vk::PFN_vkCreatePipelineLayout,
    destroy_pipeline_layout: vk::PFN_vkDestroyPipelineLayout,
    create_graphics_pipelines: vk::PFN_vkCreateGraphicsPipelines,
    destroy_pipeline: vk::PFN_vkDestroyPipeline,
    create_command_pool: vk::PFN_vkCreateCommandPool,
    destroy_command_pool: vk::PFN_vkDestroyCommandPool,
    allocate_command_buffers: vk::PFN_vkAllocateCommandBuffers,
    reset_command_buffer: vk::PFN_vkResetCommandBuffer,
    begin_command_buffer: vk::PFN_vkBeginCommandBuffer,
    end_command_buffer: vk::PFN_vkEndCommandBuffer,
    cmd_pipeline_barrier2: vk::PFN_vkCmdPipelineBarrier2,
    cmd_begin_rendering: vk::PFN_vkCmdBeginRendering,
    cmd_end_rendering: vk::PFN_vkCmdEndRendering,
    cmd_bind_pipeline: vk::PFN_vkCmdBindPipeline,
    cmd_bind_descriptor_sets: vk::PFN_vkCmdBindDescriptorSets,
    cmd_bind_vertex_buffers: vk::PFN_vkCmdBindVertexBuffers,
    cmd_bind_index_buffer: vk::PFN_vkCmdBindIndexBuffer,
    cmd_copy_buffer2: vk::PFN_vkCmdCopyBuffer2,
    cmd_copy_buffer_to_image2: vk::PFN_vkCmdCopyBufferToImage2,
    cmd_set_viewport: vk::PFN_vkCmdSetViewport,
    cmd_set_scissor: vk::PFN_vkCmdSetScissor,
    cmd_draw_indexed: vk::PFN_vkCmdDrawIndexed,
    create_semaphore: vk::PFN_vkCreateSemaphore,
    destroy_semaphore: vk::PFN_vkDestroySemaphore,
    create_fence: vk::PFN_vkCreateFence,
    destroy_fence: vk::PFN_vkDestroyFence,
    wait_for_fences: vk::PFN_vkWaitForFences,
    reset_fences: vk::PFN_vkResetFences,
    get_fence_status: vk::PFN_vkGetFenceStatus,
    queue_submit2: vk::PFN_vkQueueSubmit2,
}

impl DeviceFns {
    unsafe fn load(instance: &InstanceContext, device: vk::VkDevice) -> Result<Self, ProbeError> {
        let get = instance
            .functions
            .get_device_proc_addr
            .expect("loaded function");
        macro_rules! load {
            ($name:literal) => {{
                // SAFETY: The device is live and each requested type matches its Vulkan name.
                unsafe {
                    load_proc(
                        get(device, $name.as_ptr()),
                        $name.to_string_lossy().as_ref(),
                    )
                }?
            }};
        }
        Ok(Self {
            destroy_device: load!(c"vkDestroyDevice"),
            get_device_queue: load!(c"vkGetDeviceQueue"),
            device_wait_idle: load!(c"vkDeviceWaitIdle"),
            create_buffer: load!(c"vkCreateBuffer"),
            destroy_buffer: load!(c"vkDestroyBuffer"),
            get_buffer_memory_requirements: load!(c"vkGetBufferMemoryRequirements"),
            create_image: load!(c"vkCreateImage"),
            destroy_image: load!(c"vkDestroyImage"),
            get_image_memory_requirements: load!(c"vkGetImageMemoryRequirements"),
            allocate_memory: load!(c"vkAllocateMemory"),
            free_memory: load!(c"vkFreeMemory"),
            bind_buffer_memory: load!(c"vkBindBufferMemory"),
            bind_image_memory: load!(c"vkBindImageMemory"),
            map_memory: load!(c"vkMapMemory"),
            unmap_memory: load!(c"vkUnmapMemory"),
            create_swapchain: load!(c"vkCreateSwapchainKHR"),
            destroy_swapchain: load!(c"vkDestroySwapchainKHR"),
            get_swapchain_images: load!(c"vkGetSwapchainImagesKHR"),
            acquire_next_image: load!(c"vkAcquireNextImageKHR"),
            queue_present: load!(c"vkQueuePresentKHR"),
            create_image_view: load!(c"vkCreateImageView"),
            destroy_image_view: load!(c"vkDestroyImageView"),
            create_sampler: load!(c"vkCreateSampler"),
            destroy_sampler: load!(c"vkDestroySampler"),
            create_descriptor_set_layout: load!(c"vkCreateDescriptorSetLayout"),
            destroy_descriptor_set_layout: load!(c"vkDestroyDescriptorSetLayout"),
            create_descriptor_pool: load!(c"vkCreateDescriptorPool"),
            destroy_descriptor_pool: load!(c"vkDestroyDescriptorPool"),
            allocate_descriptor_sets: load!(c"vkAllocateDescriptorSets"),
            update_descriptor_sets: load!(c"vkUpdateDescriptorSets"),
            create_shader_module: load!(c"vkCreateShaderModule"),
            destroy_shader_module: load!(c"vkDestroyShaderModule"),
            create_pipeline_layout: load!(c"vkCreatePipelineLayout"),
            destroy_pipeline_layout: load!(c"vkDestroyPipelineLayout"),
            create_graphics_pipelines: load!(c"vkCreateGraphicsPipelines"),
            destroy_pipeline: load!(c"vkDestroyPipeline"),
            create_command_pool: load!(c"vkCreateCommandPool"),
            destroy_command_pool: load!(c"vkDestroyCommandPool"),
            allocate_command_buffers: load!(c"vkAllocateCommandBuffers"),
            reset_command_buffer: load!(c"vkResetCommandBuffer"),
            begin_command_buffer: load!(c"vkBeginCommandBuffer"),
            end_command_buffer: load!(c"vkEndCommandBuffer"),
            cmd_pipeline_barrier2: load!(c"vkCmdPipelineBarrier2"),
            cmd_begin_rendering: load!(c"vkCmdBeginRendering"),
            cmd_end_rendering: load!(c"vkCmdEndRendering"),
            cmd_bind_pipeline: load!(c"vkCmdBindPipeline"),
            cmd_bind_descriptor_sets: load!(c"vkCmdBindDescriptorSets"),
            cmd_bind_vertex_buffers: load!(c"vkCmdBindVertexBuffers"),
            cmd_bind_index_buffer: load!(c"vkCmdBindIndexBuffer"),
            cmd_copy_buffer2: load!(c"vkCmdCopyBuffer2"),
            cmd_copy_buffer_to_image2: load!(c"vkCmdCopyBufferToImage2"),
            cmd_set_viewport: load!(c"vkCmdSetViewport"),
            cmd_set_scissor: load!(c"vkCmdSetScissor"),
            cmd_draw_indexed: load!(c"vkCmdDrawIndexed"),
            create_semaphore: load!(c"vkCreateSemaphore"),
            destroy_semaphore: load!(c"vkDestroySemaphore"),
            create_fence: load!(c"vkCreateFence"),
            destroy_fence: load!(c"vkDestroyFence"),
            wait_for_fences: load!(c"vkWaitForFences"),
            reset_fences: load!(c"vkResetFences"),
            get_fence_status: load!(c"vkGetFenceStatus"),
            queue_submit2: load!(c"vkQueueSubmit2"),
        })
    }
}

struct DeviceContext {
    instance: InstanceContext,
    functions: DeviceFns,
    adapter: Adapter,
    handle: vk::VkDevice,
    queue: vk::VkQueue,
}

impl DeviceContext {
    fn new(instance: InstanceContext) -> Result<Self, ProbeError> {
        let adapter = choose_adapter(&instance)?;
        let priority = 1.0_f32;
        let queue_info = vk::VkDeviceQueueCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
            queueFamilyIndex: adapter.queue_family,
            queueCount: 1,
            pQueuePriorities: &raw const priority,
            ..Default::default()
        };
        let mut features13 = vk::VkPhysicalDeviceVulkan13Features {
            sType: vk::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
            synchronization2: vk::VK_TRUE,
            dynamicRendering: vk::VK_TRUE,
            ..Default::default()
        };
        let mut maintenance1 = vk::VkPhysicalDeviceSwapchainMaintenance1FeaturesKHR {
            sType: vk::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SWAPCHAIN_MAINTENANCE_1_FEATURES_KHR,
            swapchainMaintenance1: vk::VK_TRUE,
            ..Default::default()
        };
        if adapter.swapchain_maintenance1 {
            features13.pNext = (&raw mut maintenance1).cast();
        }
        let mut extensions = vec![vk::VK_KHR_SWAPCHAIN_EXTENSION_NAME.as_ptr().cast()];
        if adapter.swapchain_maintenance1 {
            extensions.push(
                vk::VK_KHR_SWAPCHAIN_MAINTENANCE_1_EXTENSION_NAME
                    .as_ptr()
                    .cast(),
            );
        }
        let device_info = vk::VkDeviceCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
            pNext: (&raw mut features13).cast(),
            queueCreateInfoCount: 1,
            pQueueCreateInfos: &raw const queue_info,
            enabledExtensionCount: u32::try_from(extensions.len())
                .expect("device extension count fits u32"),
            ppEnabledExtensionNames: extensions.as_ptr(),
            ..Default::default()
        };
        let mut handle = ptr::null_mut();
        // SAFETY: The selected adapter and all create-info pointers are valid.
        check(
            unsafe {
                instance.functions.create_device.expect("loaded function")(
                    adapter.handle,
                    &raw const device_info,
                    ptr::null(),
                    &raw mut handle,
                )
            },
            "vkCreateDevice",
        )?;
        // SAFETY: `handle` is live and names/types are paired in the loader.
        let functions = unsafe { DeviceFns::load(&instance, handle) }?;
        let mut queue = ptr::null_mut();
        // SAFETY: Queue zero exists because one queue was requested from this family.
        unsafe {
            functions.get_device_queue.expect("loaded function")(
                handle,
                adapter.queue_family,
                0,
                &raw mut queue,
            );
        }
        Ok(Self {
            instance,
            functions,
            adapter,
            handle,
            queue,
        })
    }
}

impl Drop for DeviceContext {
    fn drop(&mut self) {
        // SAFETY: All device children are gone and this device is destroyed once.
        unsafe {
            self.functions.destroy_device.expect("loaded function")(self.handle, ptr::null());
        };
    }
}

fn enumerate_instance_layers(entry: &Entry) -> Result<Vec<Vec<u8>>, ProbeError> {
    let function = entry
        .enumerate_instance_layer_properties
        .expect("loaded function");
    let mut count = 0;
    // SAFETY: The count pointer is writable and the property pointer is null for the count query.
    check_enumeration(
        unsafe { function(&raw mut count, ptr::null_mut()) },
        "enumerate instance layers",
    )?;
    let mut properties = vec![vk::VkLayerProperties::default(); count as usize];
    // SAFETY: Storage contains `count` writable entries.
    check_enumeration(
        unsafe { function(&raw mut count, properties.as_mut_ptr()) },
        "enumerate instance layers",
    )?;
    properties.truncate(count as usize);
    Ok(properties
        .iter()
        .map(|property| fixed_c_string(&property.layerName))
        .collect())
}

fn enumerate_instance_extensions(entry: &Entry) -> Result<Vec<Vec<u8>>, ProbeError> {
    let function = entry
        .enumerate_instance_extension_properties
        .expect("loaded function");
    let mut count = 0;
    // SAFETY: This is the Vulkan two-call enumeration pattern.
    check_enumeration(
        unsafe { function(ptr::null(), &raw mut count, ptr::null_mut()) },
        "enumerate instance extensions",
    )?;
    let mut properties = vec![vk::VkExtensionProperties::default(); count as usize];
    // SAFETY: Storage contains `count` writable entries.
    check_enumeration(
        unsafe { function(ptr::null(), &raw mut count, properties.as_mut_ptr()) },
        "enumerate instance extensions",
    )?;
    properties.truncate(count as usize);
    Ok(properties
        .iter()
        .map(|property| fixed_c_string(&property.extensionName))
        .collect())
}

fn require_name(names: &[Vec<u8>], name: &CStr, description: &str) -> Result<(), ProbeError> {
    if names.iter().any(|candidate| candidate == name.to_bytes()) {
        Ok(())
    } else {
        Err(ProbeError(format!(
            "required {description} {} is unavailable",
            name.to_string_lossy()
        )))
    }
}

#[allow(clippy::too_many_lines)]
fn choose_adapter(instance: &InstanceContext) -> Result<Adapter, ProbeError> {
    let force_swapchain_fallback =
        env::var_os("MULCIBER_VULKAN_FORCE_SWAPCHAIN_FALLBACK").is_some();
    let enumerate = instance
        .functions
        .enumerate_physical_devices
        .expect("loaded function");
    let mut count = 0;
    // SAFETY: This is the Vulkan two-call enumeration pattern.
    check_enumeration(
        unsafe { enumerate(instance.handle, &raw mut count, ptr::null_mut()) },
        "enumerate physical devices",
    )?;
    let mut devices = vec![ptr::null_mut(); count as usize];
    // SAFETY: Storage contains `count` writable handles.
    check_enumeration(
        unsafe { enumerate(instance.handle, &raw mut count, devices.as_mut_ptr()) },
        "enumerate physical devices",
    )?;
    devices.truncate(count as usize);

    let mut candidates = Vec::new();
    for device in devices {
        let mut properties = vk::VkPhysicalDeviceProperties::default();
        // SAFETY: Device is enumerated from this instance and output storage is writable.
        unsafe {
            instance
                .functions
                .get_physical_device_properties
                .expect("loaded function")(device, &raw mut properties);
        }
        let extensions = device_extensions(instance, device)?;
        if properties.apiVersion < API_VERSION_1_4
            || !extensions.iter().any(|name| {
                name == vk::VK_KHR_SWAPCHAIN_EXTENSION_NAME
                    .strip_suffix(&[0])
                    .unwrap()
            })
        {
            continue;
        }

        let maintenance_extension = !force_swapchain_fallback
            && instance.surface_maintenance1
            && extensions.iter().any(|name| {
                name == vk::VK_KHR_SWAPCHAIN_MAINTENANCE_1_EXTENSION_NAME
                    .strip_suffix(&[0])
                    .unwrap()
            });
        let mut maintenance1 = vk::VkPhysicalDeviceSwapchainMaintenance1FeaturesKHR {
            sType: vk::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SWAPCHAIN_MAINTENANCE_1_FEATURES_KHR,
            ..Default::default()
        };

        let mut features13 = vk::VkPhysicalDeviceVulkan13Features {
            sType: vk::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
            ..Default::default()
        };
        if maintenance_extension {
            features13.pNext = (&raw mut maintenance1).cast();
        }
        let mut features = vk::VkPhysicalDeviceFeatures2 {
            sType: vk::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2,
            pNext: (&raw mut features13).cast(),
            ..Default::default()
        };
        // SAFETY: The pNext chain and output structs are writable for this query.
        unsafe {
            instance
                .functions
                .get_physical_device_features2
                .expect("loaded function")(device, &raw mut features);
        }
        if features13.dynamicRendering == vk::VK_FALSE
            || features13.synchronization2 == vk::VK_FALSE
        {
            continue;
        }

        let mut family_count = 0;
        // SAFETY: Count output is writable.
        unsafe {
            instance
                .functions
                .get_queue_family_properties
                .expect("loaded function")(
                device, &raw mut family_count, ptr::null_mut()
            );
        }
        let mut families = vec![vk::VkQueueFamilyProperties::default(); family_count as usize];
        // SAFETY: Storage contains `family_count` writable entries.
        unsafe {
            instance
                .functions
                .get_queue_family_properties
                .expect("loaded function")(
                device, &raw mut family_count, families.as_mut_ptr()
            );
        }
        for (index, family) in families.iter().enumerate() {
            if family.queueCount == 0 || family.queueFlags & vk::VK_QUEUE_GRAPHICS_BIT as u32 == 0 {
                continue;
            }
            let mut supported = vk::VK_FALSE;
            // SAFETY: Surface, device, family index, and output pointer are valid.
            check(
                unsafe {
                    instance
                        .functions
                        .get_surface_support
                        .expect("loaded function")(
                        device,
                        u32::try_from(index).expect("queue family index fits u32"),
                        instance.surface,
                        &raw mut supported,
                    )
                },
                "vkGetPhysicalDeviceSurfaceSupportKHR",
            )?;
            if supported == vk::VK_TRUE {
                let score = match properties.deviceType {
                    vk::VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU => 2,
                    vk::VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU => 1,
                    _ => 0,
                };
                candidates.push((
                    score,
                    Adapter {
                        handle: device,
                        queue_family: u32::try_from(index).expect("queue family index fits u32"),
                        swapchain_maintenance1: maintenance_extension
                            && maintenance1.swapchainMaintenance1 == vk::VK_TRUE,
                    },
                    fixed_c_string(&properties.deviceName),
                ));
                break;
            }
        }
    }

    candidates.sort_by_key(|candidate| candidate.0);
    let (_, adapter, name) = candidates.pop().ok_or_else(|| {
        ProbeError("no Vulkan 1.4 graphics/present adapter satisfies Mulciber's baseline".into())
    })?;
    println!("Vulkan adapter: {}", String::from_utf8_lossy(&name));
    if force_swapchain_fallback {
        println!("Swapchain maintenance override: forced fallback");
    }
    println!(
        "Swapchain retirement: {}",
        if adapter.swapchain_maintenance1 {
            "VK_KHR_swapchain_maintenance1 presentation fences"
        } else {
            "deferred reacquisition fallback"
        }
    );
    Ok(adapter)
}

fn device_extensions(
    instance: &InstanceContext,
    device: vk::VkPhysicalDevice,
) -> Result<Vec<Vec<u8>>, ProbeError> {
    let enumerate = instance
        .functions
        .enumerate_device_extensions
        .expect("loaded function");
    let mut count = 0;
    // SAFETY: This is the Vulkan two-call enumeration pattern.
    check_enumeration(
        unsafe { enumerate(device, ptr::null(), &raw mut count, ptr::null_mut()) },
        "enumerate device extensions",
    )?;
    let mut properties = vec![vk::VkExtensionProperties::default(); count as usize];
    // SAFETY: Storage contains `count` writable entries.
    check_enumeration(
        unsafe { enumerate(device, ptr::null(), &raw mut count, properties.as_mut_ptr()) },
        "enumerate device extensions",
    )?;
    Ok(properties
        .iter()
        .map(|property| fixed_c_string(&property.extensionName))
        .collect())
}

fn debug_messenger_info() -> vk::VkDebugUtilsMessengerCreateInfoEXT {
    vk::VkDebugUtilsMessengerCreateInfoEXT {
        sType: vk::VK_STRUCTURE_TYPE_DEBUG_UTILS_MESSENGER_CREATE_INFO_EXT,
        messageSeverity: (vk::VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT
            | vk::VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT) as u32,
        messageType: (vk::VK_DEBUG_UTILS_MESSAGE_TYPE_GENERAL_BIT_EXT
            | vk::VK_DEBUG_UTILS_MESSAGE_TYPE_VALIDATION_BIT_EXT
            | vk::VK_DEBUG_UTILS_MESSAGE_TYPE_PERFORMANCE_BIT_EXT) as u32,
        pfnUserCallback: Some(debug_callback),
        ..Default::default()
    }
}

unsafe extern "C" fn debug_callback(
    severity: vk::VkDebugUtilsMessageSeverityFlagBitsEXT,
    _types: vk::VkDebugUtilsMessageTypeFlagsEXT,
    data: *const vk::VkDebugUtilsMessengerCallbackDataEXT,
    _user: *mut c_void,
) -> vk::VkBool32 {
    if !data.is_null() {
        // SAFETY: Vulkan guarantees callback data and its message remain valid during the call.
        let message = unsafe {
            let pointer = (*data).pMessage;
            if pointer.is_null() {
                "<no validation message>".into()
            } else {
                CStr::from_ptr(pointer).to_string_lossy()
            }
        };
        let level = if severity >= vk::VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT {
            "error"
        } else {
            "warning"
        };
        eprintln!("Vulkan validation {level}: {message}");
        VALIDATION_MESSAGE_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    vk::VK_FALSE
}

struct RetiredSwapchain {
    handle: vk::VkSwapchainKHR,
    views: Vec<vk::VkImageView>,
    depth: GpuImage,
    pipeline_layout: vk::VkPipelineLayout,
    pipeline: vk::VkPipeline,
    render_finished: Vec<vk::VkSemaphore>,
    present_fences: Vec<vk::VkFence>,
    present_pending: Vec<bool>,
}

#[derive(Default)]
struct GpuBuffer {
    handle: vk::VkBuffer,
    memory: vk::VkDeviceMemory,
}

#[derive(Default)]
struct GpuImage {
    handle: vk::VkImage,
    memory: vk::VkDeviceMemory,
    view: vk::VkImageView,
}

struct Renderer {
    device: DeviceContext,
    swapchain: vk::VkSwapchainKHR,
    format: vk::VkFormat,
    depth_format: vk::VkFormat,
    extent: vk::VkExtent2D,
    images: Vec<vk::VkImage>,
    views: Vec<vk::VkImageView>,
    depth: GpuImage,
    pipeline_layout: vk::VkPipelineLayout,
    pipeline: vk::VkPipeline,
    command_pool: vk::VkCommandPool,
    command_buffer: vk::VkCommandBuffer,
    vertex_buffer: GpuBuffer,
    index_buffer: GpuBuffer,
    texture: GpuImage,
    texture_sampler: vk::VkSampler,
    descriptor_set_layout: vk::VkDescriptorSetLayout,
    descriptor_pool: vk::VkDescriptorPool,
    descriptor_set: vk::VkDescriptorSet,
    image_available: vk::VkSemaphore,
    render_finished: Vec<vk::VkSemaphore>,
    present_fences: Vec<vk::VkFence>,
    present_pending: Vec<bool>,
    presented: Vec<bool>,
    frame_fence: vk::VkFence,
    frame_pending: bool,
    acquire_fence: vk::VkFence,
    retired: Vec<RetiredSwapchain>,
    recreate_after_present: bool,
    live_resize_trace: LiveResizeTrace,
}

impl Renderer {
    fn new(device: DeviceContext, window: &Window) -> Result<Self, ProbeError> {
        let depth_format = choose_depth_format(&device)?;
        let mut renderer = Self {
            device,
            swapchain: ptr::null_mut(),
            format: vk::VK_FORMAT_UNDEFINED,
            depth_format,
            extent: vk::VkExtent2D::default(),
            images: Vec::new(),
            views: Vec::new(),
            depth: GpuImage::default(),
            pipeline_layout: ptr::null_mut(),
            pipeline: ptr::null_mut(),
            command_pool: ptr::null_mut(),
            command_buffer: ptr::null_mut(),
            vertex_buffer: GpuBuffer::default(),
            index_buffer: GpuBuffer::default(),
            texture: GpuImage::default(),
            texture_sampler: ptr::null_mut(),
            descriptor_set_layout: ptr::null_mut(),
            descriptor_pool: ptr::null_mut(),
            descriptor_set: ptr::null_mut(),
            image_available: ptr::null_mut(),
            render_finished: Vec::new(),
            present_fences: Vec::new(),
            present_pending: Vec::new(),
            presented: Vec::new(),
            frame_fence: ptr::null_mut(),
            frame_pending: false,
            acquire_fence: ptr::null_mut(),
            retired: Vec::new(),
            recreate_after_present: false,
            live_resize_trace: LiveResizeTrace::from_environment(),
        };
        if renderer.live_resize_trace.enabled {
            println!("Live resize timing trace enabled");
        }
        renderer.create_frame_resources()?;
        renderer.create_geometry_buffers()?;
        renderer.create_texture_resources()?;
        let (width, height) = window
            .client_extent()
            .map_err(|error| ProbeError(error.to_string()))?;
        if width != 0 && height != 0 {
            renderer.recreate_swapchain(width, height)?;
            println!(
                "Depth: resize-dependent device-local {} attachment with testing/writes",
                depth_format_name(renderer.depth_format)
            );
        }
        Ok(renderer)
    }

    fn create_frame_resources(&mut self) -> Result<(), ProbeError> {
        let pool_info = vk::VkCommandPoolCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
            flags: vk::VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT as u32,
            queueFamilyIndex: self.device.adapter.queue_family,
            ..Default::default()
        };
        // SAFETY: Device and output handle are live and writable.
        check(
            unsafe {
                self.device
                    .functions
                    .create_command_pool
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const pool_info,
                    ptr::null(),
                    &raw mut self.command_pool,
                )
            },
            "vkCreateCommandPool",
        )?;
        let allocation = vk::VkCommandBufferAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
            commandPool: self.command_pool,
            level: vk::VK_COMMAND_BUFFER_LEVEL_PRIMARY,
            commandBufferCount: 1,
            ..Default::default()
        };
        // SAFETY: The command pool is live and output storage is writable.
        check(
            unsafe {
                self.device
                    .functions
                    .allocate_command_buffers
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const allocation,
                    &raw mut self.command_buffer,
                )
            },
            "vkAllocateCommandBuffers",
        )?;

        let semaphore_info = vk::VkSemaphoreCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO,
            ..Default::default()
        };
        // SAFETY: Device and create info are valid; output is writable.
        check(
            unsafe {
                self.device
                    .functions
                    .create_semaphore
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const semaphore_info,
                    ptr::null(),
                    &raw mut self.image_available,
                )
            },
            "vkCreateSemaphore",
        )?;
        let fence_info = vk::VkFenceCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_FENCE_CREATE_INFO,
            flags: vk::VK_FENCE_CREATE_SIGNALED_BIT as u32,
            ..Default::default()
        };
        // SAFETY: Device and create info are valid; output is writable.
        check(
            unsafe {
                self.device.functions.create_fence.expect("loaded function")(
                    self.device.handle,
                    &raw const fence_info,
                    ptr::null(),
                    &raw mut self.frame_fence,
                )
            },
            "vkCreateFence",
        )?;
        if !self.device.adapter.swapchain_maintenance1 {
            let acquire_fence_info = vk::VkFenceCreateInfo {
                sType: vk::VK_STRUCTURE_TYPE_FENCE_CREATE_INFO,
                ..Default::default()
            };
            // SAFETY: Device/create info are valid; output is writable.
            check(
                unsafe {
                    self.device.functions.create_fence.expect("loaded function")(
                        self.device.handle,
                        &raw const acquire_fence_info,
                        ptr::null(),
                        &raw mut self.acquire_fence,
                    )
                },
                "vkCreateFence for image acquisition",
            )?;
        }
        Ok(())
    }

    fn create_geometry_buffers(&mut self) -> Result<(), ProbeError> {
        // SAFETY: `Vertex` is `repr(C)`, contains only initialized `f32` arrays, and its tested
        // 28-byte layout has no padding.
        let vertex_bytes = unsafe { slice_bytes(&TRIANGLE_VERTICES) };
        // SAFETY: `u16` has no padding and every element is initialized.
        let index_bytes = unsafe { slice_bytes(&TRIANGLE_INDICES) };
        let mut vertex_staging = self.create_staging_buffer(vertex_bytes, "vertex")?;
        let mut index_staging = match self.create_staging_buffer(index_bytes, "index") {
            Ok(buffer) => buffer,
            Err(error) => {
                // SAFETY: No commands reference this buffer because recording has not started.
                unsafe { self.destroy_buffer(&mut vertex_staging) };
                return Err(error);
            }
        };

        let upload = self.create_device_geometry_and_upload(
            &vertex_staging,
            vertex_bytes.len(),
            &index_staging,
            index_bytes.len(),
        );
        if upload.is_err() {
            // An error after queue submission can leave staging buffers referenced by unfinished
            // work. The startup failure path favors orderly cleanup over latency.
            // SAFETY: The queue belongs to this device; a device-lost error still permits cleanup.
            let _ = unsafe {
                self.device
                    .functions
                    .device_wait_idle
                    .expect("loaded function")(self.device.handle)
            };
        }
        // SAFETY: Successful upload waits for its fence; the error path attempted device idle.
        unsafe {
            self.destroy_buffer(&mut vertex_staging);
            self.destroy_buffer(&mut index_staging);
        }
        upload?;
        println!("Geometry: device-local vertex/index buffers uploaded through staging");
        Ok(())
    }

    fn create_staging_buffer(
        &self,
        bytes: &[u8],
        description: &str,
    ) -> Result<GpuBuffer, ProbeError> {
        let required_flags = (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
            | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) as u32;
        let mut buffer = self.create_buffer(
            bytes.len(),
            vk::VK_BUFFER_USAGE_TRANSFER_SRC_BIT as u32,
            required_flags,
            &format!("{description} staging"),
        )?;
        if let Err(error) = self.write_buffer(&buffer, bytes, description) {
            // SAFETY: The buffer has not been submitted to the GPU.
            unsafe { self.destroy_buffer(&mut buffer) };
            return Err(error);
        }
        Ok(buffer)
    }

    fn create_device_geometry_and_upload(
        &mut self,
        vertex_staging: &GpuBuffer,
        vertex_size: usize,
        index_staging: &GpuBuffer,
        index_size: usize,
    ) -> Result<(), ProbeError> {
        self.vertex_buffer = self.create_buffer(
            vertex_size,
            (vk::VK_BUFFER_USAGE_TRANSFER_DST_BIT | vk::VK_BUFFER_USAGE_VERTEX_BUFFER_BIT) as u32,
            vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            "device-local vertex",
        )?;
        self.index_buffer = self.create_buffer(
            index_size,
            (vk::VK_BUFFER_USAGE_TRANSFER_DST_BIT | vk::VK_BUFFER_USAGE_INDEX_BUFFER_BIT) as u32,
            vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            "device-local index",
        )?;
        self.upload_geometry(
            vertex_staging,
            u64::try_from(vertex_size).expect("vertex byte length fits u64"),
            index_staging,
            u64::try_from(index_size).expect("index byte length fits u64"),
        )
    }

    fn create_texture_resources(&mut self) -> Result<(), ProbeError> {
        let mut staging = self.create_staging_buffer(&CHECKERBOARD_TEXELS, "texture")?;
        let result = self.create_texture_and_upload(&staging);
        if result.is_err() {
            // SAFETY: If submission started, waiting idle prevents the staging buffer from being
            // destroyed while referenced by the queue.
            let _ = unsafe {
                self.device
                    .functions
                    .device_wait_idle
                    .expect("loaded function")(self.device.handle)
            };
        }
        // SAFETY: Successful upload waited for completion; the error path attempted device idle.
        unsafe { self.destroy_buffer(&mut staging) };
        result?;
        self.create_texture_sampler()?;
        self.create_texture_descriptors()?;
        println!(
            "Texture: device-local {TEXTURE_WIDTH}x{TEXTURE_HEIGHT} RGBA8 image uploaded and sampled"
        );
        Ok(())
    }

    fn create_texture_and_upload(&mut self, staging: &GpuBuffer) -> Result<(), ProbeError> {
        let info = vk::VkImageCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
            imageType: vk::VK_IMAGE_TYPE_2D,
            format: vk::VK_FORMAT_R8G8B8A8_SRGB,
            extent: vk::VkExtent3D {
                width: TEXTURE_WIDTH,
                height: TEXTURE_HEIGHT,
                depth: 1,
            },
            mipLevels: 1,
            arrayLayers: 1,
            samples: vk::VK_SAMPLE_COUNT_1_BIT,
            tiling: vk::VK_IMAGE_TILING_OPTIMAL,
            usage: (vk::VK_IMAGE_USAGE_TRANSFER_DST_BIT | vk::VK_IMAGE_USAGE_SAMPLED_BIT) as u32,
            sharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
            initialLayout: vk::VK_IMAGE_LAYOUT_UNDEFINED,
            ..Default::default()
        };
        check(
            // SAFETY: Device/create info are valid and owned output storage is writable.
            unsafe {
                self.device.functions.create_image.expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut self.texture.handle,
                )
            },
            "vkCreateImage for sampled texture",
        )?;
        let mut requirements = vk::VkMemoryRequirements::default();
        // SAFETY: The image is live and requirements storage is writable.
        unsafe {
            self.device
                .functions
                .get_image_memory_requirements
                .expect("loaded function")(
                self.device.handle,
                self.texture.handle,
                &raw mut requirements,
            );
        }
        let memory_type = self
            .find_memory_type(
                requirements.memoryTypeBits,
                vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            )
            .ok_or_else(|| {
                ProbeError("adapter exposes no device-local texture memory type".into())
            })?;
        let allocation = vk::VkMemoryAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
            allocationSize: requirements.size,
            memoryTypeIndex: memory_type,
            ..Default::default()
        };
        check(
            // SAFETY: Allocation info and output memory storage are valid.
            unsafe {
                self.device
                    .functions
                    .allocate_memory
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const allocation,
                    ptr::null(),
                    &raw mut self.texture.memory,
                )
            },
            "vkAllocateMemory for sampled texture",
        )?;
        check(
            // SAFETY: Image and allocation are compatible at offset zero.
            unsafe {
                self.device
                    .functions
                    .bind_image_memory
                    .expect("loaded function")(
                    self.device.handle,
                    self.texture.handle,
                    self.texture.memory,
                    0,
                )
            },
            "vkBindImageMemory for sampled texture",
        )?;
        self.upload_texture(staging)?;
        self.texture.view = self.create_texture_view()?;
        Ok(())
    }

    fn find_memory_type(&self, compatible_bits: u32, required_flags: u32) -> Option<u32> {
        let mut properties = vk::VkPhysicalDeviceMemoryProperties::default();
        // SAFETY: The selected adapter is live and properties storage is writable.
        unsafe {
            self.device
                .instance
                .functions
                .get_physical_device_memory_properties
                .expect("loaded function")(
                self.device.adapter.handle, &raw mut properties
            );
        }
        find_memory_type(&properties, compatible_bits, required_flags)
    }

    fn create_texture_view(&self) -> Result<vk::VkImageView, ProbeError> {
        let info = vk::VkImageViewCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
            image: self.texture.handle,
            viewType: vk::VK_IMAGE_VIEW_TYPE_2D,
            format: vk::VK_FORMAT_R8G8B8A8_SRGB,
            components: vk::VkComponentMapping {
                r: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                g: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                b: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                a: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
            },
            subresourceRange: color_subresource_range(),
            ..Default::default()
        };
        let mut view = ptr::null_mut();
        check(
            // SAFETY: Image/create info are valid and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_image_view
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut view,
                )
            },
            "vkCreateImageView for sampled texture",
        )?;
        Ok(view)
    }

    fn create_texture_sampler(&mut self) -> Result<(), ProbeError> {
        let info = vk::VkSamplerCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,
            magFilter: vk::VK_FILTER_NEAREST,
            minFilter: vk::VK_FILTER_NEAREST,
            mipmapMode: vk::VK_SAMPLER_MIPMAP_MODE_NEAREST,
            addressModeU: vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            addressModeV: vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            addressModeW: vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            maxAnisotropy: 1.0,
            maxLod: 0.0,
            borderColor: vk::VK_BORDER_COLOR_INT_OPAQUE_BLACK,
            ..Default::default()
        };
        check(
            // SAFETY: Device/create info are valid and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_sampler
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut self.texture_sampler,
                )
            },
            "vkCreateSampler",
        )
    }

    fn create_texture_descriptors(&mut self) -> Result<(), ProbeError> {
        let binding = vk::VkDescriptorSetLayoutBinding {
            binding: 0,
            descriptorType: vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            descriptorCount: 1,
            stageFlags: vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
            ..Default::default()
        };
        let layout_info = vk::VkDescriptorSetLayoutCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
            bindingCount: 1,
            pBindings: &raw const binding,
            ..Default::default()
        };
        check(
            // SAFETY: Device/create info are valid and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_descriptor_set_layout
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const layout_info,
                    ptr::null(),
                    &raw mut self.descriptor_set_layout,
                )
            },
            "vkCreateDescriptorSetLayout",
        )?;
        let pool_size = vk::VkDescriptorPoolSize {
            type_: vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            descriptorCount: 1,
        };
        let pool_info = vk::VkDescriptorPoolCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
            maxSets: 1,
            poolSizeCount: 1,
            pPoolSizes: &raw const pool_size,
            ..Default::default()
        };
        check(
            // SAFETY: Device/create info are valid and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_descriptor_pool
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const pool_info,
                    ptr::null(),
                    &raw mut self.descriptor_pool,
                )
            },
            "vkCreateDescriptorPool",
        )?;
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: self.descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const self.descriptor_set_layout,
            ..Default::default()
        };
        check(
            // SAFETY: Pool/layout are live and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .allocate_descriptor_sets
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const allocate,
                    &raw mut self.descriptor_set,
                )
            },
            "vkAllocateDescriptorSets",
        )?;
        let image = vk::VkDescriptorImageInfo {
            sampler: self.texture_sampler,
            imageView: self.texture.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        };
        let write = vk::VkWriteDescriptorSet {
            sType: vk::VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
            dstSet: self.descriptor_set,
            dstBinding: 0,
            descriptorCount: 1,
            descriptorType: vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            pImageInfo: &raw const image,
            ..Default::default()
        };
        // SAFETY: The descriptor set and referenced image/sampler are live.
        unsafe {
            self.device
                .functions
                .update_descriptor_sets
                .expect("loaded function")(
                self.device.handle, 1, &raw const write, 0, ptr::null()
            );
        }
        Ok(())
    }

    fn create_buffer(
        &self,
        byte_len: usize,
        usage: vk::VkBufferUsageFlags,
        required_flags: vk::VkMemoryPropertyFlags,
        description: &str,
    ) -> Result<GpuBuffer, ProbeError> {
        let size = u64::try_from(byte_len).expect("buffer byte length fits u64");
        let info = vk::VkBufferCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
            size,
            usage,
            sharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
            ..Default::default()
        };
        let mut buffer = GpuBuffer::default();
        check(
            // SAFETY: The device/create info are live and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_buffer
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut buffer.handle,
                )
            },
            &format!("vkCreateBuffer for {description} buffer"),
        )?;
        let mut requirements = vk::VkMemoryRequirements::default();
        // SAFETY: The buffer is live and requirements storage is writable.
        unsafe {
            self.device
                .functions
                .get_buffer_memory_requirements
                .expect("loaded function")(
                self.device.handle,
                buffer.handle,
                &raw mut requirements,
            );
        }
        let mut properties = vk::VkPhysicalDeviceMemoryProperties::default();
        // SAFETY: The selected adapter is live and properties storage is writable.
        unsafe {
            self.device
                .instance
                .functions
                .get_physical_device_memory_properties
                .expect("loaded function")(
                self.device.adapter.handle, &raw mut properties
            );
        }
        let Some(memory_type) =
            find_memory_type(&properties, requirements.memoryTypeBits, required_flags)
        else {
            // SAFETY: The buffer is unbound and has never been submitted to the GPU.
            unsafe { self.destroy_buffer(&mut buffer) };
            return Err(ProbeError(format!(
                "adapter exposes no compatible memory type for the {description} buffer"
            )));
        };
        let allocation = vk::VkMemoryAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
            allocationSize: requirements.size,
            memoryTypeIndex: memory_type,
            ..Default::default()
        };
        let allocate = check(
            // SAFETY: Allocation info and output memory storage are valid.
            unsafe {
                self.device
                    .functions
                    .allocate_memory
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const allocation,
                    ptr::null(),
                    &raw mut buffer.memory,
                )
            },
            &format!("vkAllocateMemory for {description} buffer"),
        );
        if let Err(error) = allocate {
            // SAFETY: Allocation failed, so only the unbound buffer needs cleanup.
            unsafe { self.destroy_buffer(&mut buffer) };
            return Err(error);
        }
        let bind = check(
            // SAFETY: The buffer and allocation share compatible requirements at offset zero.
            unsafe {
                self.device
                    .functions
                    .bind_buffer_memory
                    .expect("loaded function")(
                    self.device.handle, buffer.handle, buffer.memory, 0
                )
            },
            &format!("vkBindBufferMemory for {description} buffer"),
        );
        if let Err(error) = bind {
            // SAFETY: Binding failed and neither object can be referenced by GPU work.
            unsafe { self.destroy_buffer(&mut buffer) };
            return Err(error);
        }
        Ok(buffer)
    }

    fn write_buffer(
        &self,
        buffer: &GpuBuffer,
        bytes: &[u8],
        description: &str,
    ) -> Result<(), ProbeError> {
        let mut mapped = ptr::null_mut();
        check(
            // SAFETY: The allocation is host-visible and the requested range is in bounds.
            unsafe {
                self.device.functions.map_memory.expect("loaded function")(
                    self.device.handle,
                    buffer.memory,
                    0,
                    u64::try_from(bytes.len()).expect("buffer byte length fits u64"),
                    0,
                    &raw mut mapped,
                )
            },
            &format!("vkMapMemory for {description} data"),
        )?;
        // SAFETY: Vulkan returned a writable mapping of at least `bytes.len()` bytes. The selected
        // memory is host coherent, so unmapping makes the copied bytes visible without a flush.
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast(), bytes.len());
            self.device.functions.unmap_memory.expect("loaded function")(
                self.device.handle,
                buffer.memory,
            );
        }
        Ok(())
    }

    fn upload_geometry(
        &mut self,
        vertex_staging: &GpuBuffer,
        vertex_size: vk::VkDeviceSize,
        index_staging: &GpuBuffer,
        index_size: vk::VkDeviceSize,
    ) -> Result<(), ProbeError> {
        check(
            // SAFETY: The frame fence is signaled during startup and not attached to GPU work.
            unsafe {
                self.device.functions.reset_fences.expect("loaded function")(
                    self.device.handle,
                    1,
                    &raw const self.frame_fence,
                )
            },
            "vkResetFences for geometry upload",
        )?;
        self.record_geometry_upload(vertex_staging, vertex_size, index_staging, index_size)?;
        self.submit_upload()?;
        self.wait_for_frame()
    }

    fn upload_texture(&mut self, staging: &GpuBuffer) -> Result<(), ProbeError> {
        check(
            // SAFETY: Geometry upload completed and left the fence signaled.
            unsafe {
                self.device.functions.reset_fences.expect("loaded function")(
                    self.device.handle,
                    1,
                    &raw const self.frame_fence,
                )
            },
            "vkResetFences for texture upload",
        )?;
        check(
            // SAFETY: The prior upload completed, so the command buffer can be reset.
            unsafe {
                self.device
                    .functions
                    .reset_command_buffer
                    .expect("loaded function")(self.command_buffer, 0)
            },
            "vkResetCommandBuffer for texture upload",
        )?;
        self.record_texture_upload(staging)?;
        self.submit_upload()?;
        self.wait_for_frame()
    }

    fn record_texture_upload(&self, staging: &GpuBuffer) -> Result<(), ProbeError> {
        let begin = vk::VkCommandBufferBeginInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            flags: vk::VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT as u32,
            ..Default::default()
        };
        check(
            // SAFETY: The reset command buffer is in its initial state.
            unsafe {
                self.device
                    .functions
                    .begin_command_buffer
                    .expect("loaded function")(self.command_buffer, &raw const begin)
            },
            "vkBeginCommandBuffer for texture upload",
        )?;
        self.image_barrier(
            self.texture.handle,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_NONE,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            color_subresource_range(),
        );
        let region = vk::VkBufferImageCopy2 {
            sType: vk::VK_STRUCTURE_TYPE_BUFFER_IMAGE_COPY_2,
            imageSubresource: vk::VkImageSubresourceLayers {
                aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
                mipLevel: 0,
                baseArrayLayer: 0,
                layerCount: 1,
            },
            imageExtent: vk::VkExtent3D {
                width: TEXTURE_WIDTH,
                height: TEXTURE_HEIGHT,
                depth: 1,
            },
            ..Default::default()
        };
        let copy = vk::VkCopyBufferToImageInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_COPY_BUFFER_TO_IMAGE_INFO_2,
            srcBuffer: staging.handle,
            dstImage: self.texture.handle,
            dstImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            regionCount: 1,
            pRegions: &raw const region,
            ..Default::default()
        };
        // SAFETY: The source buffer and destination image/range are live and correctly laid out.
        unsafe {
            self.device
                .functions
                .cmd_copy_buffer_to_image2
                .expect("loaded function")(self.command_buffer, &raw const copy);
        }
        self.image_barrier(
            self.texture.handle,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
            vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
            vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            color_subresource_range(),
        );
        check(
            // SAFETY: The command buffer is recording and the upload commands are complete.
            unsafe {
                self.device
                    .functions
                    .end_command_buffer
                    .expect("loaded function")(self.command_buffer)
            },
            "vkEndCommandBuffer for texture upload",
        )
    }

    fn record_geometry_upload(
        &self,
        vertex_staging: &GpuBuffer,
        vertex_size: vk::VkDeviceSize,
        index_staging: &GpuBuffer,
        index_size: vk::VkDeviceSize,
    ) -> Result<(), ProbeError> {
        let begin = vk::VkCommandBufferBeginInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            flags: vk::VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT as u32,
            ..Default::default()
        };
        check(
            // SAFETY: The fresh command buffer is in its initial state and begin info is valid.
            unsafe {
                self.device
                    .functions
                    .begin_command_buffer
                    .expect("loaded function")(self.command_buffer, &raw const begin)
            },
            "vkBeginCommandBuffer for geometry upload",
        )?;
        self.copy_buffer(
            vertex_staging.handle,
            self.vertex_buffer.handle,
            vertex_size,
        );
        self.copy_buffer(index_staging.handle, self.index_buffer.handle, index_size);

        let barriers = [
            buffer_barrier(
                self.vertex_buffer.handle,
                vertex_size,
                vk::VK_ACCESS_2_VERTEX_ATTRIBUTE_READ_BIT,
            ),
            buffer_barrier(
                self.index_buffer.handle,
                index_size,
                vk::VK_ACCESS_2_INDEX_READ_BIT,
            ),
        ];
        let dependency = vk::VkDependencyInfo {
            sType: vk::VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
            bufferMemoryBarrierCount: u32::try_from(barriers.len())
                .expect("buffer barrier count fits u32"),
            pBufferMemoryBarriers: barriers.as_ptr(),
            ..Default::default()
        };
        // SAFETY: The command buffer is recording and all buffers/ranges are live and valid.
        unsafe {
            self.device
                .functions
                .cmd_pipeline_barrier2
                .expect("loaded function")(self.command_buffer, &raw const dependency);
        }
        check(
            // SAFETY: The command buffer is recording and all commands are complete.
            unsafe {
                self.device
                    .functions
                    .end_command_buffer
                    .expect("loaded function")(self.command_buffer)
            },
            "vkEndCommandBuffer for geometry upload",
        )
    }

    fn copy_buffer(&self, source: vk::VkBuffer, destination: vk::VkBuffer, size: vk::VkDeviceSize) {
        let region = vk::VkBufferCopy2 {
            sType: vk::VK_STRUCTURE_TYPE_BUFFER_COPY_2,
            size,
            ..Default::default()
        };
        let copy = vk::VkCopyBufferInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_COPY_BUFFER_INFO_2,
            srcBuffer: source,
            dstBuffer: destination,
            regionCount: 1,
            pRegions: &raw const region,
            ..Default::default()
        };
        // SAFETY: The command buffer is recording; source/destination usages and range are valid.
        unsafe {
            self.device
                .functions
                .cmd_copy_buffer2
                .expect("loaded function")(self.command_buffer, &raw const copy);
        }
    }

    fn submit_upload(&mut self) -> Result<(), ProbeError> {
        let command = command_buffer_submit_info(self.command_buffer);
        let submit = vk::VkSubmitInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_SUBMIT_INFO_2,
            commandBufferInfoCount: 1,
            pCommandBufferInfos: &raw const command,
            ..Default::default()
        };
        check(
            // SAFETY: The recorded command buffer and unsignaled fence are live.
            unsafe {
                self.device
                    .functions
                    .queue_submit2
                    .expect("loaded function")(
                    self.device.queue,
                    1,
                    &raw const submit,
                    self.frame_fence,
                )
            },
            "vkQueueSubmit2 for geometry upload",
        )?;
        self.frame_pending = true;
        Ok(())
    }

    unsafe fn destroy_buffer(&self, buffer: &mut GpuBuffer) {
        if !buffer.handle.is_null() {
            // SAFETY: The buffer is owned by this renderer and no longer in GPU use.
            unsafe {
                self.device
                    .functions
                    .destroy_buffer
                    .expect("loaded function")(
                    self.device.handle, buffer.handle, ptr::null()
                );
            }
            buffer.handle = ptr::null_mut();
        }
        if !buffer.memory.is_null() {
            // SAFETY: All objects bound to the allocation have been destroyed.
            unsafe {
                self.device.functions.free_memory.expect("loaded function")(
                    self.device.handle,
                    buffer.memory,
                    ptr::null(),
                );
            }
            buffer.memory = ptr::null_mut();
        }
    }

    unsafe fn destroy_image(&self, image: &mut GpuImage) {
        // SAFETY: The caller established that this renderer-owned image is no longer in GPU use.
        unsafe { destroy_gpu_image(&self.device, image) };
    }

    fn recreate_swapchain(&mut self, width: u32, height: u32) -> Result<(), ProbeError> {
        self.wait_for_frame()?;

        let mut capabilities = vk::VkSurfaceCapabilitiesKHR::default();
        // SAFETY: Adapter/surface are live and output storage is writable.
        check(
            unsafe {
                self.device
                    .instance
                    .functions
                    .get_surface_capabilities
                    .expect("loaded function")(
                    self.device.adapter.handle,
                    self.device.instance.surface,
                    &raw mut capabilities,
                )
            },
            "vkGetPhysicalDeviceSurfaceCapabilitiesKHR",
        )?;
        let formats = self.surface_formats()?;
        let format = choose_surface_format(&formats)
            .ok_or_else(|| ProbeError("surface exposes no formats".into()))?;
        self.require_fifo_present_mode()?;
        let extent = choose_extent(capabilities, width, height);
        let mut image_count = capabilities.minImageCount.saturating_add(1).max(3);
        if capabilities.maxImageCount != 0 {
            image_count = image_count.min(capabilities.maxImageCount);
        }
        let composite_alpha = choose_composite_alpha(capabilities.supportedCompositeAlpha)
            .ok_or_else(|| ProbeError("surface exposes no composite-alpha mode".into()))?;
        let create_info = vk::VkSwapchainCreateInfoKHR {
            sType: vk::VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR,
            surface: self.device.instance.surface,
            minImageCount: image_count,
            imageFormat: format.format,
            imageColorSpace: format.colorSpace,
            imageExtent: extent,
            imageArrayLayers: 1,
            imageUsage: vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT as u32,
            imageSharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
            preTransform: capabilities.currentTransform,
            compositeAlpha: composite_alpha,
            presentMode: vk::VK_PRESENT_MODE_FIFO_KHR,
            clipped: vk::VK_TRUE,
            oldSwapchain: self.swapchain,
            ..Default::default()
        };
        let mut swapchain = ptr::null_mut();
        // SAFETY: Device, surface, and create info are valid; output is writable.
        check(
            unsafe {
                self.device
                    .functions
                    .create_swapchain
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const create_info,
                    ptr::null(),
                    &raw mut swapchain,
                )
            },
            "vkCreateSwapchainKHR",
        )?;
        let reuse_pipeline = !self.pipeline.is_null() && self.format == format.format;
        self.retire_current_swapchain(!reuse_pipeline);
        self.swapchain = swapchain;
        self.format = format.format;
        self.extent = extent;
        self.images = self.swapchain_images()?;
        self.create_present_resources()?;
        self.presented = vec![false; self.images.len()];
        self.views = self.create_image_views()?;
        self.create_depth_attachment()?;
        if !reuse_pipeline {
            self.create_pipeline()?;
        }
        self.collect_retired_swapchains()?;
        self.recreate_after_present = false;
        Ok(())
    }

    fn create_present_resources(&mut self) -> Result<(), ProbeError> {
        let info = vk::VkSemaphoreCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO,
            ..Default::default()
        };
        self.render_finished.reserve(self.images.len());
        for _ in &self.images {
            let mut semaphore = ptr::null_mut();
            // SAFETY: Device/create info are valid and the output handle is writable.
            let result = check(
                unsafe {
                    self.device
                        .functions
                        .create_semaphore
                        .expect("loaded function")(
                        self.device.handle,
                        &raw const info,
                        ptr::null(),
                        &raw mut semaphore,
                    )
                },
                "vkCreateSemaphore for swapchain image",
            );
            if let Err(error) = result {
                // SAFETY: Previously created semaphores are idle because the new swapchain has not
                // been rendered or presented yet.
                unsafe {
                    for semaphore in self.render_finished.drain(..) {
                        self.device
                            .functions
                            .destroy_semaphore
                            .expect("loaded function")(
                            self.device.handle, semaphore, ptr::null()
                        );
                    }
                }
                return Err(error);
            }
            self.render_finished.push(semaphore);
        }
        if self.device.adapter.swapchain_maintenance1 {
            let fence_info = vk::VkFenceCreateInfo {
                sType: vk::VK_STRUCTURE_TYPE_FENCE_CREATE_INFO,
                ..Default::default()
            };
            self.present_fences.reserve(self.images.len());
            for _ in &self.images {
                let mut fence = ptr::null_mut();
                // SAFETY: Device/create info are valid and the output handle is writable.
                if let Err(error) = check(
                    unsafe {
                        self.device.functions.create_fence.expect("loaded function")(
                            self.device.handle,
                            &raw const fence_info,
                            ptr::null(),
                            &raw mut fence,
                        )
                    },
                    "vkCreateFence for presentation",
                ) {
                    // SAFETY: The new swapchain has not been used, so these objects are idle.
                    unsafe {
                        for fence in self.present_fences.drain(..) {
                            self.device
                                .functions
                                .destroy_fence
                                .expect("loaded function")(
                                self.device.handle, fence, ptr::null()
                            );
                        }
                        for semaphore in self.render_finished.drain(..) {
                            self.device
                                .functions
                                .destroy_semaphore
                                .expect("loaded function")(
                                self.device.handle,
                                semaphore,
                                ptr::null(),
                            );
                        }
                    }
                    return Err(error);
                }
                self.present_fences.push(fence);
            }
            self.present_pending = vec![false; self.images.len()];
        }
        Ok(())
    }

    fn surface_formats(&self) -> Result<Vec<vk::VkSurfaceFormatKHR>, ProbeError> {
        let function = self
            .device
            .instance
            .functions
            .get_surface_formats
            .expect("loaded function");
        let mut count = 0;
        // SAFETY: This is the Vulkan two-call enumeration pattern.
        check_enumeration(
            unsafe {
                function(
                    self.device.adapter.handle,
                    self.device.instance.surface,
                    &raw mut count,
                    ptr::null_mut(),
                )
            },
            "enumerate surface formats",
        )?;
        let mut formats = vec![vk::VkSurfaceFormatKHR::default(); count as usize];
        // SAFETY: Storage contains `count` writable entries.
        check_enumeration(
            unsafe {
                function(
                    self.device.adapter.handle,
                    self.device.instance.surface,
                    &raw mut count,
                    formats.as_mut_ptr(),
                )
            },
            "enumerate surface formats",
        )?;
        formats.truncate(count as usize);
        Ok(formats)
    }

    fn require_fifo_present_mode(&self) -> Result<(), ProbeError> {
        let function = self
            .device
            .instance
            .functions
            .get_surface_present_modes
            .expect("loaded function");
        let mut count = 0;
        // SAFETY: This is the Vulkan two-call enumeration pattern.
        check_enumeration(
            unsafe {
                function(
                    self.device.adapter.handle,
                    self.device.instance.surface,
                    &raw mut count,
                    ptr::null_mut(),
                )
            },
            "enumerate present modes",
        )?;
        let mut modes = vec![0; count as usize];
        // SAFETY: Storage contains `count` writable entries.
        check_enumeration(
            unsafe {
                function(
                    self.device.adapter.handle,
                    self.device.instance.surface,
                    &raw mut count,
                    modes.as_mut_ptr(),
                )
            },
            "enumerate present modes",
        )?;
        if modes[..count as usize].contains(&vk::VK_PRESENT_MODE_FIFO_KHR) {
            Ok(())
        } else {
            Err(ProbeError(
                "surface does not expose required FIFO VSync".into(),
            ))
        }
    }

    fn swapchain_images(&self) -> Result<Vec<vk::VkImage>, ProbeError> {
        let function = self
            .device
            .functions
            .get_swapchain_images
            .expect("loaded function");
        let mut count = 0;
        // SAFETY: This is the Vulkan two-call enumeration pattern.
        check_enumeration(
            unsafe {
                function(
                    self.device.handle,
                    self.swapchain,
                    &raw mut count,
                    ptr::null_mut(),
                )
            },
            "enumerate swapchain images",
        )?;
        let mut images = vec![ptr::null_mut(); count as usize];
        // SAFETY: Storage contains `count` writable handles.
        check_enumeration(
            unsafe {
                function(
                    self.device.handle,
                    self.swapchain,
                    &raw mut count,
                    images.as_mut_ptr(),
                )
            },
            "enumerate swapchain images",
        )?;
        images.truncate(count as usize);
        Ok(images)
    }

    fn create_image_views(&self) -> Result<Vec<vk::VkImageView>, ProbeError> {
        let mut views = Vec::with_capacity(self.images.len());
        for &image in &self.images {
            let info = vk::VkImageViewCreateInfo {
                sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
                image,
                viewType: vk::VK_IMAGE_VIEW_TYPE_2D,
                format: self.format,
                components: vk::VkComponentMapping {
                    r: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                    g: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                    b: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                    a: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                },
                subresourceRange: color_subresource_range(),
                ..Default::default()
            };
            let mut view = ptr::null_mut();
            // SAFETY: The swapchain image and device are live; output is writable.
            if let Err(error) = check(
                unsafe {
                    self.device
                        .functions
                        .create_image_view
                        .expect("loaded function")(
                        self.device.handle,
                        &raw const info,
                        ptr::null(),
                        &raw mut view,
                    )
                },
                "vkCreateImageView",
            ) {
                // SAFETY: These views were successfully created and are not in use.
                unsafe {
                    for prior in views {
                        self.device
                            .functions
                            .destroy_image_view
                            .expect("loaded function")(
                            self.device.handle, prior, ptr::null()
                        );
                    }
                }
                return Err(error);
            }
            views.push(view);
        }
        Ok(views)
    }

    #[allow(clippy::too_many_lines)]
    fn create_depth_attachment(&mut self) -> Result<(), ProbeError> {
        let info = vk::VkImageCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
            imageType: vk::VK_IMAGE_TYPE_2D,
            format: self.depth_format,
            extent: vk::VkExtent3D {
                width: self.extent.width,
                height: self.extent.height,
                depth: 1,
            },
            mipLevels: 1,
            arrayLayers: 1,
            samples: vk::VK_SAMPLE_COUNT_1_BIT,
            tiling: vk::VK_IMAGE_TILING_OPTIMAL,
            usage: vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT as u32,
            sharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
            initialLayout: vk::VK_IMAGE_LAYOUT_UNDEFINED,
            ..Default::default()
        };
        check(
            // SAFETY: Device/create info are valid and output storage is writable.
            unsafe {
                self.device.functions.create_image.expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut self.depth.handle,
                )
            },
            "vkCreateImage for depth attachment",
        )?;
        let mut requirements = vk::VkMemoryRequirements::default();
        // SAFETY: The depth image is live and requirements storage is writable.
        unsafe {
            self.device
                .functions
                .get_image_memory_requirements
                .expect("loaded function")(
                self.device.handle,
                self.depth.handle,
                &raw mut requirements,
            );
        }
        let memory_type = self
            .find_memory_type(
                requirements.memoryTypeBits,
                vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            )
            .ok_or_else(|| {
                ProbeError("adapter exposes no device-local depth memory type".into())
            })?;
        let allocation = vk::VkMemoryAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
            allocationSize: requirements.size,
            memoryTypeIndex: memory_type,
            ..Default::default()
        };
        check(
            // SAFETY: Allocation info and output memory storage are valid.
            unsafe {
                self.device
                    .functions
                    .allocate_memory
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const allocation,
                    ptr::null(),
                    &raw mut self.depth.memory,
                )
            },
            "vkAllocateMemory for depth attachment",
        )?;
        check(
            // SAFETY: Image and allocation are compatible at offset zero.
            unsafe {
                self.device
                    .functions
                    .bind_image_memory
                    .expect("loaded function")(
                    self.device.handle,
                    self.depth.handle,
                    self.depth.memory,
                    0,
                )
            },
            "vkBindImageMemory for depth attachment",
        )?;
        let view_info = vk::VkImageViewCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
            image: self.depth.handle,
            viewType: vk::VK_IMAGE_VIEW_TYPE_2D,
            format: self.depth_format,
            components: vk::VkComponentMapping {
                r: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                g: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                b: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                a: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
            },
            subresourceRange: depth_subresource_range(),
            ..Default::default()
        };
        check(
            // SAFETY: The depth image/create info are valid and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_image_view
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const view_info,
                    ptr::null(),
                    &raw mut self.depth.view,
                )
            },
            "vkCreateImageView for depth attachment",
        )
    }

    fn create_pipeline(&mut self) -> Result<(), ProbeError> {
        let layout_info = vk::VkPipelineLayoutCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
            setLayoutCount: 1,
            pSetLayouts: &raw const self.descriptor_set_layout,
            ..Default::default()
        };
        // SAFETY: Device/create info are valid and output is writable.
        check(
            unsafe {
                self.device
                    .functions
                    .create_pipeline_layout
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const layout_info,
                    ptr::null(),
                    &raw mut self.pipeline_layout,
                )
            },
            "vkCreatePipelineLayout",
        )?;

        let vertex = self.create_shader_module(include_bytes!("triangle.vert.spv"))?;
        let fragment = match self.create_shader_module(include_bytes!("triangle.frag.spv")) {
            Ok(module) => module,
            Err(error) => {
                // SAFETY: Vertex module is live and unused.
                unsafe {
                    self.device
                        .functions
                        .destroy_shader_module
                        .expect("loaded function")(
                        self.device.handle, vertex, ptr::null()
                    );
                }
                return Err(error);
            }
        };
        let result = self.create_graphics_pipeline(vertex, fragment);
        // SAFETY: Pipeline creation has finished reading both modules.
        unsafe {
            for module in [vertex, fragment] {
                self.device
                    .functions
                    .destroy_shader_module
                    .expect("loaded function")(
                    self.device.handle, module, ptr::null()
                );
            }
        }
        result
    }

    fn create_shader_module(&self, bytes: &[u8]) -> Result<vk::VkShaderModule, ProbeError> {
        let words = spirv_words(bytes)?;
        let info = vk::VkShaderModuleCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
            codeSize: bytes.len(),
            pCode: words.as_ptr(),
            ..Default::default()
        };
        let mut module = ptr::null_mut();
        // SAFETY: SPIR-V words are aligned/live for the call and output is writable.
        check(
            unsafe {
                self.device
                    .functions
                    .create_shader_module
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut module,
                )
            },
            "vkCreateShaderModule",
        )?;
        Ok(module)
    }

    #[allow(clippy::too_many_lines)]
    fn create_graphics_pipeline(
        &mut self,
        vertex: vk::VkShaderModule,
        fragment: vk::VkShaderModule,
    ) -> Result<(), ProbeError> {
        let stages = [
            shader_stage(vk::VK_SHADER_STAGE_VERTEX_BIT, vertex),
            shader_stage(vk::VK_SHADER_STAGE_FRAGMENT_BIT, fragment),
        ];
        let (binding, attributes) = vertex_input_descriptions();
        let vertex_input = vk::VkPipelineVertexInputStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
            vertexBindingDescriptionCount: 1,
            pVertexBindingDescriptions: &raw const binding,
            vertexAttributeDescriptionCount: u32::try_from(attributes.len())
                .expect("vertex attribute count fits u32"),
            pVertexAttributeDescriptions: attributes.as_ptr(),
            ..Default::default()
        };
        let input_assembly = vk::VkPipelineInputAssemblyStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
            topology: vk::VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,
            ..Default::default()
        };
        let viewport = vk::VkPipelineViewportStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
            viewportCount: 1,
            scissorCount: 1,
            ..Default::default()
        };
        let rasterization = vk::VkPipelineRasterizationStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
            polygonMode: vk::VK_POLYGON_MODE_FILL,
            cullMode: vk::VK_CULL_MODE_NONE as u32,
            frontFace: vk::VK_FRONT_FACE_CLOCKWISE,
            lineWidth: 1.0,
            ..Default::default()
        };
        let multisample = vk::VkPipelineMultisampleStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
            rasterizationSamples: vk::VK_SAMPLE_COUNT_1_BIT,
            ..Default::default()
        };
        let depth_stencil = vk::VkPipelineDepthStencilStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
            depthTestEnable: vk::VK_TRUE,
            depthWriteEnable: vk::VK_TRUE,
            depthCompareOp: vk::VK_COMPARE_OP_LESS,
            minDepthBounds: 0.0,
            maxDepthBounds: 1.0,
            ..Default::default()
        };
        let blend_attachment = vk::VkPipelineColorBlendAttachmentState {
            colorWriteMask: (vk::VK_COLOR_COMPONENT_R_BIT
                | vk::VK_COLOR_COMPONENT_G_BIT
                | vk::VK_COLOR_COMPONENT_B_BIT
                | vk::VK_COLOR_COMPONENT_A_BIT) as u32,
            ..Default::default()
        };
        let blend = vk::VkPipelineColorBlendStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
            attachmentCount: 1,
            pAttachments: &raw const blend_attachment,
            ..Default::default()
        };
        let dynamic_states = [vk::VK_DYNAMIC_STATE_VIEWPORT, vk::VK_DYNAMIC_STATE_SCISSOR];
        let dynamic = vk::VkPipelineDynamicStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
            dynamicStateCount: 2,
            pDynamicStates: dynamic_states.as_ptr(),
            ..Default::default()
        };
        let rendering = vk::VkPipelineRenderingCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
            colorAttachmentCount: 1,
            pColorAttachmentFormats: &raw const self.format,
            depthAttachmentFormat: self.depth_format,
            ..Default::default()
        };
        let info = vk::VkGraphicsPipelineCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
            pNext: (&raw const rendering).cast(),
            stageCount: 2,
            pStages: stages.as_ptr(),
            pVertexInputState: &raw const vertex_input,
            pInputAssemblyState: &raw const input_assembly,
            pViewportState: &raw const viewport,
            pRasterizationState: &raw const rasterization,
            pMultisampleState: &raw const multisample,
            pDepthStencilState: &raw const depth_stencil,
            pColorBlendState: &raw const blend,
            pDynamicState: &raw const dynamic,
            layout: self.pipeline_layout,
            basePipelineIndex: -1,
            ..Default::default()
        };
        // SAFETY: All pipeline state pointers remain live and output is writable.
        check(
            unsafe {
                self.device
                    .functions
                    .create_graphics_pipelines
                    .expect("loaded function")(
                    self.device.handle,
                    ptr::null_mut(),
                    1,
                    &raw const info,
                    ptr::null(),
                    &raw mut self.pipeline,
                )
            },
            "vkCreateGraphicsPipelines",
        )
    }

    fn wait_for_frame(&mut self) -> Result<(), ProbeError> {
        if !self.frame_pending {
            return Ok(());
        }
        // SAFETY: The fence is live and belongs to this device.
        check(
            unsafe {
                self.device
                    .functions
                    .wait_for_fences
                    .expect("loaded function")(
                    self.device.handle,
                    1,
                    &raw const self.frame_fence,
                    vk::VK_TRUE,
                    UINT64_MAX,
                )
            },
            "vkWaitForFences for frame",
        )?;
        self.frame_pending = false;
        Ok(())
    }

    fn wait_and_reset_fence(
        &self,
        fence: vk::VkFence,
        description: &str,
    ) -> Result<(), ProbeError> {
        // SAFETY: The fence is live, and the caller only resets it after its signal was observed.
        check(
            unsafe {
                self.device
                    .functions
                    .wait_for_fences
                    .expect("loaded function")(
                    self.device.handle,
                    1,
                    &raw const fence,
                    vk::VK_TRUE,
                    UINT64_MAX,
                )
            },
            &format!("vkWaitForFences for {description}"),
        )?;
        check(
            unsafe {
                self.device.functions.reset_fences.expect("loaded function")(
                    self.device.handle,
                    1,
                    &raw const fence,
                )
            },
            &format!("vkResetFences for {description}"),
        )
    }

    fn retire_current_swapchain(&mut self, retire_pipeline: bool) {
        if self.swapchain.is_null() {
            return;
        }
        self.retired.push(RetiredSwapchain {
            handle: mem::replace(&mut self.swapchain, ptr::null_mut()),
            views: mem::take(&mut self.views),
            depth: mem::take(&mut self.depth),
            pipeline_layout: if retire_pipeline {
                mem::replace(&mut self.pipeline_layout, ptr::null_mut())
            } else {
                ptr::null_mut()
            },
            pipeline: if retire_pipeline {
                mem::replace(&mut self.pipeline, ptr::null_mut())
            } else {
                ptr::null_mut()
            },
            render_finished: mem::take(&mut self.render_finished),
            present_fences: mem::take(&mut self.present_fences),
            present_pending: mem::take(&mut self.present_pending),
        });
        self.images.clear();
        self.presented.clear();
    }

    fn retired_swapchain_ready(&self, retired: &RetiredSwapchain) -> Result<bool, ProbeError> {
        for (&fence, &pending) in retired.present_fences.iter().zip(&retired.present_pending) {
            if !pending {
                continue;
            }
            // SAFETY: The presentation fence remains live while its status is queried.
            let result = unsafe {
                self.device
                    .functions
                    .get_fence_status
                    .expect("loaded function")(self.device.handle, fence)
            };
            if result == vk::VK_NOT_READY {
                return Ok(false);
            }
            check(result, "vkGetFenceStatus for retired swapchain")?;
        }
        Ok(true)
    }

    fn collect_retired_swapchains(&mut self) -> Result<(), ProbeError> {
        if !self.device.adapter.swapchain_maintenance1 {
            return Ok(());
        }
        let mut index = 0;
        while index < self.retired.len() {
            if self.retired_swapchain_ready(&self.retired[index])? {
                let retired = self.retired.remove(index);
                Self::destroy_retired_swapchain(&self.device, retired);
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    fn destroy_retired_swapchain(device: &DeviceContext, retired: RetiredSwapchain) {
        // SAFETY: Completion was established before this owned resource set reached this helper.
        unsafe {
            let mut depth = retired.depth;
            destroy_gpu_image(device, &mut depth);
            if !retired.pipeline.is_null() {
                device.functions.destroy_pipeline.expect("loaded function")(
                    device.handle,
                    retired.pipeline,
                    ptr::null(),
                );
            }
            if !retired.pipeline_layout.is_null() {
                device
                    .functions
                    .destroy_pipeline_layout
                    .expect("loaded function")(
                    device.handle, retired.pipeline_layout, ptr::null()
                );
            }
            for view in retired.views {
                device
                    .functions
                    .destroy_image_view
                    .expect("loaded function")(device.handle, view, ptr::null());
            }
            for semaphore in retired.render_finished {
                device.functions.destroy_semaphore.expect("loaded function")(
                    device.handle,
                    semaphore,
                    ptr::null(),
                );
            }
            for fence in retired.present_fences {
                device.functions.destroy_fence.expect("loaded function")(
                    device.handle,
                    fence,
                    ptr::null(),
                );
            }
            device.functions.destroy_swapchain.expect("loaded function")(
                device.handle,
                retired.handle,
                ptr::null(),
            );
        }
    }

    fn destroy_all_retired_swapchains(&mut self) {
        for retired in self.retired.drain(..) {
            Self::destroy_retired_swapchain(&self.device, retired);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn render(&mut self, width: u32, height: u32, live_resize: bool) -> Result<bool, ProbeError> {
        let trace_started = self.live_resize_trace.begin(live_resize);
        let mut trace_sample = LiveResizeSample::default();
        let operation_started = Instant::now();
        self.wait_for_frame()?;
        trace_sample.frame_wait = operation_started.elapsed();
        self.collect_retired_swapchains()?;
        if self.swapchain.is_null()
            || self.extent.width != width
            || self.extent.height != height
            || self.recreate_after_present
        {
            let operation_started = Instant::now();
            self.recreate_swapchain(width, height)?;
            trace_sample.recreate = Some(operation_started.elapsed());
        }

        let mut image_index = 0;
        let acquire_fence = if self.device.adapter.swapchain_maintenance1 {
            ptr::null_mut()
        } else {
            self.acquire_fence
        };
        let operation_started = Instant::now();
        // SAFETY: Swapchain and semaphore are live; output is writable.
        let acquire = unsafe {
            self.device
                .functions
                .acquire_next_image
                .expect("loaded function")(
                self.device.handle,
                self.swapchain,
                UINT64_MAX,
                self.image_available,
                acquire_fence,
                &raw mut image_index,
            )
        };
        if acquire == vk::VK_ERROR_OUT_OF_DATE_KHR {
            trace_sample.acquire = operation_started.elapsed();
            let operation_started = Instant::now();
            self.recreate_swapchain(width, height)?;
            let recreate = operation_started.elapsed();
            trace_sample.recreate = Some(
                trace_sample
                    .recreate
                    .map_or(recreate, |previous| previous + recreate),
            );
            self.live_resize_trace
                .finish(trace_started, trace_sample, false);
            return Ok(false);
        }
        if acquire == vk::VK_SUBOPTIMAL_KHR {
            self.recreate_after_present = true;
        } else {
            check(acquire, "vkAcquireNextImageKHR")?;
        }

        let image_slot = image_index as usize;
        if self.device.adapter.swapchain_maintenance1 {
            if self.present_pending[image_slot] {
                self.wait_and_reset_fence(
                    self.present_fences[image_slot],
                    "presentation fence for reacquired image",
                )?;
                self.present_pending[image_slot] = false;
            }
        } else {
            self.wait_and_reset_fence(self.acquire_fence, "image-acquisition fence")?;
            if self.presented[image_slot] {
                self.destroy_all_retired_swapchains();
            }
        }
        trace_sample.acquire = operation_started.elapsed();

        let view = *self
            .views
            .get(image_slot)
            .ok_or_else(|| ProbeError("driver returned an invalid swapchain image index".into()))?;
        let image = self.images[image_slot];
        let render_finished = *self
            .render_finished
            .get(image_slot)
            .ok_or_else(|| ProbeError("swapchain image has no presentation semaphore".into()))?;
        let operation_started = Instant::now();
        // SAFETY: Fence is signaled and command buffer is no longer executing.
        check(
            unsafe {
                self.device.functions.reset_fences.expect("loaded function")(
                    self.device.handle,
                    1,
                    &raw const self.frame_fence,
                )
            },
            "vkResetFences",
        )?;
        check(
            unsafe {
                self.device
                    .functions
                    .reset_command_buffer
                    .expect("loaded function")(self.command_buffer, 0)
            },
            "vkResetCommandBuffer",
        )?;
        self.record(image, view)?;
        self.submit(render_finished)?;
        trace_sample.record_submit = operation_started.elapsed();

        let present_fence = self
            .present_fences
            .get(image_slot)
            .copied()
            .unwrap_or(ptr::null_mut());
        let present_fence_info = vk::VkSwapchainPresentFenceInfoKHR {
            sType: vk::VK_STRUCTURE_TYPE_SWAPCHAIN_PRESENT_FENCE_INFO_KHR,
            swapchainCount: 1,
            pFences: &raw const present_fence,
            ..Default::default()
        };
        let present = vk::VkPresentInfoKHR {
            sType: vk::VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,
            pNext: if present_fence.is_null() {
                ptr::null()
            } else {
                (&raw const present_fence_info).cast()
            },
            waitSemaphoreCount: 1,
            pWaitSemaphores: &raw const render_finished,
            swapchainCount: 1,
            pSwapchains: &raw const self.swapchain,
            pImageIndices: &raw const image_index,
            ..Default::default()
        };
        let operation_started = Instant::now();
        // SAFETY: Queue, swapchain, index, and wait semaphore are valid for this submission.
        let result = unsafe {
            self.device
                .functions
                .queue_present
                .expect("loaded function")(self.device.queue, &raw const present)
        };
        trace_sample.present = operation_started.elapsed();
        if result == vk::VK_SUCCESS
            || result == vk::VK_ERROR_OUT_OF_DATE_KHR
            || result == vk::VK_SUBOPTIMAL_KHR
        {
            if self.device.adapter.swapchain_maintenance1 {
                self.present_pending[image_slot] = true;
            } else {
                self.presented[image_slot] = true;
            }
        }
        if result == vk::VK_ERROR_OUT_OF_DATE_KHR || result == vk::VK_SUBOPTIMAL_KHR {
            self.recreate_after_present = true;
        } else {
            check(result, "vkQueuePresentKHR")?;
        }
        self.live_resize_trace
            .finish(trace_started, trace_sample, true);
        Ok(true)
    }

    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    fn record(&self, image: vk::VkImage, view: vk::VkImageView) -> Result<(), ProbeError> {
        let begin = vk::VkCommandBufferBeginInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            flags: vk::VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT as u32,
            ..Default::default()
        };
        // SAFETY: Command buffer has been reset and begin info is valid.
        check(
            unsafe {
                self.device
                    .functions
                    .begin_command_buffer
                    .expect("loaded function")(self.command_buffer, &raw const begin)
            },
            "vkBeginCommandBuffer",
        )?;

        self.image_barrier(
            image,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_NONE,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            color_subresource_range(),
        );
        self.image_barrier(
            self.depth.handle,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_NONE,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
            vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_READ_BIT
                | vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            depth_subresource_range(),
        );
        let attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: view,
            imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            resolveMode: vk::VK_RESOLVE_MODE_NONE,
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: vk::VkClearValue {
                color: vk::VkClearColorValue {
                    float32: [0.025, 0.035, 0.055, 1.0],
                },
            },
            ..Default::default()
        };
        let depth_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: self.depth.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            resolveMode: vk::VK_RESOLVE_MODE_NONE,
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: vk::VkClearValue {
                depthStencil: vk::VkClearDepthStencilValue {
                    depth: 1.0,
                    stencil: 0,
                },
            },
            ..Default::default()
        };
        let render_area = vk::VkRect2D {
            offset: vk::VkOffset2D { x: 0, y: 0 },
            extent: self.extent,
        };
        let rendering = vk::VkRenderingInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
            renderArea: render_area,
            layerCount: 1,
            colorAttachmentCount: 1,
            pColorAttachments: &raw const attachment,
            pDepthAttachment: &raw const depth_attachment,
            ..Default::default()
        };
        let viewport = vk::VkViewport {
            x: 0.0,
            y: 0.0,
            width: self.extent.width as f32,
            height: self.extent.height as f32,
            minDepth: 0.0,
            maxDepth: 1.0,
        };
        // SAFETY: Recording state is valid and all referenced objects are live.
        unsafe {
            self.device
                .functions
                .cmd_begin_rendering
                .expect("loaded function")(self.command_buffer, &raw const rendering);
            self.device
                .functions
                .cmd_bind_pipeline
                .expect("loaded function")(
                self.command_buffer,
                vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                self.pipeline,
            );
            self.device
                .functions
                .cmd_bind_descriptor_sets
                .expect("loaded function")(
                self.command_buffer,
                vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                self.pipeline_layout,
                0,
                1,
                &raw const self.descriptor_set,
                0,
                ptr::null(),
            );
            self.device
                .functions
                .cmd_set_viewport
                .expect("loaded function")(
                self.command_buffer, 0, 1, &raw const viewport
            );
            self.device
                .functions
                .cmd_set_scissor
                .expect("loaded function")(
                self.command_buffer, 0, 1, &raw const render_area
            );
            let vertex_offset: vk::VkDeviceSize = 0;
            self.device
                .functions
                .cmd_bind_vertex_buffers
                .expect("loaded function")(
                self.command_buffer,
                0,
                1,
                &raw const self.vertex_buffer.handle,
                &raw const vertex_offset,
            );
            self.device
                .functions
                .cmd_bind_index_buffer
                .expect("loaded function")(
                self.command_buffer,
                self.index_buffer.handle,
                0,
                vk::VK_INDEX_TYPE_UINT16,
            );
            self.device
                .functions
                .cmd_draw_indexed
                .expect("loaded function")(
                self.command_buffer,
                u32::try_from(TRIANGLE_INDICES.len()).expect("index count fits u32"),
                1,
                0,
                0,
                0,
            );
            self.device
                .functions
                .cmd_end_rendering
                .expect("loaded function")(self.command_buffer);
        }
        self.image_barrier(
            image,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_NONE,
            vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
            color_subresource_range(),
        );
        // SAFETY: The command buffer is recording and the render scope has ended.
        check(
            unsafe {
                self.device
                    .functions
                    .end_command_buffer
                    .expect("loaded function")(self.command_buffer)
            },
            "vkEndCommandBuffer",
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn image_barrier(
        &self,
        image: vk::VkImage,
        source_stage: vk::VkPipelineStageFlags2,
        source_access: vk::VkAccessFlags2,
        old_layout: vk::VkImageLayout,
        destination_stage: vk::VkPipelineStageFlags2,
        destination_access: vk::VkAccessFlags2,
        new_layout: vk::VkImageLayout,
        subresource_range: vk::VkImageSubresourceRange,
    ) {
        let barrier = vk::VkImageMemoryBarrier2 {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
            srcStageMask: source_stage,
            srcAccessMask: source_access,
            dstStageMask: destination_stage,
            dstAccessMask: destination_access,
            oldLayout: old_layout,
            newLayout: new_layout,
            srcQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
            dstQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
            image,
            subresourceRange: subresource_range,
            ..Default::default()
        };
        let dependency = vk::VkDependencyInfo {
            sType: vk::VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
            imageMemoryBarrierCount: 1,
            pImageMemoryBarriers: &raw const barrier,
            ..Default::default()
        };
        // SAFETY: The command buffer is recording and the barrier references its acquired image.
        unsafe {
            self.device
                .functions
                .cmd_pipeline_barrier2
                .expect("loaded function")(self.command_buffer, &raw const dependency);
        }
    }

    fn submit(&mut self, render_finished: vk::VkSemaphore) -> Result<(), ProbeError> {
        let wait = vk::VkSemaphoreSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_SEMAPHORE_SUBMIT_INFO,
            semaphore: self.image_available,
            stageMask: vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            ..Default::default()
        };
        let command = command_buffer_submit_info(self.command_buffer);
        let signal = vk::VkSemaphoreSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_SEMAPHORE_SUBMIT_INFO,
            semaphore: render_finished,
            stageMask: vk::VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
            ..Default::default()
        };
        let submit = vk::VkSubmitInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_SUBMIT_INFO_2,
            waitSemaphoreInfoCount: 1,
            pWaitSemaphoreInfos: &raw const wait,
            commandBufferInfoCount: 1,
            pCommandBufferInfos: &raw const command,
            signalSemaphoreInfoCount: 1,
            pSignalSemaphoreInfos: &raw const signal,
            ..Default::default()
        };
        // SAFETY: All submission objects and synchronization primitives are live.
        check(
            unsafe {
                self.device
                    .functions
                    .queue_submit2
                    .expect("loaded function")(
                    self.device.queue,
                    1,
                    &raw const submit,
                    self.frame_fence,
                )
            },
            "vkQueueSubmit2",
        )?;
        self.frame_pending = true;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), ProbeError> {
        self.live_resize_trace.report();
        if !self.device.adapter.swapchain_maintenance1 {
            // Base VK_KHR_swapchain lacks a portable fence for presentation completion at final
            // shutdown, so the compatibility path retains the conventional orderly-idle fallback.
            return check(
                unsafe {
                    self.device
                        .functions
                        .device_wait_idle
                        .expect("loaded function")(self.device.handle)
                },
                "vkDeviceWaitIdle",
            );
        }

        self.wait_for_frame()?;
        for (&fence, &pending) in self.present_fences.iter().zip(&self.present_pending) {
            if pending {
                // SAFETY: The live fence was attached to an enqueued presentation request.
                check(
                    unsafe {
                        self.device
                            .functions
                            .wait_for_fences
                            .expect("loaded function")(
                            self.device.handle,
                            1,
                            &raw const fence,
                            vk::VK_TRUE,
                            UINT64_MAX,
                        )
                    },
                    "vkWaitForFences for presentation at shutdown",
                )?;
            }
        }
        for retired in &self.retired {
            for (&fence, &pending) in retired.present_fences.iter().zip(&retired.present_pending) {
                if pending {
                    // SAFETY: The retired swapchain owns this live presentation fence.
                    check(
                        unsafe {
                            self.device
                                .functions
                                .wait_for_fences
                                .expect("loaded function")(
                                self.device.handle,
                                1,
                                &raw const fence,
                                vk::VK_TRUE,
                                UINT64_MAX,
                            )
                        },
                        "vkWaitForFences for retired presentation at shutdown",
                    )?;
                }
            }
        }
        Ok(())
    }

    fn destroy_swapchain_resources(&mut self) {
        self.retire_current_swapchain(true);
        self.destroy_all_retired_swapchains();
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let _ = self.finish();
        self.destroy_swapchain_resources();
        let mut vertex_buffer = mem::take(&mut self.vertex_buffer);
        let mut index_buffer = mem::take(&mut self.index_buffer);
        let mut texture = mem::take(&mut self.texture);
        // SAFETY: `finish` completed all submitted GPU work before these owned buffers are freed.
        unsafe {
            if !self.descriptor_pool.is_null() {
                self.device
                    .functions
                    .destroy_descriptor_pool
                    .expect("loaded function")(
                    self.device.handle,
                    self.descriptor_pool,
                    ptr::null(),
                );
            }
            if !self.texture_sampler.is_null() {
                self.device
                    .functions
                    .destroy_sampler
                    .expect("loaded function")(
                    self.device.handle,
                    self.texture_sampler,
                    ptr::null(),
                );
            }
            self.destroy_image(&mut texture);
            if !self.descriptor_set_layout.is_null() {
                self.device
                    .functions
                    .destroy_descriptor_set_layout
                    .expect("loaded function")(
                    self.device.handle,
                    self.descriptor_set_layout,
                    ptr::null(),
                );
            }
            self.destroy_buffer(&mut vertex_buffer);
            self.destroy_buffer(&mut index_buffer);
        }
        // SAFETY: Frame resources are owned by this renderer and destroyed once after GPU idle.
        unsafe {
            if !self.frame_fence.is_null() {
                self.device
                    .functions
                    .destroy_fence
                    .expect("loaded function")(
                    self.device.handle, self.frame_fence, ptr::null()
                );
            }
            if !self.acquire_fence.is_null() {
                self.device
                    .functions
                    .destroy_fence
                    .expect("loaded function")(
                    self.device.handle, self.acquire_fence, ptr::null()
                );
            }
            if !self.image_available.is_null() {
                self.device
                    .functions
                    .destroy_semaphore
                    .expect("loaded function")(
                    self.device.handle,
                    self.image_available,
                    ptr::null(),
                );
            }
            if !self.command_pool.is_null() {
                self.device
                    .functions
                    .destroy_command_pool
                    .expect("loaded function")(
                    self.device.handle, self.command_pool, ptr::null()
                );
            }
        }
    }
}

unsafe fn destroy_gpu_image(device: &DeviceContext, image: &mut GpuImage) {
    if !image.view.is_null() {
        // SAFETY: The view is owned by this resource and no longer in GPU use.
        unsafe {
            device
                .functions
                .destroy_image_view
                .expect("loaded function")(device.handle, image.view, ptr::null());
        }
        image.view = ptr::null_mut();
    }
    if !image.handle.is_null() {
        // SAFETY: The image is owned by this resource and no longer in GPU use.
        unsafe {
            device.functions.destroy_image.expect("loaded function")(
                device.handle,
                image.handle,
                ptr::null(),
            );
        }
        image.handle = ptr::null_mut();
    }
    if !image.memory.is_null() {
        // SAFETY: The bound image was destroyed before its allocation is freed.
        unsafe {
            device.functions.free_memory.expect("loaded function")(
                device.handle,
                image.memory,
                ptr::null(),
            );
        }
        image.memory = ptr::null_mut();
    }
}

fn choose_depth_format(device: &DeviceContext) -> Result<vk::VkFormat, ProbeError> {
    for format in [
        vk::VK_FORMAT_D32_SFLOAT,
        vk::VK_FORMAT_D24_UNORM_S8_UINT,
        vk::VK_FORMAT_D16_UNORM,
    ] {
        let mut properties = vk::VkFormatProperties::default();
        // SAFETY: The physical device is live and properties storage is writable.
        unsafe {
            device
                .instance
                .functions
                .get_physical_device_format_properties
                .expect("loaded function")(
                device.adapter.handle, format, &raw mut properties
            );
        }
        if properties.optimalTilingFeatures
            & vk::VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT as u32
            != 0
        {
            return Ok(format);
        }
    }
    Err(ProbeError(
        "adapter exposes no supported optimal-tiled depth attachment format".into(),
    ))
}

fn depth_format_name(format: vk::VkFormat) -> &'static str {
    match format {
        vk::VK_FORMAT_D32_SFLOAT => "D32_SFLOAT",
        vk::VK_FORMAT_D24_UNORM_S8_UINT => "D24_UNORM_S8_UINT",
        vk::VK_FORMAT_D16_UNORM => "D16_UNORM",
        _ => "unknown depth format",
    }
}

fn choose_surface_format(formats: &[vk::VkSurfaceFormatKHR]) -> Option<vk::VkSurfaceFormatKHR> {
    formats
        .iter()
        .copied()
        .find(|format| {
            format.format == vk::VK_FORMAT_B8G8R8A8_SRGB
                && format.colorSpace == vk::VK_COLOR_SPACE_SRGB_NONLINEAR_KHR
        })
        .or_else(|| {
            formats.first().copied().map(|format| {
                if format.format == vk::VK_FORMAT_UNDEFINED {
                    vk::VkSurfaceFormatKHR {
                        format: vk::VK_FORMAT_B8G8R8A8_SRGB,
                        colorSpace: vk::VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
                    }
                } else {
                    format
                }
            })
        })
}

fn choose_extent(
    capabilities: vk::VkSurfaceCapabilitiesKHR,
    width: u32,
    height: u32,
) -> vk::VkExtent2D {
    if capabilities.currentExtent.width != UINT32_MAX {
        return capabilities.currentExtent;
    }
    vk::VkExtent2D {
        width: width.clamp(
            capabilities.minImageExtent.width,
            capabilities.maxImageExtent.width,
        ),
        height: height.clamp(
            capabilities.minImageExtent.height,
            capabilities.maxImageExtent.height,
        ),
    }
}

fn choose_composite_alpha(
    flags: vk::VkCompositeAlphaFlagsKHR,
) -> Option<vk::VkCompositeAlphaFlagBitsKHR> {
    [
        vk::VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR,
        vk::VK_COMPOSITE_ALPHA_PRE_MULTIPLIED_BIT_KHR,
        vk::VK_COMPOSITE_ALPHA_POST_MULTIPLIED_BIT_KHR,
        vk::VK_COMPOSITE_ALPHA_INHERIT_BIT_KHR,
    ]
    .into_iter()
    .find(|mode| flags & u32::try_from(*mode).expect("positive bit flag") != 0)
}

fn shader_stage(
    stage: vk::VkShaderStageFlagBits,
    module: vk::VkShaderModule,
) -> vk::VkPipelineShaderStageCreateInfo {
    vk::VkPipelineShaderStageCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
        stage,
        module,
        pName: c"main".as_ptr(),
        ..Default::default()
    }
}

fn vertex_input_descriptions() -> (
    vk::VkVertexInputBindingDescription,
    [vk::VkVertexInputAttributeDescription; 3],
) {
    let binding = vk::VkVertexInputBindingDescription {
        binding: 0,
        stride: u32::try_from(mem::size_of::<Vertex>()).expect("vertex stride fits u32"),
        inputRate: vk::VK_VERTEX_INPUT_RATE_VERTEX,
    };
    let attributes = [
        vk::VkVertexInputAttributeDescription {
            location: 0,
            binding: 0,
            format: vk::VK_FORMAT_R32G32_SFLOAT,
            offset: 0,
        },
        vk::VkVertexInputAttributeDescription {
            location: 1,
            binding: 0,
            format: vk::VK_FORMAT_R32G32B32_SFLOAT,
            offset: u32::try_from(mem::size_of::<[f32; 2]>())
                .expect("vertex attribute offset fits u32"),
        },
        vk::VkVertexInputAttributeDescription {
            location: 2,
            binding: 0,
            format: vk::VK_FORMAT_R32G32_SFLOAT,
            offset: u32::try_from(mem::size_of::<[f32; 2]>() + mem::size_of::<[f32; 3]>())
                .expect("vertex attribute offset fits u32"),
        },
    ];
    (binding, attributes)
}

fn color_subresource_range() -> vk::VkImageSubresourceRange {
    vk::VkImageSubresourceRange {
        aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
        baseMipLevel: 0,
        levelCount: 1,
        baseArrayLayer: 0,
        layerCount: 1,
    }
}

fn depth_subresource_range() -> vk::VkImageSubresourceRange {
    vk::VkImageSubresourceRange {
        aspectMask: vk::VK_IMAGE_ASPECT_DEPTH_BIT as u32,
        baseMipLevel: 0,
        levelCount: 1,
        baseArrayLayer: 0,
        layerCount: 1,
    }
}

fn command_buffer_submit_info(
    command_buffer: vk::VkCommandBuffer,
) -> vk::VkCommandBufferSubmitInfo {
    vk::VkCommandBufferSubmitInfo {
        sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO,
        commandBuffer: command_buffer,
        // Zero selects every valid physical device, including the normal single-device case.
        deviceMask: 0,
        ..Default::default()
    }
}

fn buffer_barrier(
    buffer: vk::VkBuffer,
    size: vk::VkDeviceSize,
    destination_access: vk::VkAccessFlags2,
) -> vk::VkBufferMemoryBarrier2 {
    vk::VkBufferMemoryBarrier2 {
        sType: vk::VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER_2,
        srcStageMask: vk::VK_PIPELINE_STAGE_2_COPY_BIT,
        srcAccessMask: vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
        dstStageMask: vk::VK_PIPELINE_STAGE_2_VERTEX_INPUT_BIT,
        dstAccessMask: destination_access,
        srcQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
        dstQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
        buffer,
        offset: 0,
        size,
        ..Default::default()
    }
}

unsafe fn slice_bytes<T>(values: &[T]) -> &[u8] {
    let byte_len = mem::size_of_val(values);
    // SAFETY: The caller guarantees every byte in each value is initialized. The returned byte
    // slice has the same lifetime and exact extent as the input slice.
    unsafe { std::slice::from_raw_parts(values.as_ptr().cast(), byte_len) }
}

fn find_memory_type(
    properties: &vk::VkPhysicalDeviceMemoryProperties,
    compatible_bits: u32,
    required_flags: u32,
) -> Option<u32> {
    (0..properties.memoryTypeCount).find(|&index| {
        let compatible = compatible_bits & (1_u32 << index) != 0;
        let flags = properties.memoryTypes[index as usize].propertyFlags;
        compatible && flags & required_flags == required_flags
    })
}

fn spirv_words(bytes: &[u8]) -> Result<Vec<u32>, ProbeError> {
    if !bytes.len().is_multiple_of(mem::size_of::<u32>()) {
        return Err(ProbeError(
            "SPIR-V byte length is not divisible by four".into(),
        ));
    }
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().expect("four-byte chunk")))
        .collect();
    if words.first() != Some(&0x0723_0203) {
        return Err(ProbeError("shader does not contain SPIR-V magic".into()));
    }
    Ok(words)
}

fn fixed_c_string<const N: usize>(bytes: &[c_char; N]) -> Vec<u8> {
    bytes
        .iter()
        .map(|byte| byte.cast_unsigned())
        .take_while(|byte| *byte != 0)
        .collect()
}

fn check(result: vk::VkResult, operation: &str) -> Result<(), ProbeError> {
    if result == vk::VK_SUCCESS {
        Ok(())
    } else {
        Err(ProbeError(format!(
            "{operation} failed with VkResult {result}"
        )))
    }
}

fn check_enumeration(result: vk::VkResult, operation: &str) -> Result<(), ProbeError> {
    if result == vk::VK_SUCCESS || result == vk::VK_INCOMPLETE {
        Ok(())
    } else {
        Err(ProbeError(format!(
            "{operation} failed with VkResult {result}"
        )))
    }
}

unsafe fn cast_address<T: Copy>(address: *mut c_void, name: &str) -> Result<T, ProbeError> {
    if address.is_null() {
        return Err(ProbeError(format!("Vulkan loader does not export {name}")));
    }
    assert_eq!(mem::size_of::<T>(), mem::size_of::<*mut c_void>());
    // SAFETY: The caller pairs the exported symbol name with the generated function-pointer type.
    Ok(unsafe { mem::transmute_copy(&address) })
}

unsafe fn load_proc<T: Copy>(
    function: vk::PFN_vkVoidFunction,
    name: &str,
) -> Result<T, ProbeError> {
    if function.is_none() {
        return Err(ProbeError(format!("Vulkan function {name} is unavailable")));
    }
    assert_eq!(
        mem::size_of::<T>(),
        mem::size_of::<vk::PFN_vkVoidFunction>()
    );
    // SAFETY: The caller pairs the Vulkan function name with its generated pointer type.
    Ok(unsafe { mem::transmute_copy(&function) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variable_extent_capabilities() -> vk::VkSurfaceCapabilitiesKHR {
        vk::VkSurfaceCapabilitiesKHR {
            currentExtent: vk::VkExtent2D {
                width: UINT32_MAX,
                height: UINT32_MAX,
            },
            minImageExtent: vk::VkExtent2D {
                width: 64,
                height: 64,
            },
            maxImageExtent: vk::VkExtent2D {
                width: 4096,
                height: 2160,
            },
            ..Default::default()
        }
    }

    #[test]
    fn extent_uses_surface_fixed_size() {
        let capabilities = vk::VkSurfaceCapabilitiesKHR {
            currentExtent: vk::VkExtent2D {
                width: 1920,
                height: 1080,
            },
            ..Default::default()
        };
        let extent = choose_extent(capabilities, 800, 600);
        assert_eq!((extent.width, extent.height), (1920, 1080));
    }

    #[test]
    fn extent_clamps_variable_size() {
        let extent = choose_extent(variable_extent_capabilities(), 8, 9000);
        assert_eq!((extent.width, extent.height), (64, 2160));
    }

    #[test]
    fn spirv_rejects_invalid_input() {
        assert!(spirv_words(&[1, 2, 3]).is_err());
        assert!(spirv_words(&[0, 0, 0, 0]).is_err());
    }

    #[test]
    fn undefined_surface_format_selects_srgb() {
        let format = choose_surface_format(&[vk::VkSurfaceFormatKHR {
            format: vk::VK_FORMAT_UNDEFINED,
            colorSpace: vk::VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
        }])
        .expect("one format");
        assert_eq!(format.format, vk::VK_FORMAT_B8G8R8A8_SRGB);
        assert_eq!(format.colorSpace, vk::VK_COLOR_SPACE_SRGB_NONLINEAR_KHR);
    }

    #[test]
    fn memory_type_requires_compatibility_and_all_flags() {
        let mut properties = vk::VkPhysicalDeviceMemoryProperties {
            memoryTypeCount: 3,
            ..Default::default()
        };
        properties.memoryTypes[0].propertyFlags = vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT as u32;
        properties.memoryTypes[1].propertyFlags = (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
            | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
            as u32;
        properties.memoryTypes[2].propertyFlags = (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
            | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT
            | vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)
            as u32;
        let required = (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
            | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) as u32;

        assert_eq!(find_memory_type(&properties, 0b111, required), Some(1));
        assert_eq!(find_memory_type(&properties, 0b100, required), Some(2));
        assert_eq!(find_memory_type(&properties, 0b001, required), None);
        assert_eq!(
            find_memory_type(
                &properties,
                0b111,
                vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            ),
            Some(2)
        );
    }

    #[test]
    fn vertex_layout_matches_pipeline_descriptions() {
        let (binding, attributes) = vertex_input_descriptions();
        assert_eq!(mem::size_of::<Vertex>(), 28);
        assert_eq!(binding.stride, 28);
        assert_eq!(attributes[0].offset, 0);
        assert_eq!(attributes[1].offset, 8);
        assert_eq!(attributes[2].offset, 20);
    }

    #[test]
    fn geometry_upload_barrier_makes_transfer_writes_readable() {
        let barrier = buffer_barrier(
            ptr::null_mut(),
            64,
            vk::VK_ACCESS_2_VERTEX_ATTRIBUTE_READ_BIT,
        );
        assert_eq!(barrier.srcStageMask, vk::VK_PIPELINE_STAGE_2_COPY_BIT);
        assert_eq!(barrier.srcAccessMask, vk::VK_ACCESS_2_TRANSFER_WRITE_BIT);
        assert_eq!(
            barrier.dstStageMask,
            vk::VK_PIPELINE_STAGE_2_VERTEX_INPUT_BIT
        );
        assert_eq!(
            barrier.dstAccessMask,
            vk::VK_ACCESS_2_VERTEX_ATTRIBUTE_READ_BIT
        );
        assert_eq!(barrier.offset, 0);
        assert_eq!(barrier.size, 64);
    }
}
