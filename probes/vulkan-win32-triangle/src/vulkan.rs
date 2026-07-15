use std::env;
use std::ffi::{CStr, c_char, c_void};
use std::fmt;
use std::mem;
use std::num::NonZeroU64;
use std::ptr;
use std::thread;
use std::time::Duration;

use crate::vk;
use crate::win32::Window;

const API_VERSION_1_4: u32 = make_api_version(0, 1, 4, 0);
const UINT32_MAX: u32 = u32::MAX;
const UINT64_MAX: u64 = u64::MAX;

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

pub fn run() -> Result<(), ProbeError> {
    let frame_limit = parse_frame_limit()?;
    let window = Window::new("Zinc — native Vulkan 1.4", 960, 540)
        .map_err(|error| ProbeError(error.to_string()))?;
    let entry = Entry::load()?;
    let instance = InstanceContext::new(entry, &window)?;
    let device = DeviceContext::new(instance)?;
    let mut renderer = Renderer::new(device, &window)?;

    let mut rendered_frames = 0;
    while window.pump_events() {
        let (width, height) = window
            .client_extent()
            .map_err(|error| ProbeError(error.to_string()))?;
        if width == 0 || height == 0 {
            thread::sleep(Duration::from_millis(16));
            continue;
        }
        if renderer.render(width, height)? {
            rendered_frames += 1;
            if frame_limit.is_some_and(|limit| rendered_frames >= limit.get()) {
                break;
            }
        }
    }
    renderer.finish()
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
                "Vulkan loader exposes {}.{}.{}, but Zinc requires 1.4",
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
}

impl InstanceContext {
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
            pApplicationName: c"Zinc Vulkan probe".as_ptr(),
            applicationVersion: 0,
            pEngineName: c"Zinc".as_ptr(),
            engineVersion: 0,
            apiVersion: API_VERSION_1_4,
            ..Default::default()
        };
        let layers = [c"VK_LAYER_KHRONOS_validation".as_ptr()];
        let extensions = [
            c"VK_KHR_surface".as_ptr(),
            c"VK_KHR_win32_surface".as_ptr(),
            c"VK_EXT_debug_utils".as_ptr(),
        ];
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
}

struct DeviceFns {
    destroy_device: vk::PFN_vkDestroyDevice,
    get_device_queue: vk::PFN_vkGetDeviceQueue,
    device_wait_idle: vk::PFN_vkDeviceWaitIdle,
    create_swapchain: vk::PFN_vkCreateSwapchainKHR,
    destroy_swapchain: vk::PFN_vkDestroySwapchainKHR,
    get_swapchain_images: vk::PFN_vkGetSwapchainImagesKHR,
    acquire_next_image: vk::PFN_vkAcquireNextImageKHR,
    queue_present: vk::PFN_vkQueuePresentKHR,
    create_image_view: vk::PFN_vkCreateImageView,
    destroy_image_view: vk::PFN_vkDestroyImageView,
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
    cmd_set_viewport: vk::PFN_vkCmdSetViewport,
    cmd_set_scissor: vk::PFN_vkCmdSetScissor,
    cmd_draw: vk::PFN_vkCmdDraw,
    create_semaphore: vk::PFN_vkCreateSemaphore,
    destroy_semaphore: vk::PFN_vkDestroySemaphore,
    create_fence: vk::PFN_vkCreateFence,
    destroy_fence: vk::PFN_vkDestroyFence,
    wait_for_fences: vk::PFN_vkWaitForFences,
    reset_fences: vk::PFN_vkResetFences,
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
            create_swapchain: load!(c"vkCreateSwapchainKHR"),
            destroy_swapchain: load!(c"vkDestroySwapchainKHR"),
            get_swapchain_images: load!(c"vkGetSwapchainImagesKHR"),
            acquire_next_image: load!(c"vkAcquireNextImageKHR"),
            queue_present: load!(c"vkQueuePresentKHR"),
            create_image_view: load!(c"vkCreateImageView"),
            destroy_image_view: load!(c"vkDestroyImageView"),
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
            cmd_set_viewport: load!(c"vkCmdSetViewport"),
            cmd_set_scissor: load!(c"vkCmdSetScissor"),
            cmd_draw: load!(c"vkCmdDraw"),
            create_semaphore: load!(c"vkCreateSemaphore"),
            destroy_semaphore: load!(c"vkDestroySemaphore"),
            create_fence: load!(c"vkCreateFence"),
            destroy_fence: load!(c"vkDestroyFence"),
            wait_for_fences: load!(c"vkWaitForFences"),
            reset_fences: load!(c"vkResetFences"),
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
        let extensions = [vk::VK_KHR_SWAPCHAIN_EXTENSION_NAME.as_ptr().cast()];
        let device_info = vk::VkDeviceCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
            pNext: (&raw mut features13).cast(),
            queueCreateInfoCount: 1,
            pQueueCreateInfos: &raw const queue_info,
            enabledExtensionCount: 1,
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
        if properties.apiVersion < API_VERSION_1_4 || !supports_swapchain(instance, device)? {
            continue;
        }

