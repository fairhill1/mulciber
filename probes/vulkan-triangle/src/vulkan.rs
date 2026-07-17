use std::env;
use std::ffi::{CStr, c_char, c_void};
use std::fmt;
use std::fs;
use std::mem;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use mulciber::{FrameAcquire, FrameDisposition, SurfaceExtent, SurfaceInfo, SurfaceUnavailable};

use crate::platform::{self, Window};
use crate::vk;

mod options;
mod pipeline_cache;
mod renderer_assets;
mod renderer_cache;
mod renderer_compute;
mod renderer_descriptors;
mod renderer_frame;
mod renderer_instrumentation;
mod renderer_pipelines;
mod renderer_present;
mod renderer_retirement;
mod renderer_setup;
mod renderer_transfer;
mod resize_trace;
mod texture;

use options::{RunOptions, parse_run_options};
use pipeline_cache::{
    PipelineCacheIdentity, default_path as pipeline_cache_default_path, replace_file_atomically,
    uuid_hex as pipeline_cache_uuid_hex, validate_header as validate_pipeline_cache_header,
};
use resize_trace::{LiveResizeSample, LiveResizeTrace};
use texture::{
    Bc1Support, TEXTURE_HEIGHT, TEXTURE_WIDTH, TextureMode, TexturePath, TextureSelection,
    missing_bc1_requirements, select_texture, texture_mode_from_environment,
};

const API_VERSION_1_3: u32 = make_api_version(0, 1, 3, 0);
const API_VERSION_1_4: u32 = make_api_version(0, 1, 4, 0);
const UINT32_MAX: u32 = u32::MAX;
const UINT64_MAX: u64 = u64::MAX;
const FRAME_SLOT_COUNT: usize = 3;
const STORAGE_VALUE_COUNT: usize = 64;
const COMPUTE_IMAGE_WIDTH: u32 = 8;
const COMPUTE_IMAGE_HEIGHT: u32 = 8;
const COMPUTE_IMAGE_MIP_LEVELS: u32 = 4;
const SHADOW_MAP_SIZE: u32 = 1024;
const RGBA8_TEXEL_SIZE: usize = 4;
const OFFSCREEN_FORMAT: vk::VkFormat = vk::VK_FORMAT_R8G8B8A8_UNORM;
const GPU_QUERY_COUNT: u32 = 8;
const COMPUTE_QUERY_START: u32 = 0;
const COMPUTE_QUERY_END: u32 = 1;
const SHADOW_QUERY_START: u32 = 2;
const SHADOW_QUERY_END: u32 = 3;
const SCENE_QUERY_START: u32 = 4;
const SCENE_QUERY_END: u32 = 5;
const POST_QUERY_START: u32 = 6;
const POST_QUERY_END: u32 = 7;
static VALIDATION_MESSAGE_COUNT: AtomicU32 = AtomicU32::new(0);

fn next_surface_info(
    current: Option<SurfaceInfo>,
    extent: SurfaceExtent,
) -> Result<SurfaceInfo, ProbeError> {
    current
        .map_or_else(
            || SurfaceInfo::initial(extent),
            |info| info.reconfigured(extent),
        )
        .ok_or_else(|| {
            ProbeError(if extent.is_empty() {
                "Vulkan produced an empty configured surface extent".into()
            } else {
                "Vulkan surface generation space is exhausted".into()
            })
        })
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 3],
    uv: [f32; 2],
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct FrameUniform {
    transform: [[f32; 4]; 4],
    tint_time: [f32; 4],
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
#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
}

#[cfg(target_os = "linux")]
const RTLD_NOW: i32 = 2;

#[cfg(target_os = "linux")]
#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> i32;
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
struct GpuTimingSummary {
    frame_query_pending: bool,
    reported: bool,
    samples: u64,
    shadow_total_ms: f64,
    scene_total_ms: f64,
    post_total_ms: f64,
}

