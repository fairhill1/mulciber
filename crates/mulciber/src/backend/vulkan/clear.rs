use core::ffi::{CStr, c_char, c_void};
use core::marker::PhantomData;
use core::{mem, ptr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::vec::Vec;
use std::{eprintln, format, vec};

use mulciber_platform::{SurfaceTarget, WindowMetrics};

use super::{platform, vk};
use crate::{
    ClearColor, FrameAcquire, FrameDisposition, GraphicsError, SurfaceExtent, SurfaceInfo,
    SurfaceUnavailable,
};

pub(crate) const BACKEND_NAME: &str = "Vulkan";

const API_VERSION_1_4: u32 = make_api_version(0, 1, 4, 0);
const UINT64_MAX: u64 = u64::MAX;
static VALIDATION_MESSAGE_COUNT: AtomicU32 = AtomicU32::new(0);

pub(crate) struct ClearSurface<'window> {
    device: Option<Device>,
    swapchain: Swapchain,
    retired: Vec<Swapchain>,
    command_pool: vk::VkCommandPool,
    command_buffer: vk::VkCommandBuffer,
    image_available: vk::VkSemaphore,
    acquire_fence: vk::VkFence,
    frame_fence: vk::VkFence,
    frame_pending: bool,
    info: SurfaceInfo,
    recreate_after_present: bool,
    deferred_error: Option<GraphicsError>,
    _target: PhantomData<SurfaceTarget<'window>>,
}

pub(crate) struct ClearFrame<'surface, 'window> {
    surface: &'surface mut ClearSurface<'window>,
    image_index: u32,
    disposed: bool,
}

impl<'window> ClearSurface<'window> {
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn new(
        target: SurfaceTarget<'window>,
        initial_metrics: WindowMetrics,
    ) -> Result<Self, GraphicsError> {
        let extent = surface_extent(initial_metrics);
        if extent.is_empty() {
            return Err(error(
                "cannot create a Vulkan surface for an empty drawable extent",
            ));
        }
        VALIDATION_MESSAGE_COUNT.store(0, Ordering::Relaxed);
        let entry = Entry::load()?;
        let instance = Instance::new(entry, &target)?;
        let device = Device::new(instance)?;
        let mut surface = Self {
            device: Some(device),
            swapchain: Swapchain::default(),
            retired: Vec::new(),
            command_pool: ptr::null_mut(),
            command_buffer: ptr::null_mut(),
            image_available: ptr::null_mut(),
            acquire_fence: ptr::null_mut(),
            frame_fence: ptr::null_mut(),
            frame_pending: false,
            info: SurfaceInfo::initial(extent).expect("extent was checked"),
            recreate_after_present: false,
            deferred_error: None,
            _target: PhantomData,
        };
        surface.create_frame_resources()?;
        surface.recreate_swapchain(extent, false)?;
        Ok(surface)
    }

    pub(crate) const fn info(&self) -> SurfaceInfo {
        self.info
    }

    pub(crate) fn acquire(
        &mut self,
        metrics: WindowMetrics,
    ) -> Result<FrameAcquire<ClearFrame<'_, 'window>>, GraphicsError> {
        self.acquire_image(metrics).map(|acquisition| {
            acquisition.map_ready(|image_index| ClearFrame {
                surface: self,
                image_index,
                disposed: false,
            })
        })
    }

    fn acquire_image(
        &mut self,
        metrics: WindowMetrics,
    ) -> Result<FrameAcquire<u32>, GraphicsError> {
        if let Some(error) = self.deferred_error.take() {
            return Err(error);
        }
        self.wait_for_frame()?;
        let extent = surface_extent(metrics);
        if extent.is_empty() {
            return Ok(FrameAcquire::Unavailable(SurfaceUnavailable::Suspended));
        }
        if extent != self.info.extent() || self.recreate_after_present {
            self.recreate_swapchain(extent, true)?;
            return Ok(FrameAcquire::Reconfigured(self.info));
        }

        let mut image_index = 0;
        let result = unsafe {
            // SAFETY: The swapchain, semaphore, fence, and output pointer are live and idle.
            self.device()
                .functions
                .acquire_next_image
                .expect("loaded function")(
                self.device().handle,
                self.swapchain.handle,
                platform::acquire_timeout(),
                self.image_available,
                self.acquire_fence,
                &raw mut image_index,
            )
        };
        if result == vk::VK_NOT_READY {
            return Ok(FrameAcquire::Unavailable(
                SurfaceUnavailable::DrawableUnavailable,
            ));
        }
        if result == vk::VK_TIMEOUT {
            return Ok(FrameAcquire::Unavailable(SurfaceUnavailable::TimedOut));
        }
        if result == vk::VK_ERROR_OUT_OF_DATE_KHR {
            self.recreate_swapchain(extent, true)?;
            return Ok(FrameAcquire::Reconfigured(self.info));
        }
        if result == vk::VK_SUBOPTIMAL_KHR {
            self.recreate_after_present = true;
        } else {
            check(result, "vkAcquireNextImageKHR")?;
        }
        self.wait_and_reset_fence(self.acquire_fence, "image-acquisition fence")?;
        let slot = usize::try_from(image_index).map_err(|_| error("invalid image index"))?;
        if slot >= self.swapchain.images.len() {
            return Err(error("driver returned an invalid swapchain image index"));
        }
        if self.swapchain.presented[slot] {
            self.destroy_all_retired_swapchains();
        }
        Ok(FrameAcquire::Ready(image_index))
    }

    pub(crate) fn shutdown(mut self) -> Result<(), GraphicsError> {
        let result = self.finish();
        self.destroy();
        mem::forget(self);
        result
    }

    fn device(&self) -> &Device {
        self.device.as_ref().expect("device is live")
    }

