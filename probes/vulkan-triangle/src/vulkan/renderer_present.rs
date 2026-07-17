use super::{
    OFFSCREEN_FORMAT, ProbeError, Renderer, SurfaceExtent, check, check_enumeration,
    choose_composite_alpha, choose_extent, choose_surface_format, color_subresource_range,
    depth_subresource_range, next_surface_info, ptr, vk,
};

impl Renderer {
    pub(super) fn recreate_swapchain(&mut self, width: u32, height: u32) -> Result<(), ProbeError> {
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
        let reuse_pipeline = !self.pipeline.is_null()
            && !self.post_pipeline.is_null()
            && self.format == format.format;
        self.retire_current_swapchain(!reuse_pipeline);
        self.swapchain = swapchain;
        self.format = format.format;
        self.extent = extent;
        self.surface_info = Some(next_surface_info(
            self.surface_info,
            SurfaceExtent::new(extent.width, extent.height),
        )?);
        self.images = self.swapchain_images()?;
        self.create_present_resources()?;
        self.presented = vec![false; self.images.len()];
        self.views = self.create_image_views()?;
        self.create_offscreen_attachment()?;
        self.create_msaa_color_attachment()?;
        self.create_depth_attachment()?;
        self.update_postprocess_descriptor();
        if !reuse_pipeline {
            self.create_pipeline()?;
        }
        self.collect_retired_swapchains()?;
        self.recreate_after_present = false;
        Ok(())
    }

    pub(super) fn create_present_resources(&mut self) -> Result<(), ProbeError> {
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

    pub(super) fn surface_formats(&self) -> Result<Vec<vk::VkSurfaceFormatKHR>, ProbeError> {
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

    pub(super) fn require_fifo_present_mode(&self) -> Result<(), ProbeError> {
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

    pub(super) fn swapchain_images(&self) -> Result<Vec<vk::VkImage>, ProbeError> {
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

    pub(super) fn create_image_views(&self) -> Result<Vec<vk::VkImageView>, ProbeError> {
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
    pub(super) fn create_offscreen_attachment(&mut self) -> Result<(), ProbeError> {
        let info = vk::VkImageCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
            imageType: vk::VK_IMAGE_TYPE_2D,
            format: OFFSCREEN_FORMAT,
            extent: vk::VkExtent3D {
                width: self.extent.width,
                height: self.extent.height,
                depth: 1,
            },
            mipLevels: 1,
            arrayLayers: 1,
            samples: vk::VK_SAMPLE_COUNT_1_BIT,
            tiling: vk::VK_IMAGE_TILING_OPTIMAL,
            usage: (vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | vk::VK_IMAGE_USAGE_SAMPLED_BIT)
                as u32,
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
                    &raw mut self.offscreen.handle,
                )
            },
            "vkCreateImage for offscreen scene color",
        )?;
        let mut requirements = vk::VkMemoryRequirements::default();
        // SAFETY: The offscreen image is live and requirements storage is writable.
        unsafe {
            self.device
                .functions
                .get_image_memory_requirements
                .expect("loaded function")(
                self.device.handle,
                self.offscreen.handle,
                &raw mut requirements,
            );
        }
        let memory_type = self
            .find_memory_type(
                requirements.memoryTypeBits,
                vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            )
            .ok_or_else(|| {
                ProbeError("adapter exposes no device-local offscreen color memory type".into())
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
                    &raw mut self.offscreen.memory,
                )
            },
            "vkAllocateMemory for offscreen scene color",
        )?;
        check(
            // SAFETY: Image and allocation are compatible at offset zero.
            unsafe {
                self.device
                    .functions
                    .bind_image_memory
                    .expect("loaded function")(
                    self.device.handle,
                    self.offscreen.handle,
                    self.offscreen.memory,
                    0,
                )
            },
            "vkBindImageMemory for offscreen scene color",
        )?;
        let view_info = vk::VkImageViewCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
            image: self.offscreen.handle,
            viewType: vk::VK_IMAGE_VIEW_TYPE_2D,
            format: OFFSCREEN_FORMAT,
            components: vk::VkComponentMapping {
                r: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                g: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                b: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                a: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
            },
            subresourceRange: color_subresource_range(),
            ..Default::default()
        };
        check(
            // SAFETY: Image/create info are valid and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_image_view
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const view_info,
                    ptr::null(),
                    &raw mut self.offscreen.view,
                )
            },
            "vkCreateImageView for offscreen scene color",
        )
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn create_msaa_color_attachment(&mut self) -> Result<(), ProbeError> {
        if self.device.adapter.sample_count == vk::VK_SAMPLE_COUNT_1_BIT {
            return Ok(());
        }
        let info = vk::VkImageCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
            imageType: vk::VK_IMAGE_TYPE_2D,
            format: OFFSCREEN_FORMAT,
            extent: vk::VkExtent3D {
                width: self.extent.width,
                height: self.extent.height,
                depth: 1,
            },
            mipLevels: 1,
            arrayLayers: 1,
            samples: self.device.adapter.sample_count,
            tiling: vk::VK_IMAGE_TILING_OPTIMAL,
            usage: (vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT
                | vk::VK_IMAGE_USAGE_TRANSIENT_ATTACHMENT_BIT) as u32,
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
                    &raw mut self.msaa_color.handle,
                )
            },
            "vkCreateImage for multisampled color attachment",
        )?;
        let mut requirements = vk::VkMemoryRequirements::default();
        // SAFETY: The multisampled image is live and requirements storage is writable.
        unsafe {
            self.device
                .functions
                .get_image_memory_requirements
                .expect("loaded function")(
                self.device.handle,
                self.msaa_color.handle,
                &raw mut requirements,
            );
        }
        let memory_type = self
            .find_memory_type(
                requirements.memoryTypeBits,
                vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            )
            .ok_or_else(|| {
                ProbeError("adapter exposes no device-local multisampled color memory type".into())
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
                    &raw mut self.msaa_color.memory,
                )
            },
            "vkAllocateMemory for multisampled color attachment",
        )?;
        check(
            // SAFETY: Image and allocation are compatible at offset zero.
            unsafe {
                self.device
                    .functions
                    .bind_image_memory
                    .expect("loaded function")(
                    self.device.handle,
                    self.msaa_color.handle,
                    self.msaa_color.memory,
                    0,
                )
            },
            "vkBindImageMemory for multisampled color attachment",
        )?;
        let view_info = vk::VkImageViewCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
            image: self.msaa_color.handle,
            viewType: vk::VK_IMAGE_VIEW_TYPE_2D,
            format: OFFSCREEN_FORMAT,
            components: vk::VkComponentMapping {
                r: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                g: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                b: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                a: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
            },
            subresourceRange: color_subresource_range(),
            ..Default::default()
        };
        check(
            // SAFETY: Image/create info are valid and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_image_view
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const view_info,
                    ptr::null(),
                    &raw mut self.msaa_color.view,
                )
            },
            "vkCreateImageView for multisampled color attachment",
        )
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn create_depth_attachment(&mut self) -> Result<(), ProbeError> {
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
            samples: self.device.adapter.sample_count,
            tiling: vk::VK_IMAGE_TILING_OPTIMAL,
            usage: (vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT
                | vk::VK_IMAGE_USAGE_TRANSIENT_ATTACHMENT_BIT) as u32,
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
}