pub fn run() -> Result<(), ProbeError> {
    VALIDATION_MESSAGE_COUNT.store(0, Ordering::Relaxed);
    let options = parse_run_options(env::args().skip(1))?;
    let frame_limit = options.frame_limit;
    let mut application = platform::Application::new(options.platform.as_deref())
        .map_err(|error| ProbeError(error.to_string()))?;
    let window = application
        .create_window("Mulciber — native Vulkan 1.3+", 960, 540, true)
        .map_err(|error| ProbeError(error.to_string()))?;
    let entry = Entry::load()?;
    let instance = InstanceContext::new(entry, &window)?;
    let device = DeviceContext::new(instance)?;
    let mut renderer = Renderer::new(device, &window, &options)?;
    let mut rendered_extent = window
        .client_extent()
        .map_err(|error| ProbeError(error.to_string()))?;

    let render_result = (|| {
        let mut rendered_frames = 0;
        let mut last_resize_commit = None;
        loop {
            let mut live_resize_error = None;
            let mut frame_limit_reached = false;
            let keep_running = application
                .pump_events(&window, &mut || {
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
                            rendered_extent = window.client_extent().unwrap_or(rendered_extent);
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
            let live_resize = rendered_extent != (width, height);
            if live_resize
                && last_resize_commit.is_some_and(|last: Instant| {
                    last.elapsed() < platform::resize_commit_interval(&window)
                })
            {
                // Wayland swapchain recreation creates fresh images and can otherwise bypass FIFO
                // backpressure, queuing obsolete surface commits faster than the compositor scans
                // them out. Keep pumping protocol events and render the newest size at frame pace.
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            let resize_commit_started = live_resize.then(Instant::now);
            if renderer.render(width, height, live_resize)? {
                rendered_extent = (width, height);
                if let Some(started) = resize_commit_started {
                    last_resize_commit = Some(started);
                }
                rendered_frames += 1;
                if frame_limit.is_some_and(|limit| rendered_frames >= limit.get()) {
                    break;
                }
            } else {
                thread::sleep(Duration::from_millis(1));
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

struct Entry {
    _library: VulkanLibrary,
    api_version: u32,
    get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    enumerate_instance_version: vk::PFN_vkEnumerateInstanceVersion,
    enumerate_instance_layer_properties: vk::PFN_vkEnumerateInstanceLayerProperties,
    enumerate_instance_extension_properties: vk::PFN_vkEnumerateInstanceExtensionProperties,
    create_instance: vk::PFN_vkCreateInstance,
}

impl Entry {
    fn load() -> Result<Self, ProbeError> {
        let library = VulkanLibrary::open()?;
        // SAFETY: The loaded Vulkan loader exports vkGetInstanceProcAddr with the generated ABI.
        let get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr = unsafe {
            let address = library.symbol(c"vkGetInstanceProcAddr");
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
            _library: library,
            api_version: 0,
            get_instance_proc_addr,
            enumerate_instance_version,
            enumerate_instance_layer_properties,
            enumerate_instance_extension_properties,
            create_instance,
        };
        let mut entry = entry;
        entry.api_version = entry.require_version()?;
        Ok(entry)
    }

    fn require_version(&self) -> Result<u32, ProbeError> {
        let mut version = 0;
        // SAFETY: The output pointer is writable and the loaded function has the generated ABI.
        check(
            unsafe { self.enumerate_instance_version.expect("loaded function")(&raw mut version) },
            "vkEnumerateInstanceVersion",
        )?;
        if version < API_VERSION_1_3 {
            return Err(ProbeError(format!(
                "Vulkan loader exposes {}.{}.{}, but Mulciber requires 1.3",
                version >> 22,
                (version >> 12) & 0x3ff,
                version & 0xfff
            )));
        }
        Ok(version.min(API_VERSION_1_4))
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

struct VulkanLibrary(*mut c_void);

impl VulkanLibrary {
    fn open() -> Result<Self, ProbeError> {
        #[cfg(target_os = "windows")]
        {
            let name: Vec<u16> = "vulkan-1.dll".encode_utf16().chain(Some(0)).collect();
            // SAFETY: The UTF-16 library name is NUL-terminated.
            let library = unsafe { LoadLibraryW(name.as_ptr()) };
            if library.is_null() {
                Err(ProbeError(
                    "could not load vulkan-1.dll; install a Vulkan 1.3 driver".into(),
                ))
            } else {
                Ok(Self(library))
            }
        }
        #[cfg(target_os = "linux")]
        {
            // SAFETY: The library name is static and NUL-terminated.
            let library = unsafe { dlopen(c"libvulkan.so.1".as_ptr(), RTLD_NOW) };
            if library.is_null() {
                Err(ProbeError(
                    "could not load libvulkan.so.1; install a Vulkan 1.3 loader and driver".into(),
                ))
            } else {
                Ok(Self(library))
            }
        }
    }

    unsafe fn symbol(&self, name: &CStr) -> *mut c_void {
        #[cfg(target_os = "windows")]
        {
            // SAFETY: The module is live and the symbol name is NUL-terminated.
            unsafe { GetProcAddress(self.0, name.as_ptr()) }
        }
        #[cfg(target_os = "linux")]
        {
            // SAFETY: The loader is live and the symbol name is NUL-terminated.
            unsafe { dlsym(self.0, name.as_ptr()) }
        }
    }
}

impl Drop for VulkanLibrary {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        // SAFETY: The module is owned and every loaded Vulkan child has already been destroyed.
        unsafe {
            FreeLibrary(self.0);
        }
        #[cfg(target_os = "linux")]
        // SAFETY: The module is owned and every loaded Vulkan child has already been destroyed.
        unsafe {
            dlclose(self.0);
        }
    }
}

struct InstanceFns {
    destroy_instance: vk::PFN_vkDestroyInstance,
    create_debug_utils_messenger: vk::PFN_vkCreateDebugUtilsMessengerEXT,
    destroy_debug_utils_messenger: vk::PFN_vkDestroyDebugUtilsMessengerEXT,
    create_surface: platform::SurfaceFunction,
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
    unsafe fn load(
        entry: &Entry,
        instance: vk::VkInstance,
        window: &Window,
    ) -> Result<Self, ProbeError> {
        macro_rules! load {
            ($name:literal) => {
                unsafe { entry.instance_proc(instance, $name) }?
            };
        }
        Ok(Self {
            destroy_instance: load!(c"vkDestroyInstance"),
            create_debug_utils_messenger: load!(c"vkCreateDebugUtilsMessengerEXT"),
            destroy_debug_utils_messenger: load!(c"vkDestroyDebugUtilsMessengerEXT"),
            create_surface: unsafe {
                entry.instance_proc(instance, platform::create_surface_name(window))
            }?,
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
            (
                platform::surface_extension(window),
                platform::surface_description(window),
            ),
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
            apiVersion: entry.api_version,
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
            platform::surface_extension(window).as_ptr(),
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
        let functions = unsafe { InstanceFns::load(&entry, handle, window) }?;
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

        // SAFETY: Native window handles and the Vulkan instance are live; output is writable.
        check(
            unsafe {
                platform::create_surface(
                    context.functions.create_surface,
                    context.handle,
                    window,
                    &raw mut context.surface,
                )
            },
            platform::create_surface_name(window)
                .to_str()
                .expect("Vulkan symbol names are UTF-8"),
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
    pipeline_creation_cache_control: bool,
    pipeline_cache_identity: PipelineCacheIdentity,
    sample_count: vk::VkSampleCountFlagBits,
    timestamp_valid_bits: u32,
    timestamp_period: f32,
    texture: TextureSelection,
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
    release_swapchain_images: vk::PFN_vkReleaseSwapchainImagesKHR,
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
    create_pipeline_cache: vk::PFN_vkCreatePipelineCache,
    create_query_pool: vk::PFN_vkCreateQueryPool,
    destroy_pipeline_layout: vk::PFN_vkDestroyPipelineLayout,
    destroy_query_pool: vk::PFN_vkDestroyQueryPool,
    create_graphics_pipelines: vk::PFN_vkCreateGraphicsPipelines,
    create_compute_pipelines: vk::PFN_vkCreateComputePipelines,
    destroy_pipeline: vk::PFN_vkDestroyPipeline,
    destroy_pipeline_cache: vk::PFN_vkDestroyPipelineCache,
    get_pipeline_cache_data: vk::PFN_vkGetPipelineCacheData,
    create_command_pool: vk::PFN_vkCreateCommandPool,
    destroy_command_pool: vk::PFN_vkDestroyCommandPool,
    allocate_command_buffers: vk::PFN_vkAllocateCommandBuffers,
    reset_command_buffer: vk::PFN_vkResetCommandBuffer,
    begin_command_buffer: vk::PFN_vkBeginCommandBuffer,
    end_command_buffer: vk::PFN_vkEndCommandBuffer,
    cmd_pipeline_barrier2: vk::PFN_vkCmdPipelineBarrier2,
    cmd_begin_rendering: vk::PFN_vkCmdBeginRendering,
    cmd_end_rendering: vk::PFN_vkCmdEndRendering,
    cmd_begin_debug_utils_label: vk::PFN_vkCmdBeginDebugUtilsLabelEXT,
    cmd_end_debug_utils_label: vk::PFN_vkCmdEndDebugUtilsLabelEXT,
    cmd_bind_pipeline: vk::PFN_vkCmdBindPipeline,
    cmd_bind_descriptor_sets: vk::PFN_vkCmdBindDescriptorSets,
    cmd_bind_vertex_buffers: vk::PFN_vkCmdBindVertexBuffers,
    cmd_bind_index_buffer: vk::PFN_vkCmdBindIndexBuffer,
    cmd_blit_image2: vk::PFN_vkCmdBlitImage2,
    cmd_copy_buffer2: vk::PFN_vkCmdCopyBuffer2,
    cmd_copy_buffer_to_image2: vk::PFN_vkCmdCopyBufferToImage2,
    cmd_copy_image_to_buffer2: vk::PFN_vkCmdCopyImageToBuffer2,
    cmd_dispatch: vk::PFN_vkCmdDispatch,
    cmd_draw: vk::PFN_vkCmdDraw,
    cmd_set_viewport: vk::PFN_vkCmdSetViewport,
    cmd_set_scissor: vk::PFN_vkCmdSetScissor,
    cmd_reset_query_pool: vk::PFN_vkCmdResetQueryPool,
    cmd_write_timestamp2: vk::PFN_vkCmdWriteTimestamp2,
    cmd_draw_indexed_indirect: vk::PFN_vkCmdDrawIndexedIndirect,
    create_semaphore: vk::PFN_vkCreateSemaphore,
    destroy_semaphore: vk::PFN_vkDestroySemaphore,
    create_fence: vk::PFN_vkCreateFence,
    destroy_fence: vk::PFN_vkDestroyFence,
    wait_for_fences: vk::PFN_vkWaitForFences,
    reset_fences: vk::PFN_vkResetFences,
    get_fence_status: vk::PFN_vkGetFenceStatus,
    get_query_pool_results: vk::PFN_vkGetQueryPoolResults,
    queue_submit2: vk::PFN_vkQueueSubmit2,
}

impl DeviceFns {
    unsafe fn load(
        instance: &InstanceContext,
        device: vk::VkDevice,
        swapchain_maintenance1: bool,
    ) -> Result<Self, ProbeError> {
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
            release_swapchain_images: if swapchain_maintenance1 {
                load!(c"vkReleaseSwapchainImagesKHR")
            } else {
                None
            },
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
            create_pipeline_cache: load!(c"vkCreatePipelineCache"),
            create_query_pool: load!(c"vkCreateQueryPool"),
            destroy_pipeline_layout: load!(c"vkDestroyPipelineLayout"),
            destroy_query_pool: load!(c"vkDestroyQueryPool"),
            create_graphics_pipelines: load!(c"vkCreateGraphicsPipelines"),
            create_compute_pipelines: load!(c"vkCreateComputePipelines"),
            destroy_pipeline: load!(c"vkDestroyPipeline"),
            destroy_pipeline_cache: load!(c"vkDestroyPipelineCache"),
            get_pipeline_cache_data: load!(c"vkGetPipelineCacheData"),
            create_command_pool: load!(c"vkCreateCommandPool"),
            destroy_command_pool: load!(c"vkDestroyCommandPool"),
            allocate_command_buffers: load!(c"vkAllocateCommandBuffers"),
            reset_command_buffer: load!(c"vkResetCommandBuffer"),
            begin_command_buffer: load!(c"vkBeginCommandBuffer"),
            end_command_buffer: load!(c"vkEndCommandBuffer"),
            cmd_pipeline_barrier2: load!(c"vkCmdPipelineBarrier2"),
            cmd_begin_rendering: load!(c"vkCmdBeginRendering"),
            cmd_end_rendering: load!(c"vkCmdEndRendering"),
            cmd_begin_debug_utils_label: load!(c"vkCmdBeginDebugUtilsLabelEXT"),
            cmd_end_debug_utils_label: load!(c"vkCmdEndDebugUtilsLabelEXT"),
            cmd_bind_pipeline: load!(c"vkCmdBindPipeline"),
            cmd_bind_descriptor_sets: load!(c"vkCmdBindDescriptorSets"),
            cmd_bind_vertex_buffers: load!(c"vkCmdBindVertexBuffers"),
            cmd_bind_index_buffer: load!(c"vkCmdBindIndexBuffer"),
            cmd_blit_image2: load!(c"vkCmdBlitImage2"),
            cmd_copy_buffer2: load!(c"vkCmdCopyBuffer2"),
            cmd_copy_buffer_to_image2: load!(c"vkCmdCopyBufferToImage2"),
            cmd_copy_image_to_buffer2: load!(c"vkCmdCopyImageToBuffer2"),
            cmd_dispatch: load!(c"vkCmdDispatch"),
            cmd_draw: load!(c"vkCmdDraw"),
            cmd_set_viewport: load!(c"vkCmdSetViewport"),
            cmd_set_scissor: load!(c"vkCmdSetScissor"),
            cmd_reset_query_pool: load!(c"vkCmdResetQueryPool"),
            cmd_write_timestamp2: load!(c"vkCmdWriteTimestamp2"),
            cmd_draw_indexed_indirect: load!(c"vkCmdDrawIndexedIndirect"),
            create_semaphore: load!(c"vkCreateSemaphore"),
            destroy_semaphore: load!(c"vkDestroySemaphore"),
            create_fence: load!(c"vkCreateFence"),
            destroy_fence: load!(c"vkDestroyFence"),
            wait_for_fences: load!(c"vkWaitForFences"),
            reset_fences: load!(c"vkResetFences"),
            get_fence_status: load!(c"vkGetFenceStatus"),
            get_query_pool_results: load!(c"vkGetQueryPoolResults"),
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
            pipelineCreationCacheControl: if adapter.pipeline_creation_cache_control {
                vk::VK_TRUE
            } else {
                vk::VK_FALSE
            },
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
        let enabled_features = vk::VkPhysicalDeviceFeatures {
            textureCompressionBC: if adapter.texture.path == TexturePath::Bc1 {
                vk::VK_TRUE
            } else {
                vk::VK_FALSE
            },
            ..Default::default()
        };
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
            pEnabledFeatures: &raw const enabled_features,
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
        let functions =
            unsafe { DeviceFns::load(&instance, handle, adapter.swapchain_maintenance1) }?;
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
    let texture_mode = texture_mode_from_environment()?;
    let force_swapchain_fallback =
        env::var_os("MULCIBER_VULKAN_FORCE_SWAPCHAIN_FALLBACK").is_some();
    let force_msaa_1x = env::var_os("MULCIBER_VULKAN_FORCE_MSAA_1X").is_some();
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
    let mut texture_rejections = Vec::new();
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
        if properties.apiVersion < API_VERSION_1_3
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
        let mut bc1_properties = vk::VkFormatProperties::default();
        // SAFETY: The physical device is live and properties storage is writable.
        unsafe {
            instance
                .functions
                .get_physical_device_format_properties
                .expect("loaded function")(
                device,
                vk::VK_FORMAT_BC1_RGBA_UNORM_BLOCK,
                &raw mut bc1_properties,
            );
        }
        let bc1 = Bc1Support {
            core_feature: features.features.textureCompressionBC == vk::VK_TRUE,
            optimal_tiling_features: bc1_properties.optimalTilingFeatures,
        };

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
            let required_queue_flags =
                (vk::VK_QUEUE_GRAPHICS_BIT | vk::VK_QUEUE_COMPUTE_BIT) as u32;
            if family.queueCount == 0
                || family.queueFlags & required_queue_flags != required_queue_flags
            {
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
                let texture = match select_texture(texture_mode, bc1) {
                    Ok(selection) => selection,
                    Err(error) => {
                        texture_rejections.push(format!(
                            "{}: {error}",
                            String::from_utf8_lossy(&fixed_c_string(&properties.deviceName))
                        ));
                        break;
                    }
                };
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
                        pipeline_creation_cache_control: features13.pipelineCreationCacheControl
                            == vk::VK_TRUE,
                        pipeline_cache_identity: PipelineCacheIdentity {
                            vendor_id: properties.vendorID,
                            device_id: properties.deviceID,
                            uuid: properties.pipelineCacheUUID,
                        },
                        sample_count: choose_sample_count(
                            properties.limits.framebufferColorSampleCounts,
                            properties.limits.framebufferDepthSampleCounts,
                            force_msaa_1x,
                        ),
                        timestamp_valid_bits: family.timestampValidBits,
                        timestamp_period: properties.limits.timestampPeriod,
                        texture,
                    },
                    fixed_c_string(&properties.deviceName),
                ));
                break;
            }
        }
    }

    candidates.sort_by_key(|candidate| candidate.0);
    let (_, adapter, name) = candidates.pop().ok_or_else(|| {
        if texture_mode == TextureMode::Bc1 && !texture_rejections.is_empty() {
            ProbeError(format!(
                "no Vulkan 1.3 graphics/present adapter satisfies required BC1 mode: {}",
                texture_rejections.join("; ")
            ))
        } else {
            ProbeError(
                "no Vulkan 1.3 graphics/present adapter satisfies Mulciber's baseline".into(),
            )
        }
    })?;
    println!("Vulkan adapter: {}", String::from_utf8_lossy(&name));
    if force_swapchain_fallback {
        println!("Swapchain maintenance override: forced fallback");
    }
    if force_msaa_1x {
        println!("Multisampling override: forced 1x fallback");
    }
    println!(
        "Swapchain retirement: {}",
        if adapter.swapchain_maintenance1 {
            "VK_KHR_swapchain_maintenance1 presentation fences"
        } else {
            "deferred reacquisition fallback"
        }
    );
    println!("Multisampling: {}", sample_count_name(adapter.sample_count));
    println!(
        "Pipeline compile-required control: {}",
        if adapter.pipeline_creation_cache_control {
            "available"
        } else {
            "unavailable (feedback-only strict proof)"
        }
    );
    println!(
        "BC1 capability: textureCompressionBC={}, optimalTilingFeatures=0x{:08x}",
        if adapter.texture.bc1.core_feature {
            "yes"
        } else {
            "no"
        },
        adapter.texture.bc1.optimal_tiling_features
    );
    match (adapter.texture.path, adapter.texture.mode) {
        (TexturePath::Bc1, _) => println!("Texture path: {}", TexturePath::Bc1.diagnostic_name()),
        (TexturePath::Rgba8, TextureMode::Rgba8) => {
            println!("Texture path: RGBA8 fallback (forced by MULCIBER_VULKAN_TEXTURE_MODE)");
        }
        (TexturePath::Rgba8, TextureMode::Auto) => println!(
            "Texture path: RGBA8 fallback (missing {})",
            missing_bc1_requirements(adapter.texture.bc1)
        ),
        (TexturePath::Rgba8, TextureMode::Bc1) => unreachable!("required BC1 was rejected"),
    }
    if adapter.timestamp_valid_bits == 0 {
        println!("GPU timestamps: unavailable on the selected queue family");
    } else {
        println!(
            "GPU timestamps: {} valid bits at {:.3} ns/tick",
            adapter.timestamp_valid_bits, adapter.timestamp_period
        );
    }
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
    offscreen: GpuImage,
    msaa_color: GpuImage,
    depth: GpuImage,
    pipeline_layout: vk::VkPipelineLayout,
    pipeline: vk::VkPipeline,
    post_pipeline_layout: vk::VkPipelineLayout,
    post_pipeline: vk::VkPipeline,
    render_finished: Vec<vk::VkSemaphore>,
    present_fences: Vec<vk::VkFence>,
    present_pending: Vec<bool>,
}

#[derive(Default)]
struct GpuBuffer {
    handle: vk::VkBuffer,
    memory: vk::VkDeviceMemory,
}

struct UniformBuffer {
    buffer: GpuBuffer,
    mapped: *mut c_void,
}

#[derive(Default)]
struct GpuImage {
    handle: vk::VkImage,
    memory: vk::VkDeviceMemory,
    view: vk::VkImageView,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PipelineCacheMode {
    Learning,
    Strict,
    Disabled,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PipelineCachePersistence {
    Initializing,
    Pending,
    Saved,
}

struct PipelineCacheState {
    handle: vk::VkPipelineCache,
    path: PathBuf,
    mode: PipelineCacheMode,
    persistence: PipelineCachePersistence,
}

impl PipelineCacheState {
    fn is_strict(&self) -> bool {
        self.mode == PipelineCacheMode::Strict
    }

    fn is_disabled(&self) -> bool {
        self.mode == PipelineCacheMode::Disabled
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrameAbandonmentState {
    Disabled,
    Pending,
    AwaitingRecovery,
    Recovered,
}

impl FrameAbandonmentState {
    const fn new(requested: bool) -> Self {
        if requested {
            Self::Pending
        } else {
            Self::Disabled
        }
    }

    const fn should_abandon(self) -> bool {
        matches!(self, Self::Pending)
    }

    fn record_abandonment(&mut self) {
        debug_assert_eq!(*self, Self::Pending);
        *self = Self::AwaitingRecovery;
    }

    fn record_presentation(&mut self) -> bool {
        if *self == Self::AwaitingRecovery {
            *self = Self::Recovered;
            true
        } else {
            false
        }
    }

    fn require_recovery(self) -> Result<(), ProbeError> {
        match self {
            Self::Disabled | Self::Recovered => Ok(()),
            Self::Pending => Err(ProbeError(
                "requested acquired-frame abandonment never acquired an image".into(),
            )),
            Self::AwaitingRecovery => Err(ProbeError(
                "acquired-frame abandonment was not followed by a presented recovery frame".into(),
            )),
        }
    }
}

struct Renderer {
    device: DeviceContext,
    pipeline_cache: PipelineCacheState,
    swapchain: vk::VkSwapchainKHR,
    format: vk::VkFormat,
    depth_format: vk::VkFormat,
    extent: vk::VkExtent2D,
    surface_info: Option<SurfaceInfo>,
    images: Vec<vk::VkImage>,
    views: Vec<vk::VkImageView>,
    offscreen: GpuImage,
    msaa_color: GpuImage,
    depth: GpuImage,
    pipeline_layout: vk::VkPipelineLayout,
    pipeline: vk::VkPipeline,
    command_pool: vk::VkCommandPool,
    command_buffer: vk::VkCommandBuffer,
    query_pool: vk::VkQueryPool,
    gpu_timing: GpuTimingSummary,
    vertex_buffer: GpuBuffer,
    index_buffer: GpuBuffer,
    texture: GpuImage,
    texture_sampler: vk::VkSampler,
    shadow_map: GpuImage,
    shadow_sampler: vk::VkSampler,
    shadow_pipeline_layout: vk::VkPipelineLayout,
    shadow_pipeline: vk::VkPipeline,
    descriptor_set_layout: vk::VkDescriptorSetLayout,
    descriptor_pool: vk::VkDescriptorPool,
    descriptor_sets: Vec<vk::VkDescriptorSet>,
    post_sampler: vk::VkSampler,
    post_descriptor_set_layout: vk::VkDescriptorSetLayout,
    post_descriptor_pool: vk::VkDescriptorPool,
    post_descriptor_set: vk::VkDescriptorSet,
    post_pipeline_layout: vk::VkPipelineLayout,
    post_pipeline: vk::VkPipeline,
    uniform_buffers: Vec<UniformBuffer>,
    frame_slot: usize,
    started: Instant,
    compute_storage: GpuBuffer,
    compute_indirect: GpuBuffer,
    compute_image: GpuImage,
    compute_sampled_view: vk::VkImageView,
    compute_readback: GpuBuffer,
    compute_descriptor_set_layout: vk::VkDescriptorSetLayout,
    compute_descriptor_pool: vk::VkDescriptorPool,
    compute_descriptor_set: vk::VkDescriptorSet,
    compute_pipeline_layout: vk::VkPipelineLayout,
    compute_pipeline: vk::VkPipeline,
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
    acquire_timeout: u64,
    frame_abandonment: FrameAbandonmentState,
    live_resize_trace: LiveResizeTrace,
}

impl Renderer {
    #[allow(clippy::too_many_lines)]
    fn new(
        device: DeviceContext,
        window: &Window,
        options: &RunOptions,
    ) -> Result<Self, ProbeError> {
        let pipeline_cache_options = &options.pipeline_cache;
        let depth_format = choose_depth_format(&device)?;
        require_offscreen_format(&device)?;
        let pipeline_cache_path = pipeline_cache_options
            .path
            .clone()
            .unwrap_or_else(|| pipeline_cache_default_path(device.adapter.pipeline_cache_identity));
        let mut renderer = Self {
            device,
            pipeline_cache: PipelineCacheState {
                handle: ptr::null_mut(),
                path: pipeline_cache_path,
                mode: if pipeline_cache_options.disabled {
                    PipelineCacheMode::Disabled
                } else if pipeline_cache_options.strict {
                    PipelineCacheMode::Strict
                } else {
                    PipelineCacheMode::Learning
                },
                persistence: PipelineCachePersistence::Initializing,
            },
            swapchain: ptr::null_mut(),
            format: vk::VK_FORMAT_UNDEFINED,
            depth_format,
            extent: vk::VkExtent2D::default(),
            surface_info: None,
            images: Vec::new(),
            views: Vec::new(),
            offscreen: GpuImage::default(),
            msaa_color: GpuImage::default(),
            depth: GpuImage::default(),
            pipeline_layout: ptr::null_mut(),
            pipeline: ptr::null_mut(),
            command_pool: ptr::null_mut(),
            command_buffer: ptr::null_mut(),
            query_pool: ptr::null_mut(),
            gpu_timing: GpuTimingSummary::default(),
            vertex_buffer: GpuBuffer::default(),
            index_buffer: GpuBuffer::default(),
            texture: GpuImage::default(),
            texture_sampler: ptr::null_mut(),
            shadow_map: GpuImage::default(),
            shadow_sampler: ptr::null_mut(),
            shadow_pipeline_layout: ptr::null_mut(),
            shadow_pipeline: ptr::null_mut(),
            descriptor_set_layout: ptr::null_mut(),
            descriptor_pool: ptr::null_mut(),
            descriptor_sets: Vec::new(),
            post_sampler: ptr::null_mut(),
            post_descriptor_set_layout: ptr::null_mut(),
            post_descriptor_pool: ptr::null_mut(),
            post_descriptor_set: ptr::null_mut(),
            post_pipeline_layout: ptr::null_mut(),
            post_pipeline: ptr::null_mut(),
            uniform_buffers: Vec::new(),
            frame_slot: 0,
            started: Instant::now(),
            compute_storage: GpuBuffer::default(),
            compute_indirect: GpuBuffer::default(),
            compute_image: GpuImage::default(),
            compute_sampled_view: ptr::null_mut(),
            compute_readback: GpuBuffer::default(),
            compute_descriptor_set_layout: ptr::null_mut(),
            compute_descriptor_pool: ptr::null_mut(),
            compute_descriptor_set: ptr::null_mut(),
            compute_pipeline_layout: ptr::null_mut(),
            compute_pipeline: ptr::null_mut(),
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
            acquire_timeout: platform::acquire_timeout(window),
            frame_abandonment: FrameAbandonmentState::new(options.abandon_acquired_frame_once),
            live_resize_trace: LiveResizeTrace::from_environment(),
        };
        if renderer.live_resize_trace.is_enabled() {
            println!("Live resize timing trace enabled");
        }
        if renderer.frame_abandonment.should_abandon() {
            println!("Acquired-frame abandonment: one-shot probe enabled");
        }
        renderer.create_pipeline_cache(pipeline_cache_options.rebuild)?;
        renderer.create_frame_resources()?;
        renderer.create_gpu_instrumentation()?;
        renderer.create_geometry_buffers()?;
        renderer.create_uniform_buffers()?;
        renderer.create_texture_resources()?;
        renderer.create_compute_readback_resources()?;
        renderer.create_shadow_resources()?;
        renderer.create_texture_descriptors()?;
        renderer.create_postprocess_resources()?;
        let (width, height) = window
            .client_extent()
            .map_err(|error| ProbeError(error.to_string()))?;
        if width != 0 && height != 0 {
            renderer.recreate_swapchain(width, height)?;
            println!(
                "Depth: resize-dependent device-local {} attachment with testing/writes",
                depth_format_name(renderer.depth_format)
            );
            println!(
                "Post-processing: offscreen RGBA8 scene target sampled by a fullscreen vignette pass"
            );
            println!(
                "Shadows: {SHADOW_MAP_SIZE}x{SHADOW_MAP_SIZE} sampled depth map from a depth-only pass"
            );
        }
        if renderer.pipeline_cache.mode == PipelineCacheMode::Learning {
            renderer.pipeline_cache.persistence = PipelineCachePersistence::Pending;
        }
        Ok(renderer)
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let _ = self.finish();
        self.destroy_swapchain_resources();
        let mut vertex_buffer = mem::take(&mut self.vertex_buffer);
        let mut index_buffer = mem::take(&mut self.index_buffer);
        let mut texture = mem::take(&mut self.texture);
        let mut uniform_buffers = mem::take(&mut self.uniform_buffers);
        let mut compute_storage = mem::take(&mut self.compute_storage);
        let mut compute_indirect = mem::take(&mut self.compute_indirect);
        let mut compute_image = mem::take(&mut self.compute_image);
        let mut compute_readback = mem::take(&mut self.compute_readback);
        // SAFETY: `finish` completed all submitted GPU work before these owned buffers are freed.
        unsafe {
            self.destroy_compute_resources(
                &mut compute_storage,
                &mut compute_indirect,
                &mut compute_readback,
            );
            self.destroy_persistent_render_resources();
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
            self.destroy_compute_sampled_view();
            self.destroy_image(&mut texture);
            self.destroy_image(&mut compute_image);
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
            for uniform in &mut uniform_buffers {
                if !uniform.mapped.is_null() {
                    self.device.functions.unmap_memory.expect("loaded function")(
                        self.device.handle,
                        uniform.buffer.memory,
                    );
                    uniform.mapped = ptr::null_mut();
                }
                self.destroy_buffer(&mut uniform.buffer);
            }
            self.destroy_buffer(&mut vertex_buffer);
            self.destroy_buffer(&mut index_buffer);
        }
        // SAFETY: Frame resources are owned by this renderer and destroyed once after GPU idle.
        unsafe {
            self.destroy_gpu_instrumentation();
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
            self.destroy_owned_pipeline_cache();
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

fn choose_sample_count(
    color_sample_counts: vk::VkSampleCountFlags,
    depth_sample_counts: vk::VkSampleCountFlags,
    force_1x: bool,
) -> vk::VkSampleCountFlagBits {
    let four_samples = u32::try_from(vk::VK_SAMPLE_COUNT_4_BIT).expect("positive sample-count bit");
    if !force_1x && color_sample_counts & depth_sample_counts & four_samples != 0 {
        vk::VK_SAMPLE_COUNT_4_BIT
    } else {
        vk::VK_SAMPLE_COUNT_1_BIT
    }
}

fn timestamp_tick_delta(start: u64, end: u64, valid_bits: u32) -> u64 {
    debug_assert!(valid_bits != 0);
    let mask = if valid_bits >= u64::BITS {
        u64::MAX
    } else {
        (1_u64 << valid_bits) - 1
    };
    end.wrapping_sub(start) & mask
}

fn sample_count_name(sample_count: vk::VkSampleCountFlagBits) -> &'static str {
    if sample_count == vk::VK_SAMPLE_COUNT_4_BIT {
        "4x MSAA color/depth with swapchain resolve"
    } else {
        "1x fallback"
    }
}

fn require_offscreen_format(device: &DeviceContext) -> Result<(), ProbeError> {
    let mut properties = vk::VkFormatProperties::default();
    // SAFETY: The selected adapter is live and properties storage is writable.
    unsafe {
        device
            .instance
            .functions
            .get_physical_device_format_properties
            .expect("loaded function")(
            device.adapter.handle,
            OFFSCREEN_FORMAT,
            &raw mut properties,
        );
    }
    let required = (vk::VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT
        | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT
        | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR_BIT) as u32;
    if properties.optimalTilingFeatures & required == required {
        Ok(())
    } else {
        Err(ProbeError(
            "R8G8B8A8_UNORM lacks required offscreen color, sampled, or linear-filter support"
                .into(),
        ))
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
        let required = (vk::VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT
            | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT) as u32;
        if properties.optimalTilingFeatures & required == required {
            return Ok(format);
        }
    }
    Err(ProbeError(
        "adapter exposes no supported optimal-tiled sampled depth-attachment format".into(),
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

fn descriptor_binding(
    binding: u32,
    descriptor_type: vk::VkDescriptorType,
    stage_flags: vk::VkShaderStageFlags,
) -> vk::VkDescriptorSetLayoutBinding {
    vk::VkDescriptorSetLayoutBinding {
        binding,
        descriptorType: descriptor_type,
        descriptorCount: 1,
        stageFlags: stage_flags,
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
    color_mip_range(0, 1)
}

fn color_mip_range(base_mip_level: u32, level_count: u32) -> vk::VkImageSubresourceRange {
    vk::VkImageSubresourceRange {
        aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
        baseMipLevel: base_mip_level,
        levelCount: level_count,
        baseArrayLayer: 0,
        layerCount: 1,
    }
}

fn color_subresource_layers(mip_level: u32) -> vk::VkImageSubresourceLayers {
    vk::VkImageSubresourceLayers {
        aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
        mipLevel: mip_level,
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

fn storage_buffer_barrier(
    buffer: vk::VkBuffer,
    size: vk::VkDeviceSize,
    source_stage: vk::VkPipelineStageFlags2,
    source_access: vk::VkAccessFlags2,
    destination_stage: vk::VkPipelineStageFlags2,
    destination_access: vk::VkAccessFlags2,
) -> vk::VkBufferMemoryBarrier2 {
    vk::VkBufferMemoryBarrier2 {
        sType: vk::VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER_2,
        srcStageMask: source_stage,
        srcAccessMask: source_access,
        dstStageMask: destination_stage,
        dstAccessMask: destination_access,
        srcQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
        dstQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
        buffer,
        offset: 0,
        size,
        ..Default::default()
    }
}

fn expected_storage_value(index: usize) -> u32 {
    u32::try_from(index)
        .expect("storage value index fits u32")
        .wrapping_mul(1_664_525)
        .wrapping_add(1_013_904_223)
}

fn storage_buffer_byte_len() -> usize {
    STORAGE_VALUE_COUNT * mem::size_of::<u32>()
}

fn compute_image_byte_len() -> usize {
    usize::try_from(COMPUTE_IMAGE_WIDTH * COMPUTE_IMAGE_HEIGHT)
        .expect("compute image texel count fits usize")
        * RGBA8_TEXEL_SIZE
}

fn compute_image_readback_offset() -> usize {
    storage_buffer_byte_len() + mem::size_of::<vk::VkDrawIndexedIndirectCommand>()
}

fn compute_mip_tail_readback_offset() -> usize {
    compute_image_readback_offset() + compute_image_byte_len()
}

fn compute_readback_byte_len() -> usize {
    compute_mip_tail_readback_offset() + RGBA8_TEXEL_SIZE
}

fn compute_image_mip_extent(mip_level: u32) -> (u32, u32) {
    (
        (COMPUTE_IMAGE_WIDTH >> mip_level).max(1),
        (COMPUTE_IMAGE_HEIGHT >> mip_level).max(1),
    )
}

fn expected_indirect_command() -> vk::VkDrawIndexedIndirectCommand {
    vk::VkDrawIndexedIndirectCommand {
        indexCount: u32::try_from(TRIANGLE_INDICES.len()).expect("triangle index count fits u32"),
        instanceCount: 1,
        firstIndex: 0,
        vertexOffset: 0,
        firstInstance: 0,
    }
}

fn expected_compute_texel(index: usize) -> [u8; RGBA8_TEXEL_SIZE] {
    let width = usize::try_from(COMPUTE_IMAGE_WIDTH).expect("compute image width fits usize");
    let x = index % width;
    let y = index / width;
    if (x / 2 + y / 2).is_multiple_of(2) {
        [255, 0, 255, 255]
    } else {
        [0, 255, 255, 255]
    }
}

fn expected_compute_mip_tail() -> [u8; RGBA8_TEXEL_SIZE] {
    [255, 0, 255, 255]
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
    fn sample_count_requires_shared_color_and_depth_support() {
        let one = u32::try_from(vk::VK_SAMPLE_COUNT_1_BIT).expect("positive sample-count bit");
        let four = u32::try_from(vk::VK_SAMPLE_COUNT_4_BIT).expect("positive sample-count bit");
        assert_eq!(
            choose_sample_count(one | four, one | four, false),
            vk::VK_SAMPLE_COUNT_4_BIT
        );
        assert_eq!(
            choose_sample_count(one | four, one, false),
            vk::VK_SAMPLE_COUNT_1_BIT
        );
        assert_eq!(
            choose_sample_count(one | four, one | four, true),
            vk::VK_SAMPLE_COUNT_1_BIT
        );
    }

    #[test]
    fn timestamp_delta_handles_queue_bit_width_wraparound() {
        assert_eq!(timestamp_tick_delta(1_000, 1_025, 64), 25);
        assert_eq!(timestamp_tick_delta(250, 7, 8), 13);
    }

    #[test]
    fn frame_abandonment_requires_a_later_presentation() {
        let mut state = FrameAbandonmentState::new(true);
        assert!(state.should_abandon());
        state.record_abandonment();
        assert!(state.require_recovery().is_err());
        assert!(state.record_presentation());
        assert_eq!(state, FrameAbandonmentState::Recovered);
        assert!(state.require_recovery().is_ok());
        assert!(!state.record_presentation());
    }

    #[test]
    fn swapchain_replacement_advances_the_graphics_generation() {
        let extent = SurfaceExtent::new(960, 540);
        let first = next_surface_info(None, extent).unwrap();
        let second = next_surface_info(Some(first), extent).unwrap();
        let resized = next_surface_info(Some(second), SurfaceExtent::new(1280, 720)).unwrap();

        assert_eq!(first.generation().get(), 1);
        assert_eq!(second.generation().get(), 2);
        assert_eq!(resized.generation().get(), 3);
        assert_eq!(resized.extent(), SurfaceExtent::new(1280, 720));
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
    fn frame_uniform_matches_std140_block_layout() {
        assert_eq!(mem::size_of::<FrameUniform>(), 80);
        assert_eq!(mem::align_of::<FrameUniform>(), 16);
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

    #[test]
    fn compute_outputs_and_barriers_are_deterministic() {
        assert_eq!(expected_storage_value(0), 1_013_904_223);
        assert_eq!(expected_storage_value(1), 1_015_568_748);
        assert_eq!(expected_storage_value(63), 1_118_769_298);

        let barrier = storage_buffer_barrier(
            ptr::null_mut(),
            256,
            vk::VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
            vk::VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_TRANSFER_READ_BIT,
        );
        assert_eq!(
            barrier.srcStageMask,
            vk::VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT
        );
        assert_eq!(
            barrier.srcAccessMask,
            vk::VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT
        );
        assert_eq!(barrier.dstStageMask, vk::VK_PIPELINE_STAGE_2_COPY_BIT);
        assert_eq!(barrier.dstAccessMask, vk::VK_ACCESS_2_TRANSFER_READ_BIT);
        assert_eq!(barrier.size, 256);

        assert_eq!(mem::size_of::<vk::VkDrawIndexedIndirectCommand>(), 20);
        let command = expected_indirect_command();
        assert_eq!(command.indexCount, 3);
        assert_eq!(command.instanceCount, 1);
        assert_eq!(command.firstIndex, 0);
        assert_eq!(command.vertexOffset, 0);
        assert_eq!(command.firstInstance, 0);

        let indirect_barrier = storage_buffer_barrier(
            ptr::null_mut(),
            u64::try_from(mem::size_of::<vk::VkDrawIndexedIndirectCommand>())
                .expect("indirect command size fits u64"),
            vk::VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
            vk::VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT | vk::VK_PIPELINE_STAGE_2_DRAW_INDIRECT_BIT,
            vk::VK_ACCESS_2_TRANSFER_READ_BIT | vk::VK_ACCESS_2_INDIRECT_COMMAND_READ_BIT,
        );
        assert_eq!(
            indirect_barrier.dstStageMask,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT | vk::VK_PIPELINE_STAGE_2_DRAW_INDIRECT_BIT
        );
        assert_eq!(
            indirect_barrier.dstAccessMask,
            vk::VK_ACCESS_2_TRANSFER_READ_BIT | vk::VK_ACCESS_2_INDIRECT_COMMAND_READ_BIT
        );
        assert_eq!(indirect_barrier.size, 20);

        assert_eq!(compute_image_byte_len(), 256);
        assert_eq!(compute_image_readback_offset(), 276);
        assert_eq!(compute_mip_tail_readback_offset(), 532);
        assert_eq!(compute_readback_byte_len(), 536);
        assert_eq!(compute_image_mip_extent(0), (8, 8));
        assert_eq!(compute_image_mip_extent(1), (4, 4));
        assert_eq!(compute_image_mip_extent(2), (2, 2));
        assert_eq!(compute_image_mip_extent(3), (1, 1));
        assert_eq!(expected_compute_texel(0), [255, 0, 255, 255]);
        assert_eq!(expected_compute_texel(1), [255, 0, 255, 255]);
        assert_eq!(expected_compute_texel(2), [0, 255, 255, 255]);
        assert_eq!(expected_compute_texel(8), [255, 0, 255, 255]);
        assert_eq!(expected_compute_texel(16), [0, 255, 255, 255]);
        assert_eq!(expected_compute_texel(63), [255, 0, 255, 255]);
        assert_eq!(expected_compute_mip_tail(), [255, 0, 255, 255]);
    }
}
