//! Shared Vulkan device and presentation capability collection.

#![allow(clippy::missing_errors_doc)]

#[cfg(any(target_os = "windows", target_os = "linux"))]
mod platform {
    use std::env;
    use std::ffi::{CStr, c_char, c_void};
    use std::fmt::{self, Write as _};
    use std::mem;
    use std::ptr;

    use crate::native::{self, Window};
    use crate::vk;

    const API_VERSION_1_2: u32 = make_api_version(0, 1, 2, 0);
    const API_VERSION_1_3: u32 = make_api_version(0, 1, 3, 0);
    const API_VERSION_1_4: u32 = make_api_version(0, 1, 4, 0);

    #[derive(Debug)]
    pub struct ProbeError(String);

    impl fmt::Display for ProbeError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    impl std::error::Error for ProbeError {}

    struct Entry {
        _library: native::VulkanLibrary,
        get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
        enumerate_instance_version: vk::PFN_vkEnumerateInstanceVersion,
        enumerate_instance_extension_properties: vk::PFN_vkEnumerateInstanceExtensionProperties,
        create_instance: vk::PFN_vkCreateInstance,
    }

    impl Entry {
        fn load() -> Result<Self, ProbeError> {
            let library =
                native::VulkanLibrary::open().map_err(|error| ProbeError(error.into()))?;
            // SAFETY: The Vulkan loader exports this function with the generated ABI.
            let get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr = unsafe {
                cast_address(
                    library.symbol(c"vkGetInstanceProcAddr"),
                    "vkGetInstanceProcAddr",
                )?
            };
            let get = get_instance_proc_addr.expect("required function was checked");
            // SAFETY: Loader-global functions are requested with a null instance.
            let enumerate_instance_version = unsafe {
                load_proc(
                    get(ptr::null_mut(), c"vkEnumerateInstanceVersion".as_ptr()),
                    "vkEnumerateInstanceVersion",
                )?
            };
            // SAFETY: Loader-global functions are requested with a null instance.
            let enumerate_instance_extension_properties = unsafe {
                load_proc(
                    get(
                        ptr::null_mut(),
                        c"vkEnumerateInstanceExtensionProperties".as_ptr(),
                    ),
                    "vkEnumerateInstanceExtensionProperties",
                )?
            };
            // SAFETY: Loader-global functions are requested with a null instance.
            let create_instance = unsafe {
                load_proc(
                    get(ptr::null_mut(), c"vkCreateInstance".as_ptr()),
                    "vkCreateInstance",
                )?
            };
            Ok(Self {
                _library: library,
                get_instance_proc_addr,
                enumerate_instance_version,
                enumerate_instance_extension_properties,
                create_instance,
            })
        }

        fn loader_version(&self) -> Result<u32, ProbeError> {
            let mut version = 0;
            check(
                // SAFETY: The output pointer is writable.
                unsafe {
                    self.enumerate_instance_version.expect("loaded function")(&raw mut version)
                },
                "vkEnumerateInstanceVersion",
            )?;
            Ok(version)
        }

        unsafe fn instance_proc<T: Copy>(
            &self,
            instance: vk::VkInstance,
            name: &CStr,
        ) -> Result<T, ProbeError> {
            let get = self.get_instance_proc_addr.expect("loaded function");
            // SAFETY: The caller pairs the requested Vulkan name with the generated function type.
            unsafe {
                load_proc(
                    get(instance, name.as_ptr()),
                    name.to_string_lossy().as_ref(),
                )
            }
        }
    }

