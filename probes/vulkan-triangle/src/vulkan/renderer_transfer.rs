use super::{
    GpuBuffer, GpuImage, ProbeError, Renderer, TEXTURE_HEIGHT, TEXTURE_WIDTH, TexturePath,
    buffer_barrier, check, color_subresource_layers, color_subresource_range,
    command_buffer_submit_info, destroy_gpu_image, find_memory_type, mem, ptr,
    storage_buffer_barrier, vk,
};

impl Renderer {
    pub(super) fn create_buffer(
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

    pub(super) fn write_buffer(
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

    pub(super) fn upload_geometry(
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

    pub(super) fn upload_texture(
        &mut self,
        staging: &GpuBuffer,
        readback: &GpuBuffer,
    ) -> Result<(), ProbeError> {
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
        self.record_texture_upload(staging, readback)?;
        self.submit_upload()?;
        self.wait_for_frame()
    }

    pub(super) fn record_texture_upload(
        &self,
        staging: &GpuBuffer,
        readback: &GpuBuffer,
    ) -> Result<(), ProbeError> {
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
        self.record_texture_readback(readback);
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

    pub(super) fn record_texture_readback(&self, readback: &GpuBuffer) {
        self.image_barrier(
            self.texture.handle,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_TRANSFER_READ_BIT,
            vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            color_subresource_range(),
        );
        let readback_region = vk::VkBufferImageCopy2 {
            sType: vk::VK_STRUCTURE_TYPE_BUFFER_IMAGE_COPY_2,
            imageSubresource: color_subresource_layers(0),
            imageExtent: vk::VkExtent3D {
                width: TEXTURE_WIDTH,
                height: TEXTURE_HEIGHT,
                depth: 1,
            },
            ..Default::default()
        };
        let readback_copy = vk::VkCopyImageToBufferInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_COPY_IMAGE_TO_BUFFER_INFO_2,
            srcImage: self.texture.handle,
            srcImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            dstBuffer: readback.handle,
            regionCount: 1,
            pRegions: &raw const readback_region,
            ..Default::default()
        };
        // SAFETY: The image and buffer are live, correctly laid out, and sized for the selected
        // texture payload.
        unsafe {
            self.device
                .functions
                .cmd_copy_image_to_buffer2
                .expect("loaded function")(
                self.command_buffer, &raw const readback_copy
            );
        }
        self.image_barrier(
            self.texture.handle,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_TRANSFER_READ_BIT,
            vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
            vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
            vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            color_subresource_range(),
        );
        let host_barrier = storage_buffer_barrier(
            readback.handle,
            u64::try_from(self.device.adapter.texture.path.upload_bytes().len())
                .expect("texture readback byte length fits u64"),
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
            vk::VK_PIPELINE_STAGE_2_HOST_BIT,
            vk::VK_ACCESS_2_HOST_READ_BIT,
        );
        self.buffer_dependencies(std::slice::from_ref(&host_barrier));
    }

    pub(super) fn verify_texture_readback(&self, readback: &GpuBuffer) -> Result<(), ProbeError> {
        let texture_path = self.device.adapter.texture.path;
        let expected = texture_path.upload_bytes();
        let mut mapped = ptr::null_mut();
        check(
            // SAFETY: The coherent readback allocation is host-visible and the range is valid.
            unsafe {
                self.device.functions.map_memory.expect("loaded function")(
                    self.device.handle,
                    readback.memory,
                    0,
                    u64::try_from(expected.len()).expect("texture readback byte length fits u64"),
                    0,
                    &raw mut mapped,
                )
            },
            "vkMapMemory for texture readback",
        )?;
        // SAFETY: The queue fence completed, the host barrier made the coherent copy visible, and
        // the mapping covers `expected.len()` bytes.
        let actual = unsafe { std::slice::from_raw_parts(mapped.cast::<u8>(), expected.len()) };
        let mismatch = actual
            .iter()
            .zip(expected)
            .position(|(actual, expected)| actual != expected)
            .map(|offset| (offset, actual[offset], expected[offset]));
        // SAFETY: The mapping belongs to this live allocation and is unmapped exactly once.
        unsafe {
            self.device.functions.unmap_memory.expect("loaded function")(
                self.device.handle,
                readback.memory,
            );
        }
        if let Some((offset, actual, expected)) = mismatch {
            return Err(ProbeError(format!(
                "{} texture round trip differed at byte {offset}: expected 0x{expected:02x}, got 0x{actual:02x}",
                texture_path.diagnostic_name()
            )));
        }
        println!(
            "Texture upload: {} {} bytes round-tripped exactly",
            expected.len(),
            if texture_path == TexturePath::Bc1 {
                "BC1"
            } else {
                "RGBA8"
            }
        );
        Ok(())
    }

    pub(super) fn record_geometry_upload(
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

    pub(super) fn copy_buffer(
        &self,
        source: vk::VkBuffer,
        destination: vk::VkBuffer,
        size: vk::VkDeviceSize,
    ) {
        self.copy_buffer_region(source, destination, 0, 0, size);
    }

    pub(super) fn copy_buffer_region(
        &self,
        source: vk::VkBuffer,
        destination: vk::VkBuffer,
        source_offset: vk::VkDeviceSize,
        destination_offset: vk::VkDeviceSize,
        size: vk::VkDeviceSize,
    ) {
        let region = vk::VkBufferCopy2 {
            sType: vk::VK_STRUCTURE_TYPE_BUFFER_COPY_2,
            srcOffset: source_offset,
            dstOffset: destination_offset,
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

    pub(super) fn submit_upload(&mut self) -> Result<(), ProbeError> {
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
            "vkQueueSubmit2 for startup resource work",
        )?;
        self.frame_pending = true;
        Ok(())
    }

    pub(super) unsafe fn destroy_buffer(&self, buffer: &mut GpuBuffer) {
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

    pub(super) unsafe fn destroy_compute_resources(
        &self,
        storage: &mut GpuBuffer,
        indirect: &mut GpuBuffer,
        readback: &mut GpuBuffer,
    ) {
        // SAFETY: Shutdown established that no submitted compute work remains in flight.
        unsafe {
            if !self.compute_pipeline.is_null() {
                self.device
                    .functions
                    .destroy_pipeline
                    .expect("loaded function")(
                    self.device.handle,
                    self.compute_pipeline,
                    ptr::null(),
                );
            }
            if !self.compute_pipeline_layout.is_null() {
                self.device
                    .functions
                    .destroy_pipeline_layout
                    .expect("loaded function")(
                    self.device.handle,
                    self.compute_pipeline_layout,
                    ptr::null(),
                );
            }
            if !self.compute_descriptor_pool.is_null() {
                self.device
                    .functions
                    .destroy_descriptor_pool
                    .expect("loaded function")(
                    self.device.handle,
                    self.compute_descriptor_pool,
                    ptr::null(),
                );
            }
            if !self.compute_descriptor_set_layout.is_null() {
                self.device
                    .functions
                    .destroy_descriptor_set_layout
                    .expect("loaded function")(
                    self.device.handle,
                    self.compute_descriptor_set_layout,
                    ptr::null(),
                );
            }
            self.destroy_buffer(storage);
            self.destroy_buffer(indirect);
            self.destroy_buffer(readback);
        }
    }

    pub(super) unsafe fn destroy_image(&self, image: &mut GpuImage) {
        // SAFETY: The caller established that this renderer-owned image is no longer in GPU use.
        unsafe { destroy_gpu_image(&self.device, image) };
    }

    pub(super) unsafe fn destroy_compute_sampled_view(&mut self) {
        if !self.compute_sampled_view.is_null() {
            // SAFETY: Shutdown established that the sampled view is no longer in GPU use.
            unsafe {
                self.device
                    .functions
                    .destroy_image_view
                    .expect("loaded function")(
                    self.device.handle,
                    self.compute_sampled_view,
                    ptr::null(),
                );
            }
            self.compute_sampled_view = ptr::null_mut();
        }
    }

    pub(super) unsafe fn destroy_postprocess_resources(&self) {
        // SAFETY: Swapchain teardown destroyed post pipelines before these persistent resources.
        unsafe {
            if !self.post_descriptor_pool.is_null() {
                self.device
                    .functions
                    .destroy_descriptor_pool
                    .expect("loaded function")(
                    self.device.handle,
                    self.post_descriptor_pool,
                    ptr::null(),
                );
            }
            if !self.post_sampler.is_null() {
                self.device
                    .functions
                    .destroy_sampler
                    .expect("loaded function")(
                    self.device.handle, self.post_sampler, ptr::null()
                );
            }
            if !self.post_descriptor_set_layout.is_null() {
                self.device
                    .functions
                    .destroy_descriptor_set_layout
                    .expect("loaded function")(
                    self.device.handle,
                    self.post_descriptor_set_layout,
                    ptr::null(),
                );
            }
        }
    }

    pub(super) unsafe fn destroy_shadow_resources(&mut self) {
        let mut shadow_map = mem::take(&mut self.shadow_map);
        // SAFETY: Shutdown completed all shadow rendering and sampling before teardown.
        unsafe {
            if !self.shadow_pipeline.is_null() {
                self.device
                    .functions
                    .destroy_pipeline
                    .expect("loaded function")(
                    self.device.handle,
                    self.shadow_pipeline,
                    ptr::null(),
                );
            }
            if !self.shadow_pipeline_layout.is_null() {
                self.device
                    .functions
                    .destroy_pipeline_layout
                    .expect("loaded function")(
                    self.device.handle,
                    self.shadow_pipeline_layout,
                    ptr::null(),
                );
            }
            if !self.shadow_sampler.is_null() {
                self.device
                    .functions
                    .destroy_sampler
                    .expect("loaded function")(
                    self.device.handle,
                    self.shadow_sampler,
                    ptr::null(),
                );
            }
            self.destroy_image(&mut shadow_map);
        }
    }

    pub(super) unsafe fn destroy_persistent_render_resources(&mut self) {
        // SAFETY: Shutdown and swapchain teardown completed all descriptor and pipeline use.
        unsafe {
            self.destroy_postprocess_resources();
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
            self.destroy_shadow_resources();
        }
    }
}
