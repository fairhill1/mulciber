use super::{
    COMPUTE_QUERY_START, CStr, Instant, ProbeError, Renderer, SHADOW_QUERY_START, check, mem, ptr,
    shader_stage, timestamp_tick_delta, vk,
};

impl Renderer {
    pub(super) fn create_compute_pipeline(&mut self) -> Result<(), ProbeError> {
        let layout_info = vk::VkPipelineLayoutCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
            setLayoutCount: 1,
            pSetLayouts: &raw const self.compute_descriptor_set_layout,
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
                    &raw mut self.compute_pipeline_layout,
                )
            },
            "vkCreatePipelineLayout for compute storage",
        )?;
        let shader = self.create_shader_module(include_bytes!("../storage.comp.spv"))?;
        let mut feedback = vk::VkPipelineCreationFeedback::default();
        let feedback_info = vk::VkPipelineCreationFeedbackCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_CREATION_FEEDBACK_CREATE_INFO,
            pPipelineCreationFeedback: &raw mut feedback,
            ..Default::default()
        };
        let info = vk::VkComputePipelineCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
            pNext: (&raw const feedback_info).cast(),
            flags: self.pipeline_create_flags(),
            stage: shader_stage(vk::VK_SHADER_STAGE_COMPUTE_BIT, shader),
            layout: self.compute_pipeline_layout,
            basePipelineIndex: -1,
            ..Default::default()
        };
        let started = Instant::now();
        let create_pipelines = self
            .device
            .functions
            .create_compute_pipelines
            .expect("loaded function");
        // SAFETY: Pipeline state and shader module are live; output storage is writable.
        let result = unsafe {
            create_pipelines(
                self.device.handle,
                self.pipeline_cache.handle,
                1,
                &raw const info,
                ptr::null(),
                &raw mut self.compute_pipeline,
            )
        };
        let elapsed = started.elapsed();
        // SAFETY: Pipeline creation has finished reading the shader module.
        unsafe {
            self.device
                .functions
                .destroy_shader_module
                .expect("loaded function")(self.device.handle, shader, ptr::null());
        }
        self.check_pipeline_feedback("compute", result, feedback, elapsed)
    }

    pub(super) fn reset_gpu_queries(&self, first_query: u32, query_count: u32) {
        if self.query_pool.is_null() {
            return;
        }
        // SAFETY: The command buffer is recording and the query range is no longer in flight.
        unsafe {
            self.device
                .functions
                .cmd_reset_query_pool
                .expect("loaded function")(
                self.command_buffer,
                self.query_pool,
                first_query,
                query_count,
            );
        }
    }

    pub(super) fn begin_gpu_region(&self, name: &CStr, color: [f32; 4], start_query: u32) {
        let label = vk::VkDebugUtilsLabelEXT {
            sType: vk::VK_STRUCTURE_TYPE_DEBUG_UTILS_LABEL_EXT,
            pLabelName: name.as_ptr(),
            color,
            ..Default::default()
        };
        // SAFETY: Debug utils is enabled, the command buffer is recording, and the label string
        // and structure remain live for the duration of the call.
        unsafe {
            self.device
                .functions
                .cmd_begin_debug_utils_label
                .expect("loaded function")(self.command_buffer, &raw const label);
            if !self.query_pool.is_null() {
                self.device
                    .functions
                    .cmd_write_timestamp2
                    .expect("loaded function")(
                    self.command_buffer,
                    vk::VK_PIPELINE_STAGE_2_TOP_OF_PIPE_BIT,
                    self.query_pool,
                    start_query,
                );
            }
        }
    }

    pub(super) fn end_gpu_region(&self, end_query: u32) {
        // SAFETY: The command buffer is recording and this closes the innermost debug label.
        unsafe {
            if !self.query_pool.is_null() {
                self.device
                    .functions
                    .cmd_write_timestamp2
                    .expect("loaded function")(
                    self.command_buffer,
                    vk::VK_PIPELINE_STAGE_2_BOTTOM_OF_PIPE_BIT,
                    self.query_pool,
                    end_query,
                );
            }
            self.device
                .functions
                .cmd_end_debug_utils_label
                .expect("loaded function")(self.command_buffer);
        }
    }

    pub(super) fn query_values<const COUNT: usize>(
        &self,
        first_query: u32,
    ) -> Result<[u64; COUNT], ProbeError> {
        let mut values = [0_u64; COUNT];
        check(
            // SAFETY: Fence completion makes every requested query available, and the output
            // array has one tightly packed u64 slot per query.
            unsafe {
                self.device
                    .functions
                    .get_query_pool_results
                    .expect("loaded function")(
                    self.device.handle,
                    self.query_pool,
                    first_query,
                    u32::try_from(COUNT).expect("query result count fits u32"),
                    mem::size_of_val(&values),
                    values.as_mut_ptr().cast(),
                    u64::try_from(mem::size_of::<u64>()).expect("u64 size fits VkDeviceSize"),
                    vk::VK_QUERY_RESULT_64_BIT.cast_unsigned(),
                )
            },
            "vkGetQueryPoolResults for GPU timestamps",
        )?;
        Ok(values)
    }

    #[allow(clippy::cast_precision_loss)]
    pub(super) fn timestamp_elapsed_ms(&self, start: u64, end: u64) -> f64 {
        let ticks = timestamp_tick_delta(start, end, self.device.adapter.timestamp_valid_bits);
        ticks as f64 * f64::from(self.device.adapter.timestamp_period) / 1_000_000.0
    }

    pub(super) fn collect_compute_gpu_timestamp(&self) -> Result<(), ProbeError> {
        if self.query_pool.is_null() {
            return Ok(());
        }
        let [start, end] = self.query_values::<2>(COMPUTE_QUERY_START)?;
        println!(
            "GPU timing: startup compute dispatch {:.3} ms",
            self.timestamp_elapsed_ms(start, end)
        );
        Ok(())
    }

    pub(super) fn collect_frame_gpu_timestamps(&mut self) -> Result<(), ProbeError> {
        if !self.gpu_timing.frame_query_pending {
            return Ok(());
        }
        let [
            shadow_start,
            shadow_end,
            scene_start,
            scene_end,
            post_start,
            post_end,
        ] = self.query_values::<6>(SHADOW_QUERY_START)?;
        self.gpu_timing.frame_query_pending = false;
        self.gpu_timing.samples += 1;
        self.gpu_timing.shadow_total_ms += self.timestamp_elapsed_ms(shadow_start, shadow_end);
        self.gpu_timing.scene_total_ms += self.timestamp_elapsed_ms(scene_start, scene_end);
        self.gpu_timing.post_total_ms += self.timestamp_elapsed_ms(post_start, post_end);
        Ok(())
    }

    #[allow(clippy::cast_precision_loss)]
    pub(super) fn report_gpu_timing(&mut self) {
        if self.gpu_timing.reported || self.query_pool.is_null() {
            return;
        }
        self.gpu_timing.reported = true;
        if self.gpu_timing.samples == 0 {
            println!("GPU timing summary: no rendered frame samples");
            return;
        }
        let samples = self.gpu_timing.samples as f64;
        println!(
            "GPU timing summary: frames={} shadow_avg={:.3} ms scene_avg={:.3} ms post_avg={:.3} ms",
            self.gpu_timing.samples,
            self.gpu_timing.shadow_total_ms / samples,
            self.gpu_timing.scene_total_ms / samples,
            self.gpu_timing.post_total_ms / samples
        );
    }
}
