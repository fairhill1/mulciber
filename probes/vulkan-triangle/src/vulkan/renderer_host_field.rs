//! Agreement between a WGSL height field on the GPU and the host evaluator generated from it.
//!
//! `src/field.wgsl` is the only definition of the field. The compute entry point evaluates it for
//! every sample direction on the device; the Rust below evaluates the same two functions through
//! the evaluator `mulciber-shader` generates from that same file at build time, and the probe
//! reports the largest disagreement in metres.

use super::{
    FIELD_SAMPLE_COUNT, FIELD_TOLERANCE_METRES, FIELD_WORKGROUP_SIZE, ProbeError, Renderer, check,
    field_buffer_byte_len, ptr, shader_stage, storage_buffer_barrier, vk,
};

/// The host half of the field, generated from `src/field.wgsl` by `mulciber-shader`.
mod generated {
    include!(concat!(env!("OUT_DIR"), "/field_host.rs"));
}

impl Renderer {
    pub(super) fn create_host_field_resources(&mut self) -> Result<(), ProbeError> {
        self.field_storage = self.create_buffer(
            field_buffer_byte_len(),
            (vk::VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | vk::VK_BUFFER_USAGE_TRANSFER_SRC_BIT) as u32,
            vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            "host-field heights",
        )?;
        self.field_readback = self.create_buffer(
            field_buffer_byte_len(),
            vk::VK_BUFFER_USAGE_TRANSFER_DST_BIT as u32,
            (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
                as u32,
            "host-field readback",
        )?;
        self.create_host_field_descriptors()?;
        self.create_host_field_pipeline()?;
        self.dispatch_host_field_and_verify()
    }

    fn create_host_field_descriptors(&mut self) -> Result<(), ProbeError> {
        let binding = vk::VkDescriptorSetLayoutBinding {
            binding: 0,
            descriptorType: vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
            descriptorCount: 1,
            stageFlags: vk::VK_SHADER_STAGE_COMPUTE_BIT as u32,
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
                    &raw mut self.field_descriptor_set_layout,
                )
            },
            "vkCreateDescriptorSetLayout for the host field",
        )?;
        let pool_size = vk::VkDescriptorPoolSize {
            type_: vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
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
                    &raw mut self.field_descriptor_pool,
                )
            },
            "vkCreateDescriptorPool for the host field",
        )?;
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: self.field_descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const self.field_descriptor_set_layout,
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
                    &raw mut self.field_descriptor_set,
                )
            },
            "vkAllocateDescriptorSets for the host field",
        )?;
        let buffer = vk::VkDescriptorBufferInfo {
            buffer: self.field_storage.handle,
            offset: 0,
            range: u64::try_from(field_buffer_byte_len()).expect("host-field byte length fits u64"),
        };
        let write = vk::VkWriteDescriptorSet {
            sType: vk::VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
            dstSet: self.field_descriptor_set,
            dstBinding: 0,
            descriptorCount: 1,
            descriptorType: vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
            pBufferInfo: &raw const buffer,
            ..Default::default()
        };
        // SAFETY: The descriptor set and referenced storage buffer are live.
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

    fn create_host_field_pipeline(&mut self) -> Result<(), ProbeError> {
        let layout_info = vk::VkPipelineLayoutCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
            setLayoutCount: 1,
            pSetLayouts: &raw const self.field_descriptor_set_layout,
            ..Default::default()
        };
        check(
            // SAFETY: Device/create info are valid and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_pipeline_layout
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const layout_info,
                    ptr::null(),
                    &raw mut self.field_pipeline_layout,
                )
            },
            "vkCreatePipelineLayout for the host field",
        )?;
        let module =
            self.create_shader_module(include_bytes!(concat!(env!("OUT_DIR"), "/field.comp.spv")))?;
        let mut stage = shader_stage(vk::VK_SHADER_STAGE_COMPUTE_BIT, module);
        stage.pName = c"field_probe".as_ptr();
        let info = vk::VkComputePipelineCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
            stage,
            layout: self.field_pipeline_layout,
            basePipelineIndex: -1,
            ..Default::default()
        };
        // SAFETY: Pipeline state and shader module are live; output storage is writable.
        let result = unsafe {
            self.device
                .functions
                .create_compute_pipelines
                .expect("loaded function")(
                self.device.handle,
                self.pipeline_cache.handle,
                1,
                &raw const info,
                ptr::null(),
                &raw mut self.field_pipeline,
            )
        };
        // SAFETY: Pipeline creation has finished reading the shader module.
        unsafe {
            self.device
                .functions
                .destroy_shader_module
                .expect("loaded function")(self.device.handle, module, ptr::null());
        }
        check(result, "vkCreateComputePipelines for the host field")
    }

    fn dispatch_host_field_and_verify(&mut self) -> Result<(), ProbeError> {
        check(
            // SAFETY: The previous startup dispatch completed and left the frame fence signaled.
            unsafe {
                self.device.functions.reset_fences.expect("loaded function")(
                    self.device.handle,
                    1,
                    &raw const self.frame_fence,
                )
            },
            "vkResetFences for the host field",
        )?;
        check(
            // SAFETY: The previous submission completed, so the command buffer can be reset.
            unsafe {
                self.device
                    .functions
                    .reset_command_buffer
                    .expect("loaded function")(self.command_buffer, 0)
            },
            "vkResetCommandBuffer for the host field",
        )?;
        self.record_host_field_dispatch()?;
        self.submit_upload()?;
        self.wait_for_frame()?;
        self.verify_host_field_readback()
    }

    fn record_host_field_dispatch(&self) -> Result<(), ProbeError> {
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
            "vkBeginCommandBuffer for the host field",
        )?;
        // SAFETY: Command buffer is recording and all host-field resources are live.
        unsafe {
            self.device
                .functions
                .cmd_bind_pipeline
                .expect("loaded function")(
                self.command_buffer,
                vk::VK_PIPELINE_BIND_POINT_COMPUTE,
                self.field_pipeline,
            );
            self.device
                .functions
                .cmd_bind_descriptor_sets
                .expect("loaded function")(
                self.command_buffer,
                vk::VK_PIPELINE_BIND_POINT_COMPUTE,
                self.field_pipeline_layout,
                0,
                1,
                &raw const self.field_descriptor_set,
                0,
                ptr::null(),
            );
            self.device.functions.cmd_dispatch.expect("loaded function")(
                self.command_buffer,
                u32::try_from(FIELD_SAMPLE_COUNT.div_ceil(FIELD_WORKGROUP_SIZE))
                    .expect("host-field workgroup count fits u32"),
                1,
                1,
            );
        }
        let barrier = storage_buffer_barrier(
            self.field_storage.handle,
            u64::try_from(field_buffer_byte_len()).expect("host-field byte length fits u64"),
            vk::VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
            vk::VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_TRANSFER_READ_BIT,
        );
        self.buffer_dependencies(std::slice::from_ref(&barrier));
        self.copy_buffer_region(
            self.field_storage.handle,
            self.field_readback.handle,
            0,
            0,
            u64::try_from(field_buffer_byte_len()).expect("host-field byte length fits u64"),
        );
        let barrier = storage_buffer_barrier(
            self.field_readback.handle,
            u64::try_from(field_buffer_byte_len()).expect("host-field byte length fits u64"),
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
            vk::VK_PIPELINE_STAGE_2_HOST_BIT,
            vk::VK_ACCESS_2_HOST_READ_BIT,
        );
        self.buffer_dependencies(std::slice::from_ref(&barrier));
        check(
            // SAFETY: The command buffer is recording and every recorded command is complete.
            unsafe {
                self.device
                    .functions
                    .end_command_buffer
                    .expect("loaded function")(self.command_buffer)
            },
            "vkEndCommandBuffer for the host field",
        )
    }

    #[allow(clippy::cast_precision_loss)]
    fn verify_host_field_readback(&self) -> Result<(), ProbeError> {
        let mut mapped = ptr::null_mut();
        check(
            // SAFETY: The coherent readback allocation is host-visible and the range is valid.
            unsafe {
                self.device.functions.map_memory.expect("loaded function")(
                    self.device.handle,
                    self.field_readback.memory,
                    0,
                    u64::try_from(field_buffer_byte_len())
                        .expect("host-field byte length fits u64"),
                    0,
                    &raw mut mapped,
                )
            },
            "vkMapMemory for the host field",
        )?;
        // SAFETY: The completed copy populated `FIELD_SAMPLE_COUNT` aligned f32 heights.
        let device_heights = unsafe {
            std::slice::from_raw_parts(mapped.cast::<f32>(), FIELD_SAMPLE_COUNT).to_vec()
        };
        // SAFETY: The mapping belongs to this live allocation and is unmapped exactly once.
        unsafe {
            self.device.functions.unmap_memory.expect("loaded function")(
                self.device.handle,
                self.field_readback.memory,
            );
        }

        let mut worst = 0.0_f32;
        let mut worst_index = 0;
        let mut lowest = f32::INFINITY;
        let mut highest = f32::NEG_INFINITY;
        for (index, &device_height) in device_heights.iter().enumerate() {
            let sample = u32::try_from(index).expect("host-field sample index fits u32");
            let host_height = generated::surface_height(generated::sample_direction(sample));
            let difference = (device_height - host_height).abs();
            if difference > worst {
                worst = difference;
                worst_index = index;
            }
            lowest = lowest.min(device_height);
            highest = highest.max(device_height);
        }
        if worst > FIELD_TOLERANCE_METRES {
            return Err(ProbeError(format!(
                "host field disagreed with the GPU by {worst} m at sample {worst_index}: device \
                 {} m, host {} m",
                device_heights[worst_index],
                generated::surface_height(generated::sample_direction(
                    u32::try_from(worst_index).expect("host-field sample index fits u32")
                )),
            )));
        }
        println!(
            "Host field: {FIELD_SAMPLE_COUNT} directions evaluated on both sides of one WGSL \
             source; worst GPU-to-host difference {worst:.6} m at sample {worst_index} across a \
             {:.1} m surface range (tolerance {FIELD_TOLERANCE_METRES:.3} m)",
            highest - lowest
        );
        Ok(())
    }

    pub(super) unsafe fn destroy_host_field_resources(
        &self,
        storage: &mut super::GpuBuffer,
        readback: &mut super::GpuBuffer,
    ) {
        // SAFETY: Shutdown established that no submitted host-field work remains in flight.
        unsafe {
            if !self.field_pipeline.is_null() {
                self.device
                    .functions
                    .destroy_pipeline
                    .expect("loaded function")(
                    self.device.handle,
                    self.field_pipeline,
                    ptr::null(),
                );
            }
            if !self.field_pipeline_layout.is_null() {
                self.device
                    .functions
                    .destroy_pipeline_layout
                    .expect("loaded function")(
                    self.device.handle,
                    self.field_pipeline_layout,
                    ptr::null(),
                );
            }
            if !self.field_descriptor_pool.is_null() {
                self.device
                    .functions
                    .destroy_descriptor_pool
                    .expect("loaded function")(
                    self.device.handle,
                    self.field_descriptor_pool,
                    ptr::null(),
                );
            }
            if !self.field_descriptor_set_layout.is_null() {
                self.device
                    .functions
                    .destroy_descriptor_set_layout
                    .expect("loaded function")(
                    self.device.handle,
                    self.field_descriptor_set_layout,
                    ptr::null(),
                );
            }
            self.destroy_buffer(storage);
            self.destroy_buffer(readback);
        }
    }
}
