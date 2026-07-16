use super::{
    COMPUTE_IMAGE_HEIGHT, COMPUTE_IMAGE_MIP_LEVELS, COMPUTE_IMAGE_WIDTH, COMPUTE_QUERY_END,
    COMPUTE_QUERY_START, ProbeError, RGBA8_TEXEL_SIZE, Renderer, STORAGE_VALUE_COUNT, check,
    color_mip_range, color_subresource_layers, color_subresource_range, compute_image_byte_len,
    compute_image_mip_extent, compute_image_readback_offset, compute_mip_tail_readback_offset,
    compute_readback_byte_len, expected_compute_mip_tail, expected_compute_texel,
    expected_indirect_command, expected_storage_value, mem, ptr, storage_buffer_barrier,
    storage_buffer_byte_len, vk,
};

impl Renderer {
    pub(super) fn dispatch_compute_and_verify(&mut self) -> Result<(), ProbeError> {
        check(
            // SAFETY: The texture upload completed and left the frame fence signaled.
            unsafe {
                self.device.functions.reset_fences.expect("loaded function")(
                    self.device.handle,
                    1,
                    &raw const self.frame_fence,
                )
            },
            "vkResetFences for compute readback",
        )?;
        check(
            // SAFETY: The previous upload completed, so the command buffer can be reset.
            unsafe {
                self.device
                    .functions
                    .reset_command_buffer
                    .expect("loaded function")(self.command_buffer, 0)
            },
            "vkResetCommandBuffer for compute readback",
        )?;
        self.record_compute_readback()?;
        self.submit_upload()?;
        self.wait_for_frame()?;
        self.collect_compute_gpu_timestamp()?;
        self.verify_compute_readback()
    }

