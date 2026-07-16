use super::{GPU_QUERY_COUNT, ProbeError, Renderer, check, ptr, vk};

impl Renderer {
    pub(super) fn create_frame_resources(&mut self) -> Result<(), ProbeError> {
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

    pub(super) fn create_gpu_instrumentation(&mut self) -> Result<(), ProbeError> {
        if self.device.adapter.timestamp_valid_bits == 0 {
            println!("GPU instrumentation: debug labels enabled; timestamp queries disabled");
            return Ok(());
        }
        let info = vk::VkQueryPoolCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO,
            queryType: vk::VK_QUERY_TYPE_TIMESTAMP,
            queryCount: GPU_QUERY_COUNT,
            ..Default::default()
        };
        // SAFETY: The device and create info are live and the output handle is writable.
        check(
            unsafe {
                self.device
                    .functions
                    .create_query_pool
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut self.query_pool,
                )
            },
            "vkCreateQueryPool for GPU timestamps",
        )?;
        println!("GPU instrumentation: timestamp queries and debug labels enabled");
        Ok(())
    }
}