        let mut features13 = vk::VkPhysicalDeviceVulkan13Features {
            sType: vk::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
            ..Default::default()
        };
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
                    },
                    fixed_c_string(&properties.deviceName),
                ));
                break;
            }
        }
    }

    candidates.sort_by_key(|candidate| candidate.0);
    let (_, adapter, name) = candidates.pop().ok_or_else(|| {
        ProbeError("no Vulkan 1.4 graphics/present adapter satisfies Zinc's baseline".into())
    })?;
    println!("Vulkan adapter: {}", String::from_utf8_lossy(&name));
    Ok(adapter)
}

fn supports_swapchain(
    instance: &InstanceContext,
    device: vk::VkPhysicalDevice,
) -> Result<bool, ProbeError> {
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
        .any(|property| fixed_c_string(&property.extensionName) == c"VK_KHR_swapchain".to_bytes()))
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
    }
    vk::VK_FALSE
}

struct Renderer {
    device: DeviceContext,
    swapchain: vk::VkSwapchainKHR,
    format: vk::VkFormat,
    extent: vk::VkExtent2D,
    images: Vec<vk::VkImage>,
    views: Vec<vk::VkImageView>,
    pipeline_layout: vk::VkPipelineLayout,
    pipeline: vk::VkPipeline,
    command_pool: vk::VkCommandPool,
    command_buffer: vk::VkCommandBuffer,
    image_available: vk::VkSemaphore,
    render_finished: vk::VkSemaphore,
    frame_fence: vk::VkFence,
    recreate_after_present: bool,
}

impl Renderer {
    fn new(device: DeviceContext, window: &Window) -> Result<Self, ProbeError> {
        let mut renderer = Self {
            device,
            swapchain: ptr::null_mut(),
            format: vk::VK_FORMAT_UNDEFINED,
            extent: vk::VkExtent2D::default(),
            images: Vec::new(),
            views: Vec::new(),
            pipeline_layout: ptr::null_mut(),
            pipeline: ptr::null_mut(),
            command_pool: ptr::null_mut(),
            command_buffer: ptr::null_mut(),
            image_available: ptr::null_mut(),
            render_finished: ptr::null_mut(),
            frame_fence: ptr::null_mut(),
            recreate_after_present: false,
        };
        renderer.create_frame_resources()?;
        let (width, height) = window
            .client_extent()
            .map_err(|error| ProbeError(error.to_string()))?;
        if width != 0 && height != 0 {
            renderer.recreate_swapchain(width, height)?;
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
        for output in [&raw mut self.image_available, &raw mut self.render_finished] {
            // SAFETY: Device and create info are valid; output is a distinct writable field.
            check(
                unsafe {
                    self.device
                        .functions
                        .create_semaphore
                        .expect("loaded function")(
                        self.device.handle,
                        &raw const semaphore_info,
                        ptr::null(),
                        output,
                    )
                },
                "vkCreateSemaphore",
            )?;
        }
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
        )
    }