    pub(super) fn record_compute_readback(&self) -> Result<(), ProbeError> {
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
            "vkBeginCommandBuffer for compute readback",
        )?;
        self.reset_gpu_queries(COMPUTE_QUERY_START, 2);
        self.prepare_compute_image_for_storage();
        self.begin_gpu_region(c"compute", [0.20, 0.55, 1.00, 1.00], COMPUTE_QUERY_START);
        // SAFETY: Command buffer is recording and all compute resources are live.
        unsafe {
            self.device
                .functions
                .cmd_bind_pipeline
                .expect("loaded function")(
                self.command_buffer,
                vk::VK_PIPELINE_BIND_POINT_COMPUTE,
                self.compute_pipeline,
            );
            self.device
                .functions
                .cmd_bind_descriptor_sets
                .expect("loaded function")(
                self.command_buffer,
                vk::VK_PIPELINE_BIND_POINT_COMPUTE,
                self.compute_pipeline_layout,
                0,
                1,
                &raw const self.compute_descriptor_set,
                0,
                ptr::null(),
            );
            self.device.functions.cmd_dispatch.expect("loaded function")(
                self.command_buffer,
                1,
                1,
                1,
            );
        }
        self.end_gpu_region(COMPUTE_QUERY_END);
        self.compute_output_barriers();
        self.image_barrier(
            self.compute_image.handle,
            vk::VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
            vk::VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_GENERAL,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_TRANSFER_READ_BIT,
            vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            color_subresource_range(),
        );
        self.generate_compute_image_mips();
        self.copy_buffer_region(
            self.compute_storage.handle,
            self.compute_readback.handle,
            0,
            0,
            u64::try_from(storage_buffer_byte_len()).expect("storage buffer byte length fits u64"),
        );
        self.copy_buffer_region(
            self.compute_indirect.handle,
            self.compute_readback.handle,
            0,
            u64::try_from(storage_buffer_byte_len()).expect("storage buffer byte length fits u64"),
            u64::try_from(mem::size_of::<vk::VkDrawIndexedIndirectCommand>())
                .expect("indirect command byte length fits u64"),
        );
        self.copy_compute_image_to_readback();
        self.image_barrier(
            self.compute_image.handle,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_TRANSFER_READ_BIT,
            vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
            vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
            vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            color_mip_range(0, COMPUTE_IMAGE_MIP_LEVELS),
        );
        self.copy_to_host_barrier();
        check(
            // SAFETY: The command buffer is recording and all commands are complete.
            unsafe {
                self.device
                    .functions
                    .end_command_buffer
                    .expect("loaded function")(self.command_buffer)
            },
            "vkEndCommandBuffer for compute readback",
        )
    }

    pub(super) fn prepare_compute_image_for_storage(&self) {
        self.image_barrier(
            self.compute_image.handle,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_NONE,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
            vk::VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_GENERAL,
            color_subresource_range(),
        );
    }

    pub(super) fn generate_compute_image_mips(&self) {
        for destination_mip in 1..COMPUTE_IMAGE_MIP_LEVELS {
            self.image_barrier(
                self.compute_image.handle,
                vk::VK_PIPELINE_STAGE_2_NONE,
                vk::VK_ACCESS_2_NONE,
                vk::VK_IMAGE_LAYOUT_UNDEFINED,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
                vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                color_mip_range(destination_mip, 1),
            );
            self.blit_compute_image_mip(destination_mip - 1, destination_mip);
            self.image_barrier(
                self.compute_image.handle,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
                vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_ACCESS_2_TRANSFER_READ_BIT,
                vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                color_mip_range(destination_mip, 1),
            );
        }
    }

    pub(super) fn blit_compute_image_mip(&self, source_mip: u32, destination_mip: u32) {
        let (source_width, source_height) = compute_image_mip_extent(source_mip);
        let (destination_width, destination_height) = compute_image_mip_extent(destination_mip);
        let region = vk::VkImageBlit2 {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_BLIT_2,
            srcSubresource: color_subresource_layers(source_mip),
            srcOffsets: [
                vk::VkOffset3D::default(),
                vk::VkOffset3D {
                    x: i32::try_from(source_width).expect("source mip width fits i32"),
                    y: i32::try_from(source_height).expect("source mip height fits i32"),
                    z: 1,
                },
            ],
            dstSubresource: color_subresource_layers(destination_mip),
            dstOffsets: [
                vk::VkOffset3D::default(),
                vk::VkOffset3D {
                    x: i32::try_from(destination_width).expect("destination mip width fits i32"),
                    y: i32::try_from(destination_height).expect("destination mip height fits i32"),
                    z: 1,
                },
            ],
            ..Default::default()
        };
        let blit = vk::VkBlitImageInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_BLIT_IMAGE_INFO_2,
            srcImage: self.compute_image.handle,
            srcImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            dstImage: self.compute_image.handle,
            dstImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            regionCount: 1,
            pRegions: &raw const region,
            filter: vk::VK_FILTER_NEAREST,
            ..Default::default()
        };
        // SAFETY: Source/destination mip ranges are distinct, live, and correctly laid out.
        unsafe {
            self.device
                .functions
                .cmd_blit_image2
                .expect("loaded function")(self.command_buffer, &raw const blit);
        }
    }

    pub(super) fn copy_compute_image_to_readback(&self) {
        let regions = [
            vk::VkBufferImageCopy2 {
                sType: vk::VK_STRUCTURE_TYPE_BUFFER_IMAGE_COPY_2,
                bufferOffset: u64::try_from(compute_image_readback_offset())
                    .expect("compute image readback offset fits u64"),
                imageSubresource: color_subresource_layers(0),
                imageExtent: vk::VkExtent3D {
                    width: COMPUTE_IMAGE_WIDTH,
                    height: COMPUTE_IMAGE_HEIGHT,
                    depth: 1,
                },
                ..Default::default()
            },
            vk::VkBufferImageCopy2 {
                sType: vk::VK_STRUCTURE_TYPE_BUFFER_IMAGE_COPY_2,
                bufferOffset: u64::try_from(compute_mip_tail_readback_offset())
                    .expect("compute mip-tail readback offset fits u64"),
                imageSubresource: color_subresource_layers(COMPUTE_IMAGE_MIP_LEVELS - 1),
                imageExtent: vk::VkExtent3D {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
                ..Default::default()
            },
        ];
        let copy = vk::VkCopyImageToBufferInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_COPY_IMAGE_TO_BUFFER_INFO_2,
            srcImage: self.compute_image.handle,
            srcImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            dstBuffer: self.compute_readback.handle,
            regionCount: u32::try_from(regions.len()).expect("image copy region count fits u32"),
            pRegions: regions.as_ptr(),
            ..Default::default()
        };
        // SAFETY: The image and buffer are live, correctly laid out, and the copy range fits.
        unsafe {
            self.device
                .functions
                .cmd_copy_image_to_buffer2
                .expect("loaded function")(self.command_buffer, &raw const copy);
        }
    }

    pub(super) fn compute_output_barriers(&self) {
        let barriers = [
            storage_buffer_barrier(
                self.compute_storage.handle,
                u64::try_from(storage_buffer_byte_len())
                    .expect("storage buffer byte length fits u64"),
                vk::VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
                vk::VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                vk::VK_ACCESS_2_TRANSFER_READ_BIT,
            ),
            storage_buffer_barrier(
                self.compute_indirect.handle,
                u64::try_from(mem::size_of::<vk::VkDrawIndexedIndirectCommand>())
                    .expect("indirect command byte length fits u64"),
                vk::VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
                vk::VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT,
                vk::VK_PIPELINE_STAGE_2_COPY_BIT | vk::VK_PIPELINE_STAGE_2_DRAW_INDIRECT_BIT,
                vk::VK_ACCESS_2_TRANSFER_READ_BIT | vk::VK_ACCESS_2_INDIRECT_COMMAND_READ_BIT,
            ),
        ];
        self.buffer_dependencies(&barriers);
    }

    pub(super) fn copy_to_host_barrier(&self) {
        let barrier = storage_buffer_barrier(
            self.compute_readback.handle,
            u64::try_from(compute_readback_byte_len())
                .expect("compute readback byte length fits u64"),
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
            vk::VK_PIPELINE_STAGE_2_HOST_BIT,
            vk::VK_ACCESS_2_HOST_READ_BIT,
        );
        self.buffer_dependencies(std::slice::from_ref(&barrier));
    }

    pub(super) fn buffer_dependencies(&self, barriers: &[vk::VkBufferMemoryBarrier2]) {
        let dependency = vk::VkDependencyInfo {
            sType: vk::VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
            bufferMemoryBarrierCount: u32::try_from(barriers.len())
                .expect("buffer barrier count fits u32"),
            pBufferMemoryBarriers: barriers.as_ptr(),
            ..Default::default()
        };
        // SAFETY: The command buffer is recording and the barrier references a live buffer.
        unsafe {
            self.device
                .functions
                .cmd_pipeline_barrier2
                .expect("loaded function")(self.command_buffer, &raw const dependency);
        }
    }

    pub(super) fn verify_compute_readback(&self) -> Result<(), ProbeError> {
        let byte_len = compute_readback_byte_len();
        let mut mapped = ptr::null_mut();
        check(
            // SAFETY: The coherent readback allocation is host-visible and the range is valid.
            unsafe {
                self.device.functions.map_memory.expect("loaded function")(
                    self.device.handle,
                    self.compute_readback.memory,
                    0,
                    u64::try_from(byte_len).expect("readback byte length fits u64"),
                    0,
                    &raw mut mapped,
                )
            },
            "vkMapMemory for compute readback",
        )?;
        // SAFETY: The completed copy populated `STORAGE_VALUE_COUNT` aligned u32 values.
        let values = unsafe {
            std::slice::from_raw_parts(mapped.cast::<u32>(), STORAGE_VALUE_COUNT).to_vec()
        };
        // SAFETY: The indirect command immediately follows the storage values in the completed
        // readback copy. `read_unaligned` avoids depending on the mapped base alignment here.
        let command = unsafe {
            mapped
                .cast::<u8>()
                .add(storage_buffer_byte_len())
                .cast::<vk::VkDrawIndexedIndirectCommand>()
                .read_unaligned()
        };
        // SAFETY: The tightly packed image copy immediately follows the indirect command and is
        // fully contained in the mapped readback range.
        let texels = unsafe {
            std::slice::from_raw_parts(
                mapped.cast::<u8>().add(compute_image_readback_offset()),
                compute_image_byte_len(),
            )
            .to_vec()
        };
        // SAFETY: The final tightly packed 1x1 mip copy occupies the last four mapped bytes.
        let mip_tail = unsafe {
            std::slice::from_raw_parts(
                mapped.cast::<u8>().add(compute_mip_tail_readback_offset()),
                RGBA8_TEXEL_SIZE,
            )
            .to_vec()
        };
        // SAFETY: The mapping belongs to this live allocation and is unmapped exactly once.
        unsafe {
            self.device.functions.unmap_memory.expect("loaded function")(
                self.device.handle,
                self.compute_readback.memory,
            );
        }
        for (index, &actual) in values.iter().enumerate() {
            let expected = expected_storage_value(index);
            if actual != expected {
                return Err(ProbeError(format!(
                    "compute readback mismatch at index {index}: expected {expected:#010x}, got {actual:#010x}"
                )));
            }
        }
        let expected = expected_indirect_command();
        if command.indexCount != expected.indexCount
            || command.instanceCount != expected.instanceCount
            || command.firstIndex != expected.firstIndex
            || command.vertexOffset != expected.vertexOffset
            || command.firstInstance != expected.firstInstance
        {
            return Err(ProbeError(format!(
                "indexed-indirect readback mismatch: got indexCount={}, instanceCount={}, firstIndex={}, vertexOffset={}, firstInstance={}",
                command.indexCount,
                command.instanceCount,
                command.firstIndex,
                command.vertexOffset,
                command.firstInstance,
            )));
        }
        for (index, actual) in texels.chunks_exact(RGBA8_TEXEL_SIZE).enumerate() {
            let expected = expected_compute_texel(index);
            if actual != expected {
                return Err(ProbeError(format!(
                    "compute image readback mismatch at texel {index}: expected {expected:?}, got {actual:?}"
                )));
            }
        }
        let expected_tail = expected_compute_mip_tail();
        if mip_tail != expected_tail {
            return Err(ProbeError(format!(
                "compute image 1x1 mip-tail mismatch: expected {expected_tail:?}, got {mip_tail:?}"
            )));
        }
        Ok(())
    }
}