    fn create_frame_resources(&mut self) -> Result<(), GraphicsError> {
        let command_pool = {
            let device = self.device();
            let pool_info = vk::VkCommandPoolCreateInfo {
                sType: vk::VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
                flags: vk::VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT as u32,
                queueFamilyIndex: device.adapter.queue_family,
                ..Default::default()
            };
            let mut command_pool = ptr::null_mut();
            check(
                unsafe {
                    // SAFETY: Device and create info are valid and output storage is writable.
                    device
                        .functions
                        .create_command_pool
                        .expect("loaded function")(
                        device.handle,
                        &raw const pool_info,
                        ptr::null(),
                        &raw mut command_pool,
                    )
                },
                "vkCreateCommandPool",
            )?;
            command_pool
        };
        self.command_pool = command_pool;
        let command_buffer = {
            let device = self.device();
            let allocate_info = vk::VkCommandBufferAllocateInfo {
                sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                commandPool: command_pool,
                level: vk::VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                commandBufferCount: 1,
                ..Default::default()
            };
            let mut command_buffer = ptr::null_mut();
            check(
                unsafe {
                    // SAFETY: Pool is live and output storage is writable.
                    device
                        .functions
                        .allocate_command_buffers
                        .expect("loaded function")(
                        device.handle,
                        &raw const allocate_info,
                        &raw mut command_buffer,
                    )
                },
                "vkAllocateCommandBuffers",
            )?;
            command_buffer
        };
        self.command_buffer = command_buffer;
        self.image_available = create_semaphore(self.device(), "image-available semaphore")?;
        self.acquire_fence = create_fence(self.device(), false, "image-acquisition fence")?;
        self.frame_fence = create_fence(self.device(), false, "frame fence")?;
        Ok(())
    }