    fn recreate_swapchain(&mut self, width: u32, height: u32) -> Result<(), ProbeError> {
        // SAFETY: Waiting makes all swapchain-dependent resources idle before destruction.
        check(
            unsafe {
                self.device
                    .functions
                    .device_wait_idle
                    .expect("loaded function")(self.device.handle)
            },
            "vkDeviceWaitIdle",
        )?;
        self.destroy_swapchain_resources();

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
            ..Default::default()
        };
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
                    &raw mut self.swapchain,
                )
            },
            "vkCreateSwapchainKHR",
        )?;
        self.format = format.format;
        self.extent = extent;
        self.images = self.swapchain_images()?;
        self.views = self.create_image_views()?;
        self.create_pipeline()?;
        self.recreate_after_present = false;
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

    fn create_pipeline(&mut self) -> Result<(), ProbeError> {
        let layout_info = vk::VkPipelineLayoutCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
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

    fn create_graphics_pipeline(
        &mut self,
        vertex: vk::VkShaderModule,
        fragment: vk::VkShaderModule,
    ) -> Result<(), ProbeError> {
        let stages = [
            shader_stage(vk::VK_SHADER_STAGE_VERTEX_BIT, vertex),
            shader_stage(vk::VK_SHADER_STAGE_FRAGMENT_BIT, fragment),
        ];
        let vertex_input = vk::VkPipelineVertexInputStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
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

    fn render(&mut self, width: u32, height: u32) -> Result<bool, ProbeError> {
        if self.swapchain.is_null()
            || self.extent.width != width
            || self.extent.height != height
            || self.recreate_after_present
        {
            self.recreate_swapchain(width, height)?;
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
            "vkWaitForFences",
        )?;
        let mut image_index = 0;
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
                ptr::null_mut(),
                &raw mut image_index,
            )
        };
        if acquire == vk::VK_ERROR_OUT_OF_DATE_KHR {
            self.recreate_swapchain(width, height)?;
            return Ok(false);
        }
        if acquire == vk::VK_SUBOPTIMAL_KHR {
            self.recreate_after_present = true;
        } else {
            check(acquire, "vkAcquireNextImageKHR")?;
        }

        let view = *self
            .views
            .get(image_index as usize)
            .ok_or_else(|| ProbeError("driver returned an invalid swapchain image index".into()))?;
        let image = self.images[image_index as usize];
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
        self.submit()?;

        let present = vk::VkPresentInfoKHR {
            sType: vk::VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,
            waitSemaphoreCount: 1,
            pWaitSemaphores: &raw const self.render_finished,
            swapchainCount: 1,
            pSwapchains: &raw const self.swapchain,
            pImageIndices: &raw const image_index,
            ..Default::default()
        };
        // SAFETY: Queue, swapchain, index, and wait semaphore are valid for this submission.
        let result = unsafe {
            self.device
                .functions
                .queue_present
                .expect("loaded function")(self.device.queue, &raw const present)
        };
        if result == vk::VK_ERROR_OUT_OF_DATE_KHR || result == vk::VK_SUBOPTIMAL_KHR {
            self.recreate_after_present = true;
        } else {
            check(result, "vkQueuePresentKHR")?;
        }
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
            self.device.functions.cmd_draw.expect("loaded function")(
                self.command_buffer,
                3,
                1,
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
            subresourceRange: color_subresource_range(),
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

    fn submit(&self) -> Result<(), ProbeError> {
        let wait = vk::VkSemaphoreSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_SEMAPHORE_SUBMIT_INFO,
            semaphore: self.image_available,
            stageMask: vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            ..Default::default()
        };
        let command = vk::VkCommandBufferSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO,
            commandBuffer: self.command_buffer,
            // Zero selects every valid physical device, including the normal single-device case.
            deviceMask: 0,
            ..Default::default()
        };
        let signal = vk::VkSemaphoreSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_SEMAPHORE_SUBMIT_INFO,
            semaphore: self.render_finished,
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
        )
    }

    fn finish(&self) -> Result<(), ProbeError> {
        // SAFETY: Waiting on the owned device is valid during orderly shutdown.
        check(
            unsafe {
                self.device
                    .functions
                    .device_wait_idle
                    .expect("loaded function")(self.device.handle)
            },
            "vkDeviceWaitIdle",
        )
    }

    fn destroy_swapchain_resources(&mut self) {
        // SAFETY: Device is idle before callers invoke this; handles are owned and reset after use.
        unsafe {
            if !self.pipeline.is_null() {
                self.device
                    .functions
                    .destroy_pipeline
                    .expect("loaded function")(
                    self.device.handle, self.pipeline, ptr::null()
                );
                self.pipeline = ptr::null_mut();
            }
            if !self.pipeline_layout.is_null() {
                self.device
                    .functions
                    .destroy_pipeline_layout
                    .expect("loaded function")(
                    self.device.handle,
                    self.pipeline_layout,
                    ptr::null(),
                );
                self.pipeline_layout = ptr::null_mut();
            }
            for view in self.views.drain(..) {
                self.device
                    .functions
                    .destroy_image_view
                    .expect("loaded function")(
                    self.device.handle, view, ptr::null()
                );
            }
            self.images.clear();
            if !self.swapchain.is_null() {
                self.device
                    .functions
                    .destroy_swapchain
                    .expect("loaded function")(
                    self.device.handle, self.swapchain, ptr::null()
                );
                self.swapchain = ptr::null_mut();
            }
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: Best-effort wait ensures resource destruction does not race the GPU.
        unsafe {
            let _ = self
                .device
                .functions
                .device_wait_idle
                .expect("loaded function")(self.device.handle);
        }
        self.destroy_swapchain_resources();
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
            for semaphore in [self.render_finished, self.image_available] {
                if !semaphore.is_null() {
                    self.device
                        .functions
                        .destroy_semaphore
                        .expect("loaded function")(
                        self.device.handle, semaphore, ptr::null()
                    );
                }
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

fn color_subresource_range() -> vk::VkImageSubresourceRange {
    vk::VkImageSubresourceRange {
        aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
        baseMipLevel: 0,
        levelCount: 1,
        baseArrayLayer: 0,
        layerCount: 1,
    }
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
}