    struct InstanceFns {
        destroy_instance: vk::PFN_vkDestroyInstance,
        create_surface: native::CreateSurface,
        destroy_surface: vk::PFN_vkDestroySurfaceKHR,
        enumerate_physical_devices: vk::PFN_vkEnumeratePhysicalDevices,
        get_properties: vk::PFN_vkGetPhysicalDeviceProperties,
        get_features2: vk::PFN_vkGetPhysicalDeviceFeatures2,
        get_memory_properties: vk::PFN_vkGetPhysicalDeviceMemoryProperties,
        get_queue_families: vk::PFN_vkGetPhysicalDeviceQueueFamilyProperties,
        enumerate_device_extensions: vk::PFN_vkEnumerateDeviceExtensionProperties,
        get_surface_support: vk::PFN_vkGetPhysicalDeviceSurfaceSupportKHR,
        get_surface_capabilities: vk::PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR,
        get_surface_formats: vk::PFN_vkGetPhysicalDeviceSurfaceFormatsKHR,
        get_present_modes: vk::PFN_vkGetPhysicalDeviceSurfacePresentModesKHR,
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
                create_surface: unsafe {
                    entry.instance_proc(instance, native::CREATE_SURFACE_NAME)
                }?,
                destroy_surface: load!(c"vkDestroySurfaceKHR"),
                enumerate_physical_devices: load!(c"vkEnumeratePhysicalDevices"),
                get_properties: load!(c"vkGetPhysicalDeviceProperties"),
                get_features2: load!(c"vkGetPhysicalDeviceFeatures2"),
                get_memory_properties: load!(c"vkGetPhysicalDeviceMemoryProperties"),
                get_queue_families: load!(c"vkGetPhysicalDeviceQueueFamilyProperties"),
                enumerate_device_extensions: load!(c"vkEnumerateDeviceExtensionProperties"),
                get_surface_support: load!(c"vkGetPhysicalDeviceSurfaceSupportKHR"),
                get_surface_capabilities: load!(c"vkGetPhysicalDeviceSurfaceCapabilitiesKHR"),
                get_surface_formats: load!(c"vkGetPhysicalDeviceSurfaceFormatsKHR"),
                get_present_modes: load!(c"vkGetPhysicalDeviceSurfacePresentModesKHR"),
            })
        }
    }

    struct Context {
        _entry: Entry,
        functions: InstanceFns,
        instance: vk::VkInstance,
        surface: vk::VkSurfaceKHR,
        loader_version: u32,
    }

    impl Context {
        fn new(window: &Window) -> Result<Self, ProbeError> {
            let entry = Entry::load()?;
            let loader_version = entry.loader_version()?;
            if loader_version < API_VERSION_1_4 {
                return Err(ProbeError(format!(
                    "Vulkan loader exposes {}, but Mulciber capability reporting requires 1.4",
                    version_string(loader_version)
                )));
            }
            let extensions = instance_extensions(&entry)?;
            for required in [c"VK_KHR_surface", native::SURFACE_EXTENSION] {
                if !extensions.iter().any(|name| name == required.to_bytes()) {
                    return Err(ProbeError(format!(
                        "required instance extension {} is unavailable",
                        required.to_string_lossy()
                    )));
                }
            }
            let application = vk::VkApplicationInfo {
                sType: vk::VK_STRUCTURE_TYPE_APPLICATION_INFO,
                pApplicationName: c"Mulciber Vulkan capability report".as_ptr(),
                apiVersion: API_VERSION_1_4,
                ..Default::default()
            };
            let enabled_extensions = [
                c"VK_KHR_surface".as_ptr(),
                native::SURFACE_EXTENSION.as_ptr(),
            ];
            let create_info = vk::VkInstanceCreateInfo {
                sType: vk::VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                pApplicationInfo: &raw const application,
                enabledExtensionCount: 2,
                ppEnabledExtensionNames: enabled_extensions.as_ptr(),
                ..Default::default()
            };
            let mut instance = ptr::null_mut();
            check(
                // SAFETY: All create-info pointers remain live for this call.
                unsafe {
                    entry.create_instance.expect("loaded function")(
                        &raw const create_info,
                        ptr::null(),
                        &raw mut instance,
                    )
                },
                "vkCreateInstance",
            )?;
            // SAFETY: The instance is live and names are paired with generated function types.
            let functions = unsafe { InstanceFns::load(&entry, instance) }?;
            let mut surface = ptr::null_mut();
            check(
                // SAFETY: The native window and instance remain live; output is writable.
                unsafe {
                    native::create_surface(
                        functions.create_surface,
                        instance,
                        window,
                        &raw mut surface,
                    )
                },
                native::CREATE_SURFACE_NAME
                    .to_str()
                    .expect("static Vulkan function name is UTF-8"),
            )?;
            Ok(Self {
                _entry: entry,
                functions,
                instance,
                surface,
                loader_version,
            })
        }
    }

    impl Drop for Context {
        fn drop(&mut self) {
            // SAFETY: Handles are owned and destroyed in child-before-parent order.
            unsafe {
                self.functions.destroy_surface.expect("loaded function")(
                    self.instance,
                    self.surface,
                    ptr::null(),
                );
                self.functions.destroy_instance.expect("loaded function")(
                    self.instance,
                    ptr::null(),
                );
            }
        }
    }

    struct Report {
        loader_version: u32,
        selected_adapter: Option<usize>,
        adapters: Vec<AdapterReport>,
    }

    struct AdapterReport {
        name: String,
        api_version: u32,
        driver_version: u32,
        vendor_id: u32,
        device_id: u32,
        device_type: &'static str,
        pipeline_cache_uuid: [u8; 16],
        features: Features,
        limits: Limits,
        memory_heaps: Vec<MemoryHeap>,
        queues: Vec<QueueFamily>,
        extensions: Vec<Extension>,
        surface: Surface,
        baseline_failures: Vec<String>,
    }

    #[derive(Default)]
    #[allow(clippy::struct_excessive_bools)]
    struct Features {
        sampler_anisotropy: bool,
        texture_compression_bc: bool,
        multi_draw_indirect: bool,
        shader_int64: bool,
        descriptor_indexing: bool,
        timeline_semaphore: bool,
        buffer_device_address: bool,
        synchronization2: bool,
        dynamic_rendering: bool,
        maintenance4: bool,
    }

    struct Limits {
        maximum_image_2d: u32,
        maximum_uniform_buffer_range: u32,
        maximum_storage_buffer_range: u32,
        maximum_push_constants_size: u32,
        maximum_compute_workgroup_invocations: u32,
        maximum_compute_shared_memory_size: u32,
        timestamp_period_nanoseconds: f32,
    }

    struct MemoryHeap {
        size: u64,
        device_local: bool,
    }

    #[allow(clippy::struct_excessive_bools)]
    struct QueueFamily {
        index: u32,
        count: u32,
        graphics: bool,
        compute: bool,
        transfer: bool,
        present: bool,
        timestamp_valid_bits: u32,
    }

    struct Extension {
        name: String,
        specification_version: u32,
    }

    struct Surface {
        minimum_images: u32,
        maximum_images: u32,
        current_extent: vk::VkExtent2D,
        minimum_extent: vk::VkExtent2D,
        maximum_extent: vk::VkExtent2D,
        supported_usage_flags: u32,
        supported_composite_alpha: u32,
        formats: Vec<vk::VkSurfaceFormatKHR>,
        present_modes: Vec<vk::VkPresentModeKHR>,
    }

    impl Report {
        fn collect(context: &Context) -> Result<Self, ProbeError> {
            let devices = physical_devices(context)?;
            let mut adapters = Vec::with_capacity(devices.len());
            for device in devices {
                adapters.push(AdapterReport::collect(context, device)?);
            }
            let selected_adapter = adapters
                .iter()
                .enumerate()
                .filter(|(_, adapter)| adapter.baseline_failures.is_empty())
                .max_by_key(|(_, adapter)| adapter.selection_score())
                .map(|(index, _)| index);
            Ok(Self {
                loader_version: context.loader_version,
                selected_adapter,
                adapters,
            })
        }

        #[allow(clippy::too_many_lines)]
        fn print_human(&self) {
            println!("Mulciber Vulkan capability report");
            println!("platform: {}", native::JSON_NAME);
            println!("loader API: {}", version_string(self.loader_version));
            println!("adapters: {}", self.adapters.len());
            match self.selected_adapter {
                Some(index) => {
                    println!("selected adapter: {index} ({})", self.adapters[index].name);
                }
                None => {
                    println!("selected adapter: none (Mulciber Vulkan 1.4 baseline unavailable)");
                }
            }
            for (index, adapter) in self.adapters.iter().enumerate() {
                println!();
                println!("adapter {index}: {}", adapter.name);
                println!(
                    "  API: {}  driver: {}  type: {}",
                    version_string(adapter.api_version),
                    adapter.driver_version_string(),
                    adapter.device_type
                );
                println!(
                    "  PCI IDs: vendor=0x{:04x} device=0x{:04x}",
                    adapter.vendor_id, adapter.device_id
                );
                println!(
                    "  baseline: {}",
                    if adapter.baseline_failures.is_empty() {
                        "compatible"
                    } else {
                        "incompatible"
                    }
                );
                for failure in &adapter.baseline_failures {
                    println!("    - {failure}");
                }
                println!(
                    "  features: anisotropy={} BC={} multi_draw_indirect={} shader_int64={} descriptor_indexing={} timeline_semaphore={} buffer_device_address={} synchronization2={} dynamic_rendering={}",
                    yes_no(adapter.features.sampler_anisotropy),
                    yes_no(adapter.features.texture_compression_bc),
                    yes_no(adapter.features.multi_draw_indirect),
                    yes_no(adapter.features.shader_int64),
                    yes_no(adapter.features.descriptor_indexing),
                    yes_no(adapter.features.timeline_semaphore),
                    yes_no(adapter.features.buffer_device_address),
                    yes_no(adapter.features.synchronization2),
                    yes_no(adapter.features.dynamic_rendering)
                );
                println!(
                    "  relevant extensions: swapchain_maintenance1={} memory_budget={} descriptor_buffer={} mesh_shader={} ray_tracing_pipeline={}",
                    yes_no(adapter.has_extension("VK_KHR_swapchain_maintenance1")),
                    yes_no(adapter.has_extension("VK_EXT_memory_budget")),
                    yes_no(adapter.has_extension("VK_EXT_descriptor_buffer")),
                    yes_no(adapter.has_extension("VK_EXT_mesh_shader")),
                    yes_no(adapter.has_extension("VK_KHR_ray_tracing_pipeline"))
                );
                println!(
                    "  limits: image2D={} uniform_buffer={} storage_buffer={} push_constants={} compute_invocations={} compute_shared_memory={}",
                    adapter.limits.maximum_image_2d,
                    adapter.limits.maximum_uniform_buffer_range,
                    adapter.limits.maximum_storage_buffer_range,
                    adapter.limits.maximum_push_constants_size,
                    adapter.limits.maximum_compute_workgroup_invocations,
                    adapter.limits.maximum_compute_shared_memory_size
                );
                println!(
                    "  memory heaps: {}  queue families: {}  device extensions: {}",
                    adapter.memory_heaps.len(),
                    adapter.queues.len(),
                    adapter.extensions.len()
                );
                for heap in &adapter.memory_heaps {
                    let gib = heap.size / (1 << 30);
                    let hundredths = (heap.size % (1 << 30)) * 100 / (1 << 30);
                    println!(
                        "    heap: {gib}.{hundredths:02} GiB{}",
                        if heap.device_local {
                            " device-local"
                        } else {
                            ""
                        }
                    );
                }
                for queue in &adapter.queues {
                    println!(
                        "    queue {}: count={} graphics={} compute={} transfer={} present={}",
                        queue.index,
                        queue.count,
                        yes_no(queue.graphics),
                        yes_no(queue.compute),
                        yes_no(queue.transfer),
                        yes_no(queue.present)
                    );
                }
                println!(
                    "  surface: images={}..{} extent={}x{}..{}x{} formats={} present_modes={}",
                    adapter.surface.minimum_images,
                    adapter.surface.maximum_images,
                    adapter.surface.minimum_extent.width,
                    adapter.surface.minimum_extent.height,
                    adapter.surface.maximum_extent.width,
                    adapter.surface.maximum_extent.height,
                    adapter.surface.formats.len(),
                    adapter.surface.present_modes.len()
                );
            }
        }

        fn json(&self) -> String {
            let mut output = String::new();
            write!(
                output,
                "{{\n  \"schema_version\": 1,\n  \"backend\": \"vulkan\",\n  \"platform\": \"{}\",\n  \"loader_api_version\": \"{}\",\n  \"loader_api_version_raw\": {},\n  \"selected_adapter_index\": ",
                native::JSON_NAME,
                version_string(self.loader_version),
                self.loader_version
            )
            .expect("writing to a String cannot fail");
            push_optional_usize(&mut output, self.selected_adapter);
            output.push_str(",\n  \"adapters\": [");
            for (index, adapter) in self.adapters.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push('\n');
                adapter.push_json(&mut output, index);
            }
            output.push_str("\n  ]\n}");
            output
        }
    }

    impl AdapterReport {
        #[allow(clippy::similar_names, clippy::too_many_lines)]
        fn collect(context: &Context, device: vk::VkPhysicalDevice) -> Result<Self, ProbeError> {
            let mut properties = vk::VkPhysicalDeviceProperties::default();
            // SAFETY: The enumerated physical device and writable output are valid.
            unsafe {
                context.functions.get_properties.expect("loaded function")(
                    device,
                    &raw mut properties,
                );
            }

            let mut features12 = vk::VkPhysicalDeviceVulkan12Features {
                sType: vk::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES,
                ..Default::default()
            };
            let mut features13 = vk::VkPhysicalDeviceVulkan13Features {
                sType: vk::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
                ..Default::default()
            };
            let mut features2 = vk::VkPhysicalDeviceFeatures2 {
                sType: vk::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2,
                ..Default::default()
            };
            if properties.apiVersion >= API_VERSION_1_3 {
                features12.pNext = (&raw mut features13).cast();
                features2.pNext = (&raw mut features12).cast();
            } else if properties.apiVersion >= API_VERSION_1_2 {
                features2.pNext = (&raw mut features12).cast();
            }
            // SAFETY: The pNext feature chain and output structures are writable.
            unsafe {
                context.functions.get_features2.expect("loaded function")(
                    device,
                    &raw mut features2,
                );
            }

            let mut memory = vk::VkPhysicalDeviceMemoryProperties::default();
            // SAFETY: The output structure is writable for this enumerated device.
            unsafe {
                context
                    .functions
                    .get_memory_properties
                    .expect("loaded function")(device, &raw mut memory);
            }
            let memory_heaps = memory.memoryHeaps[..memory.memoryHeapCount as usize]
                .iter()
                .map(|heap| MemoryHeap {
                    size: heap.size,
                    device_local: heap.flags & vk::VK_MEMORY_HEAP_DEVICE_LOCAL_BIT as u32 != 0,
                })
                .collect();

            let queues = queue_families(context, device)?;
            let extensions = device_extensions(context, device)?;
            let surface = Surface::collect(context, device)?;
            let features = Features {
                sampler_anisotropy: features2.features.samplerAnisotropy == vk::VK_TRUE,
                texture_compression_bc: features2.features.textureCompressionBC == vk::VK_TRUE,
                multi_draw_indirect: features2.features.multiDrawIndirect == vk::VK_TRUE,
                shader_int64: features2.features.shaderInt64 == vk::VK_TRUE,
                descriptor_indexing: features12.descriptorIndexing == vk::VK_TRUE,
                timeline_semaphore: features12.timelineSemaphore == vk::VK_TRUE,
                buffer_device_address: features12.bufferDeviceAddress == vk::VK_TRUE,
                synchronization2: features13.synchronization2 == vk::VK_TRUE,
                dynamic_rendering: features13.dynamicRendering == vk::VK_TRUE,
                maintenance4: features13.maintenance4 == vk::VK_TRUE,
            };
            let mut baseline_failures = Vec::new();
            if properties.apiVersion < API_VERSION_1_4 {
                baseline_failures.push(format!(
                    "API {} is below required Vulkan 1.4",
                    version_string(properties.apiVersion)
                ));
            }
            if !extensions
                .iter()
                .any(|extension| extension.name == "VK_KHR_swapchain")
            {
                baseline_failures.push("VK_KHR_swapchain is unavailable".into());
            }
            if !features.synchronization2 {
                baseline_failures.push("synchronization2 is unavailable".into());
            }
            if !features.dynamic_rendering {
                baseline_failures.push("dynamicRendering is unavailable".into());
            }
            if !queues.iter().any(|queue| queue.graphics && queue.present) {
                baseline_failures.push(format!(
                    "no queue family supports both graphics and this {} surface",
                    native::DISPLAY_NAME
                ));
            }
            if surface.formats.is_empty() {
                baseline_failures.push(format!(
                    "the {} surface exposes no formats",
                    native::DISPLAY_NAME
                ));
            }
            if !surface
                .present_modes
                .contains(&vk::VK_PRESENT_MODE_FIFO_KHR)
            {
                baseline_failures.push(format!(
                    "the {} surface does not expose FIFO presentation",
                    native::DISPLAY_NAME
                ));
            }
            Ok(Self {
                name: fixed_c_string(&properties.deviceName),
                api_version: properties.apiVersion,
                driver_version: properties.driverVersion,
                vendor_id: properties.vendorID,
                device_id: properties.deviceID,
                device_type: device_type(properties.deviceType),
                pipeline_cache_uuid: properties.pipelineCacheUUID,
                features,
                limits: Limits {
                    maximum_image_2d: properties.limits.maxImageDimension2D,
                    maximum_uniform_buffer_range: properties.limits.maxUniformBufferRange,
                    maximum_storage_buffer_range: properties.limits.maxStorageBufferRange,
                    maximum_push_constants_size: properties.limits.maxPushConstantsSize,
                    maximum_compute_workgroup_invocations: properties
                        .limits
                        .maxComputeWorkGroupInvocations,
                    maximum_compute_shared_memory_size: properties
                        .limits
                        .maxComputeSharedMemorySize,
                    timestamp_period_nanoseconds: properties.limits.timestampPeriod,
                },
                memory_heaps,
                queues,
                extensions,
                surface,
                baseline_failures,
            })
        }

        fn selection_score(&self) -> u8 {
            match self.device_type {
                "discrete_gpu" => 2,
                "integrated_gpu" => 1,
                _ => 0,
            }
        }

        fn driver_version_string(&self) -> String {
            driver_version_string(self.vendor_id, self.driver_version)
        }

        fn has_extension(&self, name: &str) -> bool {
            self.extensions
                .iter()
                .any(|extension| extension.name == name)
        }

        #[allow(clippy::too_many_lines)]
        fn push_json(&self, output: &mut String, index: usize) {
            write!(
                output,
                "    {{\n      \"index\": {index},\n      \"name\": "
            )
            .expect("writing to a String cannot fail");
            push_json_string(output, &self.name);
            write!(
                output,
                ",\n      \"api_version\": \"{}\",\n      \"api_version_raw\": {},\n      \"driver_version\": \"{}\",\n      \"driver_version_raw\": {},\n      \"vendor_id\": {},\n      \"device_id\": {},\n      \"device_type\": \"{}\",\n      \"pipeline_cache_uuid\": \"{}\",\n      \"baseline_compatible\": {},\n      \"baseline_failures\": [",
                version_string(self.api_version),
                self.api_version,
                self.driver_version_string(),
                self.driver_version,
                self.vendor_id,
                self.device_id,
                self.device_type,
                hex_bytes(&self.pipeline_cache_uuid),
                self.baseline_failures.is_empty()
            )
            .expect("writing to a String cannot fail");
            for (failure_index, failure) in self.baseline_failures.iter().enumerate() {
                if failure_index != 0 {
                    output.push_str(", ");
                }
                push_json_string(output, failure);
            }
            write!(
                output,
                "],\n      \"features\": {{\n        \"sampler_anisotropy\": {},\n        \"texture_compression_bc\": {},\n        \"multi_draw_indirect\": {},\n        \"shader_int64\": {},\n        \"descriptor_indexing\": {},\n        \"timeline_semaphore\": {},\n        \"buffer_device_address\": {},\n        \"synchronization2\": {},\n        \"dynamic_rendering\": {},\n        \"maintenance4\": {}\n      }},\n      \"relevant_extensions\": {{\n        \"swapchain_maintenance1\": {},\n        \"memory_budget\": {},\n        \"descriptor_buffer\": {},\n        \"mesh_shader\": {},\n        \"ray_tracing_pipeline\": {}\n      }},\n      \"limits\": {{\n        \"maximum_image_2d\": {},\n        \"maximum_uniform_buffer_range\": {},\n        \"maximum_storage_buffer_range\": {},\n        \"maximum_push_constants_size\": {},\n        \"maximum_compute_workgroup_invocations\": {},\n        \"maximum_compute_shared_memory_size\": {},\n        \"timestamp_period_nanoseconds\": {}\n      }},\n      \"memory_heaps\": [",
                self.features.sampler_anisotropy,
                self.features.texture_compression_bc,
                self.features.multi_draw_indirect,
                self.features.shader_int64,
                self.features.descriptor_indexing,
                self.features.timeline_semaphore,
                self.features.buffer_device_address,
                self.features.synchronization2,
                self.features.dynamic_rendering,
                self.features.maintenance4,
                self.has_extension("VK_KHR_swapchain_maintenance1"),
                self.has_extension("VK_EXT_memory_budget"),
                self.has_extension("VK_EXT_descriptor_buffer"),
                self.has_extension("VK_EXT_mesh_shader"),
                self.has_extension("VK_KHR_ray_tracing_pipeline"),
                self.limits.maximum_image_2d,
                self.limits.maximum_uniform_buffer_range,
                self.limits.maximum_storage_buffer_range,
                self.limits.maximum_push_constants_size,
                self.limits.maximum_compute_workgroup_invocations,
                self.limits.maximum_compute_shared_memory_size,
                self.limits.timestamp_period_nanoseconds
            )
            .expect("writing to a String cannot fail");
            for (heap_index, heap) in self.memory_heaps.iter().enumerate() {
                if heap_index != 0 {
                    output.push_str(", ");
                }
                write!(
                    output,
                    "{{\"index\": {heap_index}, \"size_bytes\": {}, \"device_local\": {}}}",
                    heap.size, heap.device_local
                )
                .expect("writing to a String cannot fail");
            }
            output.push_str("],\n      \"queue_families\": [");
            for (queue_index, queue) in self.queues.iter().enumerate() {
                if queue_index != 0 {
                    output.push_str(", ");
                }
                write!(
                    output,
                    "{{\"index\": {}, \"count\": {}, \"graphics\": {}, \"compute\": {}, \"transfer\": {}, \"present\": {}, \"timestamp_valid_bits\": {}}}",
                    queue.index,
                    queue.count,
                    queue.graphics,
                    queue.compute,
                    queue.transfer,
                    queue.present,
                    queue.timestamp_valid_bits
                )
                .expect("writing to a String cannot fail");
            }
            output.push_str("],\n      \"extensions\": [");
            for (extension_index, extension) in self.extensions.iter().enumerate() {
                if extension_index != 0 {
                    output.push_str(", ");
                }
                output.push_str("{\"name\": ");
                push_json_string(output, &extension.name);
                write!(
                    output,
                    ", \"specification_version\": {}}}",
                    extension.specification_version
                )
                .expect("writing to a String cannot fail");
            }
            write!(
                output,
                "],\n      \"surface\": {{\n        \"minimum_image_count\": {},\n        \"maximum_image_count\": {},\n        \"current_extent\": [{}, {}],\n        \"minimum_extent\": [{}, {}],\n        \"maximum_extent\": [{}, {}],\n        \"supported_usage_flags\": {},\n        \"supported_composite_alpha\": {},\n        \"formats\": [",
                self.surface.minimum_images,
                self.surface.maximum_images,
                self.surface.current_extent.width,
                self.surface.current_extent.height,
                self.surface.minimum_extent.width,
                self.surface.minimum_extent.height,
                self.surface.maximum_extent.width,
                self.surface.maximum_extent.height,
                self.surface.supported_usage_flags,
                self.surface.supported_composite_alpha
            )
            .expect("writing to a String cannot fail");
            for (format_index, format) in self.surface.formats.iter().enumerate() {
                if format_index != 0 {
                    output.push_str(", ");
                }
                write!(
                    output,
                    "{{\"format\": {}, \"color_space\": {}}}",
                    format.format, format.colorSpace
                )
                .expect("writing to a String cannot fail");
            }
            output.push_str("],\n        \"present_modes\": [");
            for (mode_index, mode) in self.surface.present_modes.iter().enumerate() {
                if mode_index != 0 {
                    output.push_str(", ");
                }
                write!(output, "{{\"value\": {mode}, \"name\": ")
                    .expect("writing to a String cannot fail");
                push_json_string(output, present_mode_name(*mode));
                output.push('}');
            }
            output.push_str("]\n      }\n    }");
        }
    }

    impl Surface {
        fn collect(context: &Context, device: vk::VkPhysicalDevice) -> Result<Self, ProbeError> {
            let mut capabilities = vk::VkSurfaceCapabilitiesKHR::default();
            check(
                // SAFETY: Device/surface are live and output is writable.
                unsafe {
                    context
                        .functions
                        .get_surface_capabilities
                        .expect("loaded function")(
                        device, context.surface, &raw mut capabilities
                    )
                },
                "vkGetPhysicalDeviceSurfaceCapabilitiesKHR",
            )?;
            Ok(Self {
                minimum_images: capabilities.minImageCount,
                maximum_images: capabilities.maxImageCount,
                current_extent: capabilities.currentExtent,
                minimum_extent: capabilities.minImageExtent,
                maximum_extent: capabilities.maxImageExtent,
                supported_usage_flags: capabilities.supportedUsageFlags,
                supported_composite_alpha: capabilities.supportedCompositeAlpha,
                formats: surface_formats(context, device)?,
                present_modes: present_modes(context, device)?,
            })
        }
    }

    fn physical_devices(context: &Context) -> Result<Vec<vk::VkPhysicalDevice>, ProbeError> {
        let function = context
            .functions
            .enumerate_physical_devices
            .expect("loaded function");
        let mut count = 0;
        check_enumeration(
            // SAFETY: This is the Vulkan two-call enumeration pattern.
            unsafe { function(context.instance, &raw mut count, ptr::null_mut()) },
            "enumerate physical devices",
        )?;
        let mut devices = vec![ptr::null_mut(); count as usize];
        check_enumeration(
            // SAFETY: Storage contains `count` writable handles.
            unsafe { function(context.instance, &raw mut count, devices.as_mut_ptr()) },
            "enumerate physical devices",
        )?;
        devices.truncate(count as usize);
        Ok(devices)
    }

    fn queue_families(
        context: &Context,
        device: vk::VkPhysicalDevice,
    ) -> Result<Vec<QueueFamily>, ProbeError> {
        let function = context
            .functions
            .get_queue_families
            .expect("loaded function");
        let mut count = 0;
        // SAFETY: The count output is writable.
        unsafe { function(device, &raw mut count, ptr::null_mut()) };
        let mut properties = vec![vk::VkQueueFamilyProperties::default(); count as usize];
        // SAFETY: Storage contains `count` writable entries.
        unsafe { function(device, &raw mut count, properties.as_mut_ptr()) };
        properties.truncate(count as usize);
        properties
            .iter()
            .enumerate()
            .map(|(index, property)| {
                let mut present = vk::VK_FALSE;
                check(
                    // SAFETY: Device/surface/family and writable output are valid.
                    unsafe {
                        context
                            .functions
                            .get_surface_support
                            .expect("loaded function")(
                            device,
                            u32::try_from(index).expect("queue family index fits u32"),
                            context.surface,
                            &raw mut present,
                        )
                    },
                    "vkGetPhysicalDeviceSurfaceSupportKHR",
                )?;
                Ok(QueueFamily {
                    index: u32::try_from(index).expect("queue family index fits u32"),
                    count: property.queueCount,
                    graphics: property.queueFlags & vk::VK_QUEUE_GRAPHICS_BIT as u32 != 0,
                    compute: property.queueFlags & vk::VK_QUEUE_COMPUTE_BIT as u32 != 0,
                    transfer: property.queueFlags & vk::VK_QUEUE_TRANSFER_BIT as u32 != 0,
                    present: present == vk::VK_TRUE,
                    timestamp_valid_bits: property.timestampValidBits,
                })
            })
            .collect()
    }

    fn device_extensions(
        context: &Context,
        device: vk::VkPhysicalDevice,
    ) -> Result<Vec<Extension>, ProbeError> {
        let function = context
            .functions
            .enumerate_device_extensions
            .expect("loaded function");
        let mut count = 0;
        check_enumeration(
            // SAFETY: This is the Vulkan two-call enumeration pattern.
            unsafe { function(device, ptr::null(), &raw mut count, ptr::null_mut()) },
            "enumerate device extensions",
        )?;
        let mut properties = vec![vk::VkExtensionProperties::default(); count as usize];
        check_enumeration(
            // SAFETY: Storage contains `count` writable entries.
            unsafe { function(device, ptr::null(), &raw mut count, properties.as_mut_ptr()) },
            "enumerate device extensions",
        )?;
        properties.truncate(count as usize);
        let mut extensions: Vec<_> = properties
            .iter()
            .map(|property| Extension {
                name: fixed_c_string(&property.extensionName),
                specification_version: property.specVersion,
            })
            .collect();
        extensions.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(extensions)
    }

    fn surface_formats(
        context: &Context,
        device: vk::VkPhysicalDevice,
    ) -> Result<Vec<vk::VkSurfaceFormatKHR>, ProbeError> {
        let function = context
            .functions
            .get_surface_formats
            .expect("loaded function");
        let mut count = 0;
        check_enumeration(
            // SAFETY: This is the Vulkan two-call enumeration pattern.
            unsafe { function(device, context.surface, &raw mut count, ptr::null_mut()) },
            "enumerate surface formats",
        )?;
        let mut formats = vec![vk::VkSurfaceFormatKHR::default(); count as usize];
        check_enumeration(
            // SAFETY: Storage contains `count` writable entries.
            unsafe {
                function(
                    device,
                    context.surface,
                    &raw mut count,
                    formats.as_mut_ptr(),
                )
            },
            "enumerate surface formats",
        )?;
        formats.truncate(count as usize);
        Ok(formats)
    }

    fn present_modes(
        context: &Context,
        device: vk::VkPhysicalDevice,
    ) -> Result<Vec<vk::VkPresentModeKHR>, ProbeError> {
        let function = context
            .functions
            .get_present_modes
            .expect("loaded function");
        let mut count = 0;
        check_enumeration(
            // SAFETY: This is the Vulkan two-call enumeration pattern.
            unsafe { function(device, context.surface, &raw mut count, ptr::null_mut()) },
            "enumerate present modes",
        )?;
        let mut modes = vec![0; count as usize];
        check_enumeration(
            // SAFETY: Storage contains `count` writable entries.
            unsafe { function(device, context.surface, &raw mut count, modes.as_mut_ptr()) },
            "enumerate present modes",
        )?;
        modes.truncate(count as usize);
        Ok(modes)
    }

    fn instance_extensions(entry: &Entry) -> Result<Vec<Vec<u8>>, ProbeError> {
        let function = entry
            .enumerate_instance_extension_properties
            .expect("loaded function");
        let mut count = 0;
        check_enumeration(
            // SAFETY: This is the Vulkan two-call enumeration pattern.
            unsafe { function(ptr::null(), &raw mut count, ptr::null_mut()) },
            "enumerate instance extensions",
        )?;
        let mut properties = vec![vk::VkExtensionProperties::default(); count as usize];
        check_enumeration(
            // SAFETY: Storage contains `count` writable entries.
            unsafe { function(ptr::null(), &raw mut count, properties.as_mut_ptr()) },
            "enumerate instance extensions",
        )?;
        properties.truncate(count as usize);
        Ok(properties
            .iter()
            .map(|property| fixed_c_bytes(&property.extensionName))
            .collect())
    }

    pub fn run() -> Result<(), ProbeError> {
        let json = match env::args_os().skip(1).collect::<Vec<_>>().as_slice() {
            [] => false,
            [argument] if argument == "--json" => true,
            _ => return Err(ProbeError("usage: mulciber-vulkan-info [--json]".into())),
        };
        let window = Window::new("Mulciber Vulkan capability surface", 64, 64, false)
            .map_err(|error| ProbeError(error.to_string()))?;
        let context = Context::new(&window)?;
        let report = Report::collect(&context)?;
        if json {
            println!("{}", report.json());
        } else {
            report.print_human();
        }
        Ok(())
    }

    const fn make_api_version(variant: u32, major: u32, minor: u32, patch: u32) -> u32 {
        (variant << 29) | (major << 22) | (minor << 12) | patch
    }

    fn version_string(version: u32) -> String {
        format!(
            "{}.{}.{}",
            (version >> 22) & 0x7f,
            (version >> 12) & 0x3ff,
            version & 0xfff
        )
    }

    fn driver_version_string(vendor_id: u32, version: u32) -> String {
        if vendor_id == 0x10de {
            let major = (version >> 22) & 0x3ff;
            let minor = (version >> 14) & 0xff;
            let secondary = (version >> 6) & 0xff;
            let tertiary = version & 0x3f;
            if secondary == 0 && tertiary == 0 {
                format!("{major}.{minor}")
            } else {
                format!("{major}.{minor}.{secondary}.{tertiary}")
            }
        } else {
            version.to_string()
        }
    }

    fn device_type(value: vk::VkPhysicalDeviceType) -> &'static str {
        match value {
            vk::VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU => "discrete_gpu",
            vk::VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU => "integrated_gpu",
            vk::VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU => "virtual_gpu",
            vk::VK_PHYSICAL_DEVICE_TYPE_CPU => "cpu",
            vk::VK_PHYSICAL_DEVICE_TYPE_OTHER => "other",
            _ => "unknown",
        }
    }

    fn present_mode_name(value: vk::VkPresentModeKHR) -> &'static str {
        match value {
            vk::VK_PRESENT_MODE_IMMEDIATE_KHR => "immediate",
            vk::VK_PRESENT_MODE_MAILBOX_KHR => "mailbox",
            vk::VK_PRESENT_MODE_FIFO_KHR => "fifo",
            vk::VK_PRESENT_MODE_FIFO_LATEST_READY_KHR => "fifo_latest_ready",
            vk::VK_PRESENT_MODE_FIFO_RELAXED_KHR => "fifo_relaxed",
            _ => "unknown",
        }
    }

    fn fixed_c_bytes<const N: usize>(value: &[c_char; N]) -> Vec<u8> {
        value
            .iter()
            .map(|byte| byte.cast_unsigned())
            .take_while(|byte| *byte != 0)
            .collect()
    }

    fn fixed_c_string<const N: usize>(value: &[c_char; N]) -> String {
        String::from_utf8_lossy(&fixed_c_bytes(value)).into_owned()
    }

    fn hex_bytes(bytes: &[u8]) -> String {
        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
        }
        output
    }

    fn yes_no(value: bool) -> &'static str {
        if value { "yes" } else { "no" }
    }

    fn push_optional_usize(output: &mut String, value: Option<usize>) {
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
            return Err(ProbeError(format!("Vulkan loader is missing {name}")));
        }
        debug_assert_eq!(mem::size_of::<T>(), mem::size_of::<*mut c_void>());
        // SAFETY: The caller pairs the exported symbol with its generated function-pointer type.
        Ok(unsafe { mem::transmute_copy(&address) })
    }

    unsafe fn load_proc<T: Copy>(
        function: vk::PFN_vkVoidFunction,
        name: &str,
    ) -> Result<T, ProbeError> {
        let function = function.ok_or_else(|| ProbeError(format!("Vulkan is missing {name}")))?;
        debug_assert_eq!(mem::size_of::<T>(), mem::size_of_val(&function));
        // SAFETY: The caller pairs the Vulkan name with its generated function-pointer type.
        Ok(unsafe { mem::transmute_copy(&function) })
    }

    #[cfg(test)]
    mod tests {
        use super::{driver_version_string, push_json_string, version_string};

        #[test]
        fn version_is_decoded() {
            assert_eq!(version_string((1 << 22) | (4 << 12) | 0x0164), "1.4.356");
        }

        #[test]
        fn json_strings_are_escaped() {
            let mut output = String::new();
            push_json_string(&mut output, "a\"b\\c\n");
            assert_eq!(output, "\"a\\\"b\\\\c\\n\"");
        }

        #[test]
        fn nvidia_driver_version_is_decoded() {
            assert_eq!(driver_version_string(0x10de, 2_480_242_688), "591.86");
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub use platform::run;
