use super::*;
use std::string::ToString;

#[cfg(target_os = "windows")]
const SURFACE_EXTENSION: &CStr = c"VK_KHR_win32_surface";
#[cfg(target_os = "linux")]
const SURFACE_EXTENSION: &CStr = c"VK_KHR_xlib_surface";

#[test]
fn ordinary_instance_requirements_exclude_sdk_dependencies() {
    let requirements = InstanceRequirements::new(SURFACE_EXTENSION, false);
    assert!(requirements.layers.is_empty());
    assert_eq!(
        requirements.extensions,
        [c"VK_KHR_surface", SURFACE_EXTENSION]
    );
}

#[test]
fn validation_opt_in_requires_both_the_layer_and_debug_extension() {
    let requirements = InstanceRequirements::new(SURFACE_EXTENSION, true);
    assert_eq!(requirements.layers, [c"VK_LAYER_KHRONOS_validation"]);
    assert!(requirements.extensions.contains(&c"VK_EXT_debug_utils"));
    for name in requirements.layers {
        assert!(require_name(&[], name, "Vulkan validation layer").is_err());
    }
}

#[test]
#[ignore = "requires the native Vulkan loader; creates no window or surface"]
fn native_instance_without_validation_never_queries_layers_or_loads_debug_functions() {
    let mut entry = Entry::load().expect("native Vulkan loader");
    // An attempted layer query would panic. Ordinary startup must not need the
    // SDK's layer enumeration, irrespective of what is installed on this host.
    entry.enumerate_instance_layer_properties = None;
    let instance = Instance::create(entry, SURFACE_EXTENSION, false).expect("SDK-free instance");
    assert!(instance.debug_messenger.is_null());
    assert!(instance.functions.create_debug_utils_messenger.is_none());
    assert!(instance.functions.destroy_debug_utils_messenger.is_none());
    assert!(instance.surface.is_null());
    drop(instance);
}

#[test]
#[ignore = "requires the native Vulkan loader; creates no window or surface"]
fn native_instance_validation_rejects_missing_layer() {
    unsafe extern "C" fn no_layers(
        count: *mut u32,
        _properties: *mut vk::VkLayerProperties,
    ) -> vk::VkResult {
        // SAFETY: The enumeration caller supplies a writable count.
        unsafe { *count = 0 };
        vk::VK_SUCCESS
    }
    let mut entry = Entry::load().expect("native Vulkan loader");
    entry.enumerate_instance_layer_properties = Some(no_layers);
    let error = Instance::create(entry, SURFACE_EXTENSION, true)
        .err()
        .expect("explicit validation must not silently fall back");
    assert!(error.to_string().contains("VK_LAYER_KHRONOS_validation"));
}

#[test]
#[ignore = "requires native Vulkan and the validation layer; creates no window or surface"]
fn native_instance_with_validation_keeps_the_debug_messenger() {
    VALIDATION_MESSAGE_COUNT.store(0, Ordering::Relaxed);
    let instance = Instance::create(
        Entry::load().expect("native Vulkan loader"),
        SURFACE_EXTENSION,
        true,
    )
    .expect("validated instance");
    assert!(!instance.debug_messenger.is_null());
    assert!(instance.functions.create_debug_utils_messenger.is_some());
    assert!(instance.functions.destroy_debug_utils_messenger.is_some());
    drop(instance);
    assert_eq!(VALIDATION_MESSAGE_COUNT.load(Ordering::Relaxed), 0);
}

/// Only presentation support is substituted: no surface exists in this test.
/// Adapter discovery, logical device creation and every device symbol are native.
pub(super) fn windowless_device(validation: bool) -> Device {
    unsafe extern "C" fn presentation_support(
        _device: vk::VkPhysicalDevice,
        _family: u32,
        _surface: vk::VkSurfaceKHR,
        supported: *mut vk::VkBool32,
    ) -> vk::VkResult {
        // SAFETY: The caller provides a writable output.
        unsafe { *supported = vk::VK_TRUE };
        vk::VK_SUCCESS
    }
    let mut instance = Instance::create(
        Entry::load().expect("native Vulkan loader"),
        SURFACE_EXTENSION,
        validation,
    )
    .expect("native instance");
    instance.functions.get_surface_support = Some(presentation_support);
    // Present-timing queries require a real surface and are outside this test.
    instance.surface_capabilities2 = false;
    Device::new(instance).expect("native device and complete device function table")
}

#[test]
#[ignore = "requires native Vulkan; creates no window or surface"]
fn native_instance_device_without_validation_loads_all_functions() {
    let device = windowless_device(false);
    assert!(device.functions.cmd_begin_debug_utils_label.is_none());
    assert!(device.functions.cmd_end_debug_utils_label.is_none());
}