    fn recreate_swapchain(
        &mut self,
        requested: SurfaceExtent,
        advance_generation: bool,
    ) -> Result<(), GraphicsError> {
        self.wait_for_frame()?;
        let device = self.device();
        let mut capabilities = vk::VkSurfaceCapabilitiesKHR::default();
        check(
            unsafe {
                // SAFETY: Adapter/surface are live and output storage is writable.
                device
                    .instance
                    .functions
                    .get_surface_capabilities
                    .expect("loaded function")(
                    device.adapter.handle,
                    device.instance.surface,
                    &raw mut capabilities,
                )
            },
            "vkGetPhysicalDeviceSurfaceCapabilitiesKHR",
        )?;
        let formats = surface_formats(device)?;
        let format = choose_surface_format(&formats)
            .ok_or_else(|| error("surface exposes no supported sRGB format"))?;
        require_fifo_present_mode(device)?;
        let extent = choose_extent(capabilities, requested);
        let extent_info = SurfaceExtent::new(extent.width, extent.height);
        let mut image_count = capabilities.minImageCount.saturating_add(1).max(3);
        if capabilities.maxImageCount != 0 {
            image_count = image_count.min(capabilities.maxImageCount);
        }
        let composite_alpha = choose_composite_alpha(capabilities.supportedCompositeAlpha)
            .ok_or_else(|| error("surface exposes no supported composite-alpha mode"))?;
        let create_info = vk::VkSwapchainCreateInfoKHR {
            sType: vk::VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR,
            surface: device.instance.surface,
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
            oldSwapchain: self.swapchain.handle,
            ..Default::default()
        };
        let mut handle = ptr::null_mut();
        check(
            unsafe {
                // SAFETY: Device, surface, old swapchain, and create info are valid.
                device.functions.create_swapchain.expect("loaded function")(
                    device.handle,
                    &raw const create_info,
                    ptr::null(),
                    &raw mut handle,
                )
            },
            "vkCreateSwapchainKHR",
        )?;
        let mut next = Swapchain {
            handle,
            format: format.format,
            extent,
            ..Default::default()
        };
        if let Err(error) = populate_swapchain(device, &mut next) {
            destroy_swapchain(device, next);
            return Err(error);
        }
        let old = mem::replace(&mut self.swapchain, next);
        if !old.handle.is_null() {
            self.retired.push(old);
        }
        self.info = if advance_generation {
            self.info
                .reconfigured(extent_info)
                .ok_or_else(|| error("surface generation counter exhausted"))?
        } else {
            SurfaceInfo::initial(extent_info).expect("Vulkan returned a non-empty extent")
        };
        self.recreate_after_present = false;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn present(
        &mut self,
        image_index: u32,
        color: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let slot = usize::try_from(image_index).map_err(|_| error("invalid image index"))?;
        let image = self.swapchain.images[slot];
        let view = self.swapchain.views[slot];
        let render_finished = self.swapchain.render_finished[slot];
        let old_layout = if self.swapchain.initialized[slot] {
            vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR
        } else {
            vk::VK_IMAGE_LAYOUT_UNDEFINED
        };
        {
            let device = self.device();
            check(
                unsafe {
                    // SAFETY: The frame fence is idle after `wait_for_frame`.
                    device.functions.reset_fences.expect("loaded function")(
                        device.handle,
                        1,
                        &raw const self.frame_fence,
                    )
                },
                "vkResetFences",
            )?;
            check(
                unsafe {
                    // SAFETY: The only command buffer is no longer executing.
                    device
                        .functions
                        .reset_command_buffer
                        .expect("loaded function")(self.command_buffer, 0)
                },
                "vkResetCommandBuffer",
            )?;
        }
        let begin = vk::VkCommandBufferBeginInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            flags: vk::VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT as u32,
            ..Default::default()
        };
        check(
            unsafe {
                // SAFETY: The reset primary command buffer may begin recording.
                self.device()
                    .functions
                    .begin_command_buffer
                    .expect("loaded function")(self.command_buffer, &raw const begin)
            },
            "vkBeginCommandBuffer",
        )?;
        self.record_clear(image, view, old_layout, color);
        check(
            unsafe {
                // SAFETY: Recording has a balanced rendering scope.
                self.device()
                    .functions
                    .end_command_buffer
                    .expect("loaded function")(self.command_buffer)
            },
            "vkEndCommandBuffer",
        )?;

        let wait = vk::VkSemaphoreSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_SEMAPHORE_SUBMIT_INFO,
            semaphore: self.image_available,
            stageMask: vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            ..Default::default()
        };
        let command = vk::VkCommandBufferSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO,
            commandBuffer: self.command_buffer,
            ..Default::default()
        };
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
        check(
            unsafe {
                // SAFETY: Submission resources are live and synchronization is externally serialized.
                self.device()
                    .functions
                    .queue_submit2
                    .expect("loaded function")(
                    self.device().queue,
                    1,
                    &raw const submit,
                    self.frame_fence,
                )
            },
            "vkQueueSubmit2",
        )?;
        self.frame_pending = true;
        self.swapchain.initialized[slot] = true;

        let present = vk::VkPresentInfoKHR {
            sType: vk::VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,
            waitSemaphoreCount: 1,
            pWaitSemaphores: &raw const render_finished,
            swapchainCount: 1,
            pSwapchains: &raw const self.swapchain.handle,
            pImageIndices: &raw const image_index,
            ..Default::default()
        };
        let result = unsafe {
            // SAFETY: Queue, swapchain, image index, and wait semaphore are valid.
            self.device()
                .functions
                .queue_present
                .expect("loaded function")(self.device().queue, &raw const present)
        };
        if matches!(
            result,
            vk::VK_SUCCESS | vk::VK_SUBOPTIMAL_KHR | vk::VK_ERROR_OUT_OF_DATE_KHR
        ) {
            self.swapchain.presented[slot] = true;
        }
        if matches!(result, vk::VK_SUBOPTIMAL_KHR | vk::VK_ERROR_OUT_OF_DATE_KHR) {
            self.recreate_after_present = true;
        } else {
            check(result, "vkQueuePresentKHR")?;
        }
        Ok(FrameDisposition::Presented(self.info.generation()))
    }

    fn record_clear(
        &self,
        image: vk::VkImage,
        view: vk::VkImageView,
        old_layout: vk::VkImageLayout,
        color: ClearColor,
    ) {
        let range = color_subresource_range();
        let to_attachment = vk::VkImageMemoryBarrier2 {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
            srcStageMask: if old_layout == vk::VK_IMAGE_LAYOUT_UNDEFINED {
                vk::VK_PIPELINE_STAGE_2_NONE
            } else {
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT
            },
            srcAccessMask: vk::VK_ACCESS_2_NONE,
            dstStageMask: vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            dstAccessMask: vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            oldLayout: old_layout,
            newLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            srcQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
            dstQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
            image,
            subresourceRange: range,
            ..Default::default()
        };
        let dependency = vk::VkDependencyInfo {
            sType: vk::VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
            imageMemoryBarrierCount: 1,
            pImageMemoryBarriers: &raw const to_attachment,
            ..Default::default()
        };
        let components = color.components();
        let attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: view,
            imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: vk::VkClearValue {
                color: vk::VkClearColorValue {
                    float32: components,
                },
            },
            ..Default::default()
        };
        let rendering = vk::VkRenderingInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
            renderArea: vk::VkRect2D {
                offset: vk::VkOffset2D { x: 0, y: 0 },
                extent: self.swapchain.extent,
            },
            layerCount: 1,
            colorAttachmentCount: 1,
            pColorAttachments: &raw const attachment,
            ..Default::default()
        };
        let to_present = vk::VkImageMemoryBarrier2 {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
            srcStageMask: vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            srcAccessMask: vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            dstStageMask: vk::VK_PIPELINE_STAGE_2_NONE,
            dstAccessMask: vk::VK_ACCESS_2_NONE,
            oldLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            newLayout: vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
            srcQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
            dstQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
            image,
            subresourceRange: range,
            ..Default::default()
        };
        let present_dependency = vk::VkDependencyInfo {
            sType: vk::VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
            imageMemoryBarrierCount: 1,
            pImageMemoryBarriers: &raw const to_present,
            ..Default::default()
        };
        let functions = &self.device().functions;
        unsafe {
            // SAFETY: The command buffer is recording and all referenced image resources are live.
            functions.cmd_pipeline_barrier2.expect("loaded function")(
                self.command_buffer,
                &raw const dependency,
            );
            functions.cmd_begin_rendering.expect("loaded function")(
                self.command_buffer,
                &raw const rendering,
            );
            functions.cmd_end_rendering.expect("loaded function")(self.command_buffer);
            functions.cmd_pipeline_barrier2.expect("loaded function")(
                self.command_buffer,
                &raw const present_dependency,
            );
        }
    }

    fn abandon(&mut self) -> Result<FrameDisposition, GraphicsError> {
        let old_semaphore = self.image_available;
        let replacement = create_semaphore(self.device(), "replacement image-available semaphore")?;
        if let Err(error) = self.recreate_swapchain(self.info.extent(), true) {
            unsafe {
                // SAFETY: The replacement semaphore was never submitted or used for acquisition.
                self.device()
                    .functions
                    .destroy_semaphore
                    .expect("loaded function")(
                    self.device().handle, replacement, ptr::null()
                );
            }
            return Err(error);
        }
        self.image_available = replacement;
        unsafe {
            // SAFETY: Acquisition completion was fenced and this semaphore has no queued waits.
            self.device()
                .functions
                .destroy_semaphore
                .expect("loaded function")(
                self.device().handle, old_semaphore, ptr::null()
            );
        }
        Ok(FrameDisposition::Abandoned(self.info.generation()))
    }

    fn wait_for_frame(&mut self) -> Result<(), GraphicsError> {
        if !self.frame_pending {
            return Ok(());
        }
        check(
            unsafe {
                // SAFETY: Fence is live and belongs to this device.
                self.device()
                    .functions
                    .wait_for_fences
                    .expect("loaded function")(
                    self.device().handle,
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
    ) -> Result<(), GraphicsError> {
        let device = self.device();
        check(
            unsafe {
                // SAFETY: Fence is live and was supplied to a completed acquisition.
                device.functions.wait_for_fences.expect("loaded function")(
                    device.handle,
                    1,
                    &raw const fence,
                    vk::VK_TRUE,
                    UINT64_MAX,
                )
            },
            description,
        )?;
        check(
            unsafe {
                // SAFETY: Signaled fence may be reset for its next acquisition.
                device.functions.reset_fences.expect("loaded function")(
                    device.handle,
                    1,
                    &raw const fence,
                )
            },
            description,
        )
    }

    fn destroy_all_retired_swapchains(&mut self) {
        if let Some(device) = self.device.as_ref() {
            for swapchain in self.retired.drain(..) {
                destroy_swapchain(device, swapchain);
            }
        }
    }

    fn finish(&mut self) -> Result<(), GraphicsError> {
        let mut result = self.wait_for_frame();
        if let Some(device) = self.device.as_ref()
            && let Err(error) = check(
                unsafe {
                    // SAFETY: Device is live; idle completion covers GPU and queue work before teardown.
                    device.functions.device_wait_idle.expect("loaded function")(device.handle)
                },
                "vkDeviceWaitIdle",
            )
            && result.is_ok()
        {
            result = Err(error);
        }
        let count = VALIDATION_MESSAGE_COUNT.load(Ordering::Relaxed);
        if count != 0 && result.is_ok() {
            result = Err(error(format!(
                "Vulkan validation reported {count} warning/error message(s)"
            )));
        }
        result
    }

    fn destroy(&mut self) {
        let retired = mem::take(&mut self.retired);
        let Some(device) = self.device.as_ref() else {
            return;
        };
        for swapchain in retired {
            destroy_swapchain(device, swapchain);
        }
        destroy_swapchain(device, mem::take(&mut self.swapchain));
        unsafe {
            // SAFETY: Device idle was requested and each owned child is destroyed once.
            if !self.image_available.is_null() {
                device.functions.destroy_semaphore.expect("loaded function")(
                    device.handle,
                    self.image_available,
                    ptr::null(),
                );
            }
            if !self.acquire_fence.is_null() {
                device.functions.destroy_fence.expect("loaded function")(
                    device.handle,
                    self.acquire_fence,
                    ptr::null(),
                );
            }
            if !self.frame_fence.is_null() {
                device.functions.destroy_fence.expect("loaded function")(
                    device.handle,
                    self.frame_fence,
                    ptr::null(),
                );
            }
            if !self.command_pool.is_null() {
                device
                    .functions
                    .destroy_command_pool
                    .expect("loaded function")(
                    device.handle, self.command_pool, ptr::null()
                );
            }
        }
        drop(self.device.take());
    }
}

mod textured;
pub(crate) use textured::{TexturedFrameToken, TexturedSession};

impl Drop for ClearSurface<'_> {
    fn drop(&mut self) {
        let _ = self.finish();
        self.destroy();
    }
}

impl ClearFrame<'_, '_> {
    pub(crate) fn surface_info(&self) -> SurfaceInfo {
        self.surface.info
    }

    pub(crate) fn clear_and_present(
        mut self,
        color: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let result = self.surface.present(self.image_index, color);
        self.disposed = true;
        result
    }

    pub(crate) fn abandon(mut self) -> Result<FrameDisposition, GraphicsError> {
        let result = self.surface.abandon();
        self.disposed = true;
        result
    }
}

impl Drop for ClearFrame<'_, '_> {
    fn drop(&mut self) {
        if !self.disposed
            && let Err(error) = self.surface.abandon()
        {
            self.surface.deferred_error = Some(error);
        }
    }
}

#[derive(Default)]
struct Swapchain {
    handle: vk::VkSwapchainKHR,
    format: vk::VkFormat,
    extent: vk::VkExtent2D,
    images: Vec<vk::VkImage>,
    views: Vec<vk::VkImageView>,
    render_finished: Vec<vk::VkSemaphore>,
    initialized: Vec<bool>,
    presented: Vec<bool>,
}

struct Entry {
    _library: VulkanLibrary,
    get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    enumerate_instance_version: vk::PFN_vkEnumerateInstanceVersion,
    enumerate_instance_layer_properties: vk::PFN_vkEnumerateInstanceLayerProperties,
    enumerate_instance_extension_properties: vk::PFN_vkEnumerateInstanceExtensionProperties,
    create_instance: vk::PFN_vkCreateInstance,
}

impl Entry {
    fn load() -> Result<Self, GraphicsError> {
        let library = VulkanLibrary::open()?;
        let get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr = unsafe {
            // SAFETY: The Vulkan loader exports this symbol with the generated ABI.
            cast_address(
                library.symbol(c"vkGetInstanceProcAddr"),
                "vkGetInstanceProcAddr",
            )?
        };
        let get = get_instance_proc_addr.expect("loaded function");
        let mut entry = Self {
            _library: library,
            get_instance_proc_addr,
            enumerate_instance_version: None,
            enumerate_instance_layer_properties: None,
            enumerate_instance_extension_properties: None,
            create_instance: None,
        };
        unsafe {
            // SAFETY: Global symbols are requested with a null instance and matching ABI types.
            entry.enumerate_instance_version = load_proc(
                get(ptr::null_mut(), c"vkEnumerateInstanceVersion".as_ptr()),
                "vkEnumerateInstanceVersion",
            )?;
            entry.enumerate_instance_layer_properties = load_proc(
                get(
                    ptr::null_mut(),
                    c"vkEnumerateInstanceLayerProperties".as_ptr(),
                ),
                "vkEnumerateInstanceLayerProperties",
            )?;
            entry.enumerate_instance_extension_properties = load_proc(
                get(
                    ptr::null_mut(),
                    c"vkEnumerateInstanceExtensionProperties".as_ptr(),
                ),
                "vkEnumerateInstanceExtensionProperties",
            )?;
            entry.create_instance = load_proc(
                get(ptr::null_mut(), c"vkCreateInstance".as_ptr()),
                "vkCreateInstance",
            )?;
        }
        let mut version = 0;
        check(
            unsafe {
                // SAFETY: Output pointer is writable.
                entry.enumerate_instance_version.expect("loaded function")(&raw mut version)
            },
            "vkEnumerateInstanceVersion",
        )?;
        if version < API_VERSION_1_4 {
            return Err(error(format!(
                "Vulkan loader exposes {}.{}, but Mulciber requires 1.4",
                version >> 22,
                (version >> 12) & 0x3ff
            )));
        }
        Ok(entry)
    }

    unsafe fn instance_proc<T: Copy>(
        &self,
        instance: vk::VkInstance,
        name: &CStr,
    ) -> Result<T, GraphicsError> {
        // SAFETY: Caller pairs the requested type with the exact Vulkan symbol.
        unsafe {
            load_proc(
                self.get_instance_proc_addr.expect("loaded function")(instance, name.as_ptr()),
                name.to_string_lossy().as_ref(),
            )
        }
    }
}

struct InstanceFns {
    destroy_instance: vk::PFN_vkDestroyInstance,
    create_debug_utils_messenger: vk::PFN_vkCreateDebugUtilsMessengerEXT,
    destroy_debug_utils_messenger: vk::PFN_vkDestroyDebugUtilsMessengerEXT,
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
    unsafe fn load(entry: &Entry, instance: vk::VkInstance) -> Result<Self, GraphicsError> {
        macro_rules! load {
            ($name:literal) => {
                unsafe { entry.instance_proc(instance, $name) }?
            };
        }
        Ok(Self {
            destroy_instance: load!(c"vkDestroyInstance"),
            create_debug_utils_messenger: load!(c"vkCreateDebugUtilsMessengerEXT"),
            destroy_debug_utils_messenger: load!(c"vkDestroyDebugUtilsMessengerEXT"),
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

struct Instance {
    entry: Entry,
    functions: InstanceFns,
    handle: vk::VkInstance,
    debug_messenger: vk::VkDebugUtilsMessengerEXT,
    surface: vk::VkSurfaceKHR,
}

impl Instance {
    fn new(entry: Entry, target: &SurfaceTarget<'_>) -> Result<Self, GraphicsError> {
        require_name(
            &enumerate_instance_layers(&entry)?,
            c"VK_LAYER_KHRONOS_validation",
            "Vulkan validation layer",
        )?;
        let available = enumerate_instance_extensions(&entry)?;
        let required = [
            c"VK_KHR_surface",
            platform::surface_extension(target),
            c"VK_EXT_debug_utils",
        ];
        for name in required {
            require_name(&available, name, "instance extension")?;
        }
        let application = vk::VkApplicationInfo {
            sType: vk::VK_STRUCTURE_TYPE_APPLICATION_INFO,
            pApplicationName: c"Mulciber clear slice".as_ptr(),
            applicationVersion: 0,
            pEngineName: c"Mulciber".as_ptr(),
            engineVersion: 0,
            apiVersion: API_VERSION_1_4,
            ..Default::default()
        };
        let layers = [c"VK_LAYER_KHRONOS_validation".as_ptr()];
        let extensions = required.map(CStr::as_ptr);
        let debug_info = debug_messenger_info();
        let create_info = vk::VkInstanceCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
            pNext: (&raw const debug_info).cast(),
            pApplicationInfo: &raw const application,
            enabledLayerCount: 1,
            ppEnabledLayerNames: layers.as_ptr(),
            enabledExtensionCount: 3,
            ppEnabledExtensionNames: extensions.as_ptr(),
            ..Default::default()
        };
        let mut handle = ptr::null_mut();
        check(
            unsafe {
                // SAFETY: Create-info pointers remain live for the call.
                entry.create_instance.expect("loaded function")(
                    &raw const create_info,
                    ptr::null(),
                    &raw mut handle,
                )
            },
            "vkCreateInstance",
        )?;
        let functions = unsafe {
            // SAFETY: Instance is live and each loaded type matches its symbol.
            InstanceFns::load(&entry, handle)
        }?;
        let mut instance = Self {
            entry,
            functions,
            handle,
            debug_messenger: ptr::null_mut(),
            surface: ptr::null_mut(),
        };
        check(
            unsafe {
                // SAFETY: Callback and create info are valid.
                instance
                    .functions
                    .create_debug_utils_messenger
                    .expect("loaded function")(
                    instance.handle,
                    &raw const debug_info,
                    ptr::null(),
                    &raw mut instance.debug_messenger,
                )
            },
            "vkCreateDebugUtilsMessengerEXT",
        )?;
        let create_surface = unsafe {
            // SAFETY: Symbol name and platform ABI are paired by the adapter.
            instance
                .entry
                .instance_proc(instance.handle, platform::create_surface_name(target))
        }?;
        check(
            unsafe {
                // SAFETY: Native target and Vulkan instance are live.
                platform::create_surface(
                    create_surface,
                    instance.handle,
                    target,
                    &raw mut instance.surface,
                )
            },
            platform::create_surface_name(target)
                .to_string_lossy()
                .as_ref(),
        )?;
        Ok(instance)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: Device children are gone and these owned handles are destroyed once.
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
    sample_count: vk::VkSampleCountFlagBits,
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
    cmd_copy_buffer_to_image2: vk::PFN_vkCmdCopyBufferToImage2,
    cmd_set_viewport: vk::PFN_vkCmdSetViewport,
    cmd_set_scissor: vk::PFN_vkCmdSetScissor,
    cmd_draw_indexed_indirect: vk::PFN_vkCmdDrawIndexedIndirect,
    create_semaphore: vk::PFN_vkCreateSemaphore,
    destroy_semaphore: vk::PFN_vkDestroySemaphore,
    create_fence: vk::PFN_vkCreateFence,
    destroy_fence: vk::PFN_vkDestroyFence,
    wait_for_fences: vk::PFN_vkWaitForFences,
    reset_fences: vk::PFN_vkResetFences,
    queue_submit2: vk::PFN_vkQueueSubmit2,
}

impl DeviceFns {
    unsafe fn load(instance: &Instance, device: vk::VkDevice) -> Result<Self, GraphicsError> {
        let get = instance
            .functions
            .get_device_proc_addr
            .expect("loaded function");
        macro_rules! load {
            ($name:literal) => {{
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
            cmd_copy_buffer_to_image2: load!(c"vkCmdCopyBufferToImage2"),
            cmd_set_viewport: load!(c"vkCmdSetViewport"),
            cmd_set_scissor: load!(c"vkCmdSetScissor"),
            cmd_draw_indexed_indirect: load!(c"vkCmdDrawIndexedIndirect"),
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

struct Device {
    instance: Instance,
    functions: DeviceFns,
    adapter: Adapter,
    handle: vk::VkDevice,
    queue: vk::VkQueue,
}

impl Device {
    fn new(instance: Instance) -> Result<Self, GraphicsError> {
        let adapter = choose_adapter(&instance)?;
        let priority = 1.0;
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
        let info = vk::VkDeviceCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
            pNext: (&raw mut features13).cast(),
            queueCreateInfoCount: 1,
            pQueueCreateInfos: &raw const queue_info,
            enabledExtensionCount: 1,
            ppEnabledExtensionNames: extensions.as_ptr(),
            ..Default::default()
        };
        let mut handle = ptr::null_mut();
        check(
            unsafe {
                // SAFETY: Adapter and create-info chain are valid.
                instance.functions.create_device.expect("loaded function")(
                    adapter.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut handle,
                )
            },
            "vkCreateDevice",
        )?;
        let functions = unsafe {
            // SAFETY: Device is live and symbol/type pairs match.
            DeviceFns::load(&instance, handle)
        }?;
        let mut queue = ptr::null_mut();
        unsafe {
            // SAFETY: Queue zero was requested from the selected family.
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

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: Every device child is destroyed before the owning device.
            self.functions.destroy_device.expect("loaded function")(self.handle, ptr::null());
        }
    }
}

#[allow(clippy::too_many_lines)]
fn choose_adapter(instance: &Instance) -> Result<Adapter, GraphicsError> {
    let enumerate = instance
        .functions
        .enumerate_physical_devices
        .expect("loaded function");
    let mut count = 0;
    check_enumeration(
        unsafe { enumerate(instance.handle, &raw mut count, ptr::null_mut()) },
        "enumerate physical devices",
    )?;
    let mut devices = vec![ptr::null_mut(); count as usize];
    check_enumeration(
        unsafe { enumerate(instance.handle, &raw mut count, devices.as_mut_ptr()) },
        "enumerate physical devices",
    )?;
    devices.truncate(count as usize);
    let mut candidates = Vec::new();
    for handle in devices {
        let mut properties = vk::VkPhysicalDeviceProperties::default();
        unsafe {
            instance
                .functions
                .get_physical_device_properties
                .expect("loaded function")(handle, &raw mut properties);
        }
        if properties.apiVersion < API_VERSION_1_4
            || !device_extensions(instance, handle)?.iter().any(|name| {
                name == vk::VK_KHR_SWAPCHAIN_EXTENSION_NAME
                    .strip_suffix(&[0])
                    .expect("NUL suffix")
            })
        {
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
        unsafe {
            instance
                .functions
                .get_physical_device_features2
                .expect("loaded function")(handle, &raw mut features);
        }
        if features13.dynamicRendering == vk::VK_FALSE
            || features13.synchronization2 == vk::VK_FALSE
        {
            continue;
        }
        let mut family_count = 0;
        unsafe {
            instance
                .functions
                .get_queue_family_properties
                .expect("loaded function")(
                handle, &raw mut family_count, ptr::null_mut()
            );
        }
        let mut families = vec![vk::VkQueueFamilyProperties::default(); family_count as usize];
        unsafe {
            instance
                .functions
                .get_queue_family_properties
                .expect("loaded function")(
                handle, &raw mut family_count, families.as_mut_ptr()
            );
        }
        for (index, family) in families.iter().enumerate() {
            if family.queueCount == 0 || family.queueFlags & vk::VK_QUEUE_GRAPHICS_BIT as u32 == 0 {
                continue;
            }
            let mut supported = vk::VK_FALSE;
            check(
                unsafe {
                    instance
                        .functions
                        .get_surface_support
                        .expect("loaded function")(
                        handle,
                        u32::try_from(index).expect("queue family index"),
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
                        handle,
                        queue_family: u32::try_from(index).expect("queue family index"),
                        sample_count: if properties.limits.framebufferColorSampleCounts
                            & properties.limits.framebufferDepthSampleCounts
                            & vk::VK_SAMPLE_COUNT_4_BIT as u32
                            != 0
                        {
                            vk::VK_SAMPLE_COUNT_4_BIT
                        } else {
                            vk::VK_SAMPLE_COUNT_1_BIT
                        },
                    },
                ));
                break;
            }
        }
    }
    candidates.sort_by_key(|candidate| candidate.0);
    candidates.pop().map(|(_, adapter)| adapter).ok_or_else(|| error("no Vulkan 1.4 graphics/present adapter supports dynamic rendering and synchronization2"))
}

fn populate_swapchain(device: &Device, swapchain: &mut Swapchain) -> Result<(), GraphicsError> {
    swapchain.images = swapchain_images(device, swapchain.handle)?;
    for &image in &swapchain.images {
        let info = vk::VkImageViewCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
            image,
            viewType: vk::VK_IMAGE_VIEW_TYPE_2D,
            format: swapchain.format,
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
            unsafe {
                device.functions.create_image_view.expect("loaded function")(
                    device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut view,
                )
            },
            "vkCreateImageView",
        )?;
        swapchain.views.push(view);
        swapchain
            .render_finished
            .push(create_semaphore(device, "render-finished semaphore")?);
    }
    swapchain.initialized = vec![false; swapchain.images.len()];
    swapchain.presented = vec![false; swapchain.images.len()];
    Ok(())
}

fn destroy_swapchain(device: &Device, swapchain: Swapchain) {
    unsafe {
        // SAFETY: Caller established completion/retirement before releasing this owned set.
        for view in swapchain.views {
            device
                .functions
                .destroy_image_view
                .expect("loaded function")(device.handle, view, ptr::null());
        }
        for semaphore in swapchain.render_finished {
            device.functions.destroy_semaphore.expect("loaded function")(
                device.handle,
                semaphore,
                ptr::null(),
            );
        }
        if !swapchain.handle.is_null() {
            device.functions.destroy_swapchain.expect("loaded function")(
                device.handle,
                swapchain.handle,
                ptr::null(),
            );
        }
    }
}

fn create_semaphore(device: &Device, description: &str) -> Result<vk::VkSemaphore, GraphicsError> {
    let info = vk::VkSemaphoreCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO,
        ..Default::default()
    };
    let mut handle = ptr::null_mut();
    check(
        unsafe {
            device.functions.create_semaphore.expect("loaded function")(
                device.handle,
                &raw const info,
                ptr::null(),
                &raw mut handle,
            )
        },
        description,
    )?;
    Ok(handle)
}

fn create_fence(
    device: &Device,
    signaled: bool,
    description: &str,
) -> Result<vk::VkFence, GraphicsError> {
    let info = vk::VkFenceCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_FENCE_CREATE_INFO,
        flags: if signaled {
            vk::VK_FENCE_CREATE_SIGNALED_BIT as u32
        } else {
            0
        },
        ..Default::default()
    };
    let mut handle = ptr::null_mut();
    check(
        unsafe {
            device.functions.create_fence.expect("loaded function")(
                device.handle,
                &raw const info,
                ptr::null(),
                &raw mut handle,
            )
        },
        description,
    )?;
    Ok(handle)
}

fn surface_formats(device: &Device) -> Result<Vec<vk::VkSurfaceFormatKHR>, GraphicsError> {
    let function = device
        .instance
        .functions
        .get_surface_formats
        .expect("loaded function");
    let mut count = 0;
    check_enumeration(
        unsafe {
            function(
                device.adapter.handle,
                device.instance.surface,
                &raw mut count,
                ptr::null_mut(),
            )
        },
        "enumerate surface formats",
    )?;
    let mut values = vec![vk::VkSurfaceFormatKHR::default(); count as usize];
    check_enumeration(
        unsafe {
            function(
                device.adapter.handle,
                device.instance.surface,
                &raw mut count,
                values.as_mut_ptr(),
            )
        },
        "enumerate surface formats",
    )?;
    values.truncate(count as usize);
    Ok(values)
}

fn require_fifo_present_mode(device: &Device) -> Result<(), GraphicsError> {
    let function = device
        .instance
        .functions
        .get_surface_present_modes
        .expect("loaded function");
    let mut count = 0;
    check_enumeration(
        unsafe {
            function(
                device.adapter.handle,
                device.instance.surface,
                &raw mut count,
                ptr::null_mut(),
            )
        },
        "enumerate present modes",
    )?;
    let mut values = vec![0; count as usize];
    check_enumeration(
        unsafe {
            function(
                device.adapter.handle,
                device.instance.surface,
                &raw mut count,
                values.as_mut_ptr(),
            )
        },
        "enumerate present modes",
    )?;
    if values[..count as usize].contains(&vk::VK_PRESENT_MODE_FIFO_KHR) {
        Ok(())
    } else {
        Err(error("surface does not expose required FIFO presentation"))
    }
}

fn swapchain_images(
    device: &Device,
    swapchain: vk::VkSwapchainKHR,
) -> Result<Vec<vk::VkImage>, GraphicsError> {
    let function = device
        .functions
        .get_swapchain_images
        .expect("loaded function");
    let mut count = 0;
    check_enumeration(
        unsafe { function(device.handle, swapchain, &raw mut count, ptr::null_mut()) },
        "enumerate swapchain images",
    )?;
    let mut values = vec![ptr::null_mut(); count as usize];
    check_enumeration(
        unsafe {
            function(
                device.handle,
                swapchain,
                &raw mut count,
                values.as_mut_ptr(),
            )
        },
        "enumerate swapchain images",
    )?;
    values.truncate(count as usize);
    Ok(values)
}

fn device_extensions(
    instance: &Instance,
    device: vk::VkPhysicalDevice,
) -> Result<Vec<Vec<u8>>, GraphicsError> {
    let function = instance
        .functions
        .enumerate_device_extensions
        .expect("loaded function");
    let mut count = 0;
    check_enumeration(
        unsafe { function(device, ptr::null(), &raw mut count, ptr::null_mut()) },
        "enumerate device extensions",
    )?;
    let mut values = vec![vk::VkExtensionProperties::default(); count as usize];
    check_enumeration(
        unsafe { function(device, ptr::null(), &raw mut count, values.as_mut_ptr()) },
        "enumerate device extensions",
    )?;
    values.truncate(count as usize);
    Ok(values
        .iter()
        .map(|value| fixed_c_string(&value.extensionName))
        .collect())
}

fn enumerate_instance_layers(entry: &Entry) -> Result<Vec<Vec<u8>>, GraphicsError> {
    let function = entry
        .enumerate_instance_layer_properties
        .expect("loaded function");
    let mut count = 0;
    check_enumeration(
        unsafe { function(&raw mut count, ptr::null_mut()) },
        "enumerate instance layers",
    )?;
    let mut values = vec![vk::VkLayerProperties::default(); count as usize];
    check_enumeration(
        unsafe { function(&raw mut count, values.as_mut_ptr()) },
        "enumerate instance layers",
    )?;
    values.truncate(count as usize);
    Ok(values
        .iter()
        .map(|value| fixed_c_string(&value.layerName))
        .collect())
}

fn enumerate_instance_extensions(entry: &Entry) -> Result<Vec<Vec<u8>>, GraphicsError> {
    let function = entry
        .enumerate_instance_extension_properties
        .expect("loaded function");
    let mut count = 0;
    check_enumeration(
        unsafe { function(ptr::null(), &raw mut count, ptr::null_mut()) },
        "enumerate instance extensions",
    )?;
    let mut values = vec![vk::VkExtensionProperties::default(); count as usize];
    check_enumeration(
        unsafe { function(ptr::null(), &raw mut count, values.as_mut_ptr()) },
        "enumerate instance extensions",
    )?;
    values.truncate(count as usize);
    Ok(values
        .iter()
        .map(|value| fixed_c_string(&value.extensionName))
        .collect())
}

fn choose_surface_format(formats: &[vk::VkSurfaceFormatKHR]) -> Option<vk::VkSurfaceFormatKHR> {
    formats.iter().copied().find(|format| {
        format.colorSpace == vk::VK_COLOR_SPACE_SRGB_NONLINEAR_KHR
            && matches!(
                format.format,
                vk::VK_FORMAT_B8G8R8A8_SRGB | vk::VK_FORMAT_R8G8B8A8_SRGB
            )
    })
}

fn choose_extent(
    capabilities: vk::VkSurfaceCapabilitiesKHR,
    requested: SurfaceExtent,
) -> vk::VkExtent2D {
    if capabilities.currentExtent.width != u32::MAX {
        return capabilities.currentExtent;
    }
    vk::VkExtent2D {
        width: requested.width().clamp(
            capabilities.minImageExtent.width,
            capabilities.maxImageExtent.width,
        ),
        height: requested.height().clamp(
            capabilities.minImageExtent.height,
            capabilities.maxImageExtent.height,
        ),
    }
}

fn choose_composite_alpha(
    supported: vk::VkCompositeAlphaFlagsKHR,
) -> Option<vk::VkCompositeAlphaFlagBitsKHR> {
    [
        vk::VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR,
        vk::VK_COMPOSITE_ALPHA_PRE_MULTIPLIED_BIT_KHR,
        vk::VK_COMPOSITE_ALPHA_POST_MULTIPLIED_BIT_KHR,
        vk::VK_COMPOSITE_ALPHA_INHERIT_BIT_KHR,
    ]
    .into_iter()
    .find(|mode| supported & (*mode).cast_unsigned() != 0)
}

const fn color_subresource_range() -> vk::VkImageSubresourceRange {
    vk::VkImageSubresourceRange {
        aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
        baseMipLevel: 0,
        levelCount: 1,
        baseArrayLayer: 0,
        layerCount: 1,
    }
}

fn surface_extent(metrics: WindowMetrics) -> SurfaceExtent {
    SurfaceExtent::new(metrics.extent().width(), metrics.extent().height())
}
fn require_name(names: &[Vec<u8>], name: &CStr, description: &str) -> Result<(), GraphicsError> {
    if names.iter().any(|candidate| candidate == name.to_bytes()) {
        Ok(())
    } else {
        Err(error(format!(
            "required {description} {} is unavailable",
            name.to_string_lossy()
        )))
    }
}
fn fixed_c_string(value: &[c_char]) -> Vec<u8> {
    value
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte.cast_unsigned())
        .collect()
}
fn check(result: vk::VkResult, operation: &str) -> Result<(), GraphicsError> {
    if result == vk::VK_SUCCESS {
        Ok(())
    } else {
        Err(error(format!(
            "{operation} failed with Vulkan result {result}"
        )))
    }
}
fn check_enumeration(result: vk::VkResult, operation: &str) -> Result<(), GraphicsError> {
    if matches!(result, vk::VK_SUCCESS | vk::VK_INCOMPLETE) {
        Ok(())
    } else {
        check(result, operation)
    }
}
fn error(message: impl Into<std::string::String>) -> GraphicsError {
    GraphicsError::new(message)
}
const fn make_api_version(variant: u32, major: u32, minor: u32, patch: u32) -> u32 {
    (variant << 29) | (major << 22) | (minor << 12) | patch
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

struct VulkanLibrary(*mut c_void);

impl VulkanLibrary {
    fn open() -> Result<Self, GraphicsError> {
        #[cfg(target_os = "windows")]
        {
            let name: Vec<u16> = "vulkan-1.dll".encode_utf16().chain(Some(0)).collect();
            let handle = unsafe { LoadLibraryW(name.as_ptr()) };
            if handle.is_null() {
                Err(error(
                    "could not load vulkan-1.dll; install a Vulkan 1.4 driver",
                ))
            } else {
                Ok(Self(handle))
            }
        }
        #[cfg(target_os = "linux")]
        {
            let handle = unsafe { dlopen(c"libvulkan.so.1".as_ptr(), RTLD_NOW) };
            if handle.is_null() {
                Err(error(
                    "could not load libvulkan.so.1; install a Vulkan 1.4 loader and driver",
                ))
            } else {
                Ok(Self(handle))
            }
        }
    }
    unsafe fn symbol(&self, name: &CStr) -> *mut c_void {
        #[cfg(target_os = "windows")]
        {
            unsafe { GetProcAddress(self.0, name.as_ptr()) }
        }
        #[cfg(target_os = "linux")]
        {
            unsafe { dlsym(self.0, name.as_ptr()) }
        }
    }
}

impl Drop for VulkanLibrary {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        unsafe {
            FreeLibrary(self.0);
        }
        #[cfg(target_os = "linux")]
        unsafe {
            dlclose(self.0);
        }
    }
}

unsafe fn cast_address<T: Copy>(address: *mut c_void, name: &str) -> Result<T, GraphicsError> {
    if address.is_null() {
        return Err(error(format!("Vulkan loader did not expose {name}")));
    }
    assert_eq!(mem::size_of::<T>(), mem::size_of_val(&address));
    Ok(unsafe { mem::transmute_copy(&address) })
}
unsafe fn load_proc<T: Copy>(
    function: vk::PFN_vkVoidFunction,
    name: &str,
) -> Result<T, GraphicsError> {
    let Some(function) = function else {
        return Err(error(format!("Vulkan did not expose {name}")));
    };
    assert_eq!(mem::size_of::<T>(), mem::size_of_val(&function));
    Ok(unsafe { mem::transmute_copy(&function) })
}

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}
#[cfg(target_os = "linux")]
const RTLD_NOW: i32 = 2;
#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn dlopen(name: *const c_char, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> i32;
}
