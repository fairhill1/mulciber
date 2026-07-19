use super::{
    FRAME_SLOT_COUNT, FrameAcquire, FrameDisposition, FrameUniform, Instant, LiveResizeSample,
    POST_QUERY_END, POST_QUERY_START, ProbeError, Renderer, SCENE_QUERY_END, SCENE_QUERY_START,
    SHADOW_MAP_SIZE, SHADOW_QUERY_END, SHADOW_QUERY_START, SurfaceUnavailable, UINT64_MAX, check,
    color_subresource_range, command_buffer_submit_info, depth_subresource_range, mem, ptr, thread,
    vk,
};

impl Renderer {
    #[allow(clippy::too_many_lines)]
    pub(super) fn render(
        &mut self,
        width: u32,
        height: u32,
        live_resize: bool,
    ) -> Result<bool, ProbeError> {
        frame_trace("render begin");
        if let Some(stall) = self.present_pacing.spike_sleep() {
            thread::sleep(stall);
        }
        let trace_started = self.live_resize_trace.begin(live_resize);
        let mut trace_sample = LiveResizeSample::default();
        let operation_started = Instant::now();
        self.wait_for_frame()?;
        frame_trace("frame fence complete");
        self.collect_frame_gpu_timestamps()?;
        trace_sample.frame_wait = operation_started.elapsed();
        self.collect_retired_swapchains()?;
        if self.swapchain.is_null()
            || self.extent.width != width
            || self.extent.height != height
            || self.recreate_after_present
        {
            let operation_started = Instant::now();
            self.recreate_swapchain(width, height)?;
            trace_sample.recreate = Some(operation_started.elapsed());
        }

        let mut image_index = 0;
        let abandon_acquired_frame = self.frame_abandonment.should_abandon();
        let acquire_semaphore = if abandon_acquired_frame {
            ptr::null_mut()
        } else {
            self.image_available
        };
        let acquire_fence = if abandon_acquired_frame || !self.device.adapter.swapchain_maintenance1
        {
            self.acquire_fence
        } else {
            ptr::null_mut()
        };
        let operation_started = Instant::now();
        // SAFETY: Swapchain and semaphore are live; output is writable.
        let acquire = unsafe {
            self.device
                .functions
                .acquire_next_image
                .expect("loaded function")(
                self.device.handle,
                self.swapchain,
                self.acquire_timeout,
                acquire_semaphore,
                acquire_fence,
                &raw mut image_index,
            )
        };
        frame_trace("image acquired");
        let acquisition = if acquire == vk::VK_NOT_READY {
            FrameAcquire::Unavailable(SurfaceUnavailable::DrawableUnavailable)
        } else if acquire == vk::VK_TIMEOUT {
            FrameAcquire::Unavailable(SurfaceUnavailable::TimedOut)
        } else if acquire == vk::VK_ERROR_OUT_OF_DATE_KHR {
            trace_sample.acquire = operation_started.elapsed();
            let operation_started = Instant::now();
            self.recreate_swapchain(width, height)?;
            let recreate = operation_started.elapsed();
            trace_sample.recreate = Some(
                trace_sample
                    .recreate
                    .map_or(recreate, |previous| previous + recreate),
            );
            // The replacement swapchain provides no image this iteration; the caller acquires
            // from it on the next render.
            FrameAcquire::Unavailable(SurfaceUnavailable::DrawableUnavailable)
        } else {
            if acquire == vk::VK_SUBOPTIMAL_KHR {
                self.recreate_after_present = true;
            } else {
                check(acquire, "vkAcquireNextImageKHR")?;
            }
            FrameAcquire::Ready(image_index)
        };
        image_index = match acquisition {
            FrameAcquire::Ready(image_index) => image_index,
            FrameAcquire::Unavailable(_) => {
                self.live_resize_trace
                    .finish(trace_started, trace_sample, false);
                return Ok(false);
            }
        };

        let image_slot = image_index as usize;
        if !acquire_fence.is_null() {
            self.wait_and_reset_fence(acquire_fence, "image-acquisition fence")?;
        }
        if self.device.adapter.swapchain_maintenance1 {
            if self.present_pending[image_slot] {
                self.wait_and_reset_fence(
                    self.present_fences[image_slot],
                    "presentation fence for reacquired image",
                )?;
                self.present_pending[image_slot] = false;
            }
        } else if self.presented[image_slot] {
            self.destroy_all_retired_swapchains();
        }
        trace_sample.acquire = operation_started.elapsed();

        if abandon_acquired_frame {
            let operation_started = Instant::now();
            self.abandon_acquired_image(image_index, width, height)?;
            self.frame_abandonment.record_abandonment();
            let _disposition = FrameDisposition::Abandoned(
                self.surface_info
                    .expect("an acquired image has a configured surface generation")
                    .generation(),
            );
            trace_sample.recreate =
                (!self.device.adapter.swapchain_maintenance1).then(|| operation_started.elapsed());
            self.live_resize_trace
                .finish(trace_started, trace_sample, false);
            return Ok(false);
        }

        let view = *self
            .views
            .get(image_slot)
            .ok_or_else(|| ProbeError("driver returned an invalid swapchain image index".into()))?;
        let image = self.images[image_slot];
        let render_finished = *self
            .render_finished
            .get(image_slot)
            .ok_or_else(|| ProbeError("swapchain image has no presentation semaphore".into()))?;
        let descriptor_set = *self
            .descriptor_sets
            .get(self.frame_slot)
            .ok_or_else(|| ProbeError("current frame slot has no descriptor set".into()))?;
        self.write_frame_uniform(self.frame_slot)?;
        let operation_started = Instant::now();
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
        self.record(image, view, descriptor_set)?;
        self.submit(render_finished)?;
        frame_trace("graphics submitted");
        self.frame_slot = (self.frame_slot + 1) % FRAME_SLOT_COUNT;
        trace_sample.record_submit = operation_started.elapsed();

        let present_fence = self
            .present_fences
            .get(image_slot)
            .copied()
            .unwrap_or(ptr::null_mut());
        let present_fence_info = vk::VkSwapchainPresentFenceInfoKHR {
            sType: vk::VK_STRUCTURE_TYPE_SWAPCHAIN_PRESENT_FENCE_INFO_KHR,
            swapchainCount: 1,
            pFences: &raw const present_fence,
            ..Default::default()
        };
        let present = vk::VkPresentInfoKHR {
            sType: vk::VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,
            pNext: if present_fence.is_null() {
                ptr::null()
            } else {
                (&raw const present_fence_info).cast()
            },
            waitSemaphoreCount: 1,
            pWaitSemaphores: &raw const render_finished,
            swapchainCount: 1,
            pSwapchains: &raw const self.swapchain,
            pImageIndices: &raw const image_index,
            ..Default::default()
        };
        let operation_started = Instant::now();
        // SAFETY: Queue, swapchain, index, and wait semaphore are valid for this submission.
        let result = unsafe {
            self.device
                .functions
                .queue_present
                .expect("loaded function")(self.device.queue, &raw const present)
        };
        frame_trace("presentation queued");
        trace_sample.present = operation_started.elapsed();
        if result == vk::VK_SUCCESS
            || result == vk::VK_ERROR_OUT_OF_DATE_KHR
            || result == vk::VK_SUBOPTIMAL_KHR
        {
            if self.device.adapter.swapchain_maintenance1 {
                self.present_pending[image_slot] = true;
            } else {
                self.presented[image_slot] = true;
            }
        }
        if result == vk::VK_ERROR_OUT_OF_DATE_KHR || result == vk::VK_SUBOPTIMAL_KHR {
            self.recreate_after_present = true;
        } else {
            check(result, "vkQueuePresentKHR")?;
        }
        if matches!(result, vk::VK_SUCCESS | vk::VK_SUBOPTIMAL_KHR)
            && self.frame_abandonment.record_presentation()
        {
            println!("Acquired-frame abandonment: recovery confirmed by a later presented frame");
        }
        if matches!(result, vk::VK_SUCCESS | vk::VK_SUBOPTIMAL_KHR) {
            self.present_pacing.record_present_return();
            let _disposition = FrameDisposition::Presented(
                self.surface_info
                    .expect("a presented image has a configured surface generation")
                    .generation(),
            );
        }
        self.live_resize_trace
            .finish(trace_started, trace_sample, true);
        Ok(true)
    }

    fn abandon_acquired_image(
        &mut self,
        image_index: u32,
        width: u32,
        height: u32,
    ) -> Result<(), ProbeError> {
        if self.device.adapter.swapchain_maintenance1 {
            let release_info = vk::VkReleaseSwapchainImagesInfoKHR {
                sType: vk::VK_STRUCTURE_TYPE_RELEASE_SWAPCHAIN_IMAGES_INFO_KHR,
                swapchain: self.swapchain,
                imageIndexCount: 1,
                pImageIndices: &raw const image_index,
                ..Default::default()
            };
            // SAFETY: Acquisition completion was observed through the dedicated fence, no device
            // work references this image, and the maintenance feature is enabled on the device.
            check(
                unsafe {
                    self.device
                        .functions
                        .release_swapchain_images
                        .expect("loaded maintenance function")(
                        self.device.handle,
                        &raw const release_info,
                    )
                },
                "vkReleaseSwapchainImagesKHR",
            )?;
            println!(
                "Acquired-frame abandonment: released image {image_index} without submission or \
                 presentation via vkReleaseSwapchainImagesKHR"
            );
        } else {
            // Base VK_KHR_swapchain cannot return an acquired image without presentation. Retiring
            // this generation through oldSwapchain and later destroying it after all image uses
            // have completed is the compatibility recovery boundary.
            self.recreate_swapchain(width, height)?;
            println!(
                "Acquired-frame abandonment: retired the acquired image's swapchain generation \
                 without submission or presentation"
            );
        }
        Ok(())
    }

    #[allow(clippy::cast_precision_loss)]
    pub(super) fn write_frame_uniform(&self, slot: usize) -> Result<(), ProbeError> {
        let width = self.extent.width as f32;
        let height = self.extent.height as f32;
        let (scale_x, scale_y) = if width >= height {
            (height / width, 1.0)
        } else {
            (1.0, width / height)
        };
        let uniform = FrameUniform {
            transform: [
                [scale_x, 0.0, 0.0, 0.0],
                [0.0, scale_y, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            tint_time: [1.0, 1.0, 1.0, self.started.elapsed().as_secs_f32()],
        };
        let buffer = self
            .uniform_buffers
            .get(slot)
            .ok_or_else(|| ProbeError("current frame slot has no uniform buffer".into()))?;
        // SAFETY: The slot's persistent coherent mapping spans one `FrameUniform`. Frame-fence
        // completion was established before this write, so the GPU is not reading this allocation.
        unsafe {
            ptr::copy_nonoverlapping(
                (&raw const uniform).cast::<u8>(),
                buffer.mapped.cast(),
                mem::size_of::<FrameUniform>(),
            );
        }
        Ok(())
    }

    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    pub(super) fn record_shadow_pass(&self) {
        self.begin_gpu_region(c"shadow", [0.55, 0.35, 0.15, 1.00], SHADOW_QUERY_START);
        self.image_barrier(
            self.shadow_map.handle,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_NONE,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
            vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_READ_BIT
                | vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            depth_subresource_range(),
        );
        let depth_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: self.shadow_map.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            resolveMode: vk::VK_RESOLVE_MODE_NONE,
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: vk::VkClearValue {
                depthStencil: vk::VkClearDepthStencilValue {
                    depth: 1.0,
                    stencil: 0,
                },
            },
            ..Default::default()
        };
        let extent = vk::VkExtent2D {
            width: SHADOW_MAP_SIZE,
            height: SHADOW_MAP_SIZE,
        };
        let render_area = vk::VkRect2D {
            offset: vk::VkOffset2D { x: 0, y: 0 },
            extent,
        };
        let rendering = vk::VkRenderingInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
            renderArea: render_area,
            layerCount: 1,
            pDepthAttachment: &raw const depth_attachment,
            ..Default::default()
        };
        let viewport = vk::VkViewport {
            x: 0.0,
            y: 0.0,
            width: SHADOW_MAP_SIZE as f32,
            height: SHADOW_MAP_SIZE as f32,
            minDepth: 0.0,
            maxDepth: 1.0,
        };
        let vertex_offset: vk::VkDeviceSize = 0;
        // SAFETY: The command buffer is recording and all shadow-pass resources are live.
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
                self.shadow_pipeline,
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
            self.device
                .functions
                .cmd_bind_vertex_buffers
                .expect("loaded function")(
                self.command_buffer,
                0,
                1,
                &raw const self.vertex_buffer.handle,
                &raw const vertex_offset,
            );
            self.device
                .functions
                .cmd_bind_index_buffer
                .expect("loaded function")(
                self.command_buffer,
                self.index_buffer.handle,
                0,
                vk::VK_INDEX_TYPE_UINT16,
            );
            self.device
                .functions
                .cmd_draw_indexed_indirect
                .expect("loaded function")(
                self.command_buffer,
                self.compute_indirect.handle,
                0,
                1,
                u32::try_from(mem::size_of::<vk::VkDrawIndexedIndirectCommand>())
                    .expect("indexed-indirect stride fits u32"),
            );
            self.device
                .functions
                .cmd_end_rendering
                .expect("loaded function")(self.command_buffer);
        }
        self.image_barrier(
            self.shadow_map.handle,
            vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
            vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
            vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
            vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            depth_subresource_range(),
        );
        self.end_gpu_region(SHADOW_QUERY_END);
    }

    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    pub(super) fn record(
        &self,
        image: vk::VkImage,
        view: vk::VkImageView,
        descriptor_set: vk::VkDescriptorSet,
    ) -> Result<(), ProbeError> {
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

        self.reset_gpu_queries(SHADOW_QUERY_START, 6);
        self.record_shadow_pass();
        self.begin_gpu_region(c"scene", [0.25, 0.85, 0.35, 1.00], SCENE_QUERY_START);

        self.image_barrier(
            self.offscreen.handle,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_NONE,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            color_subresource_range(),
        );
        let multisampled = self.device.adapter.sample_count != vk::VK_SAMPLE_COUNT_1_BIT;
        if multisampled {
            self.image_barrier(
                self.msaa_color.handle,
                vk::VK_PIPELINE_STAGE_2_NONE,
                vk::VK_ACCESS_2_NONE,
                vk::VK_IMAGE_LAYOUT_UNDEFINED,
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                color_subresource_range(),
            );
        }
        self.image_barrier(
            self.depth.handle,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_NONE,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
            vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_READ_BIT
                | vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            depth_subresource_range(),
        );
        let attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: if multisampled {
                self.msaa_color.view
            } else {
                self.offscreen.view
            },
            imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            resolveMode: if multisampled {
                vk::VK_RESOLVE_MODE_AVERAGE_BIT
            } else {
                vk::VK_RESOLVE_MODE_NONE
            },
            resolveImageView: if multisampled {
                self.offscreen.view
            } else {
                ptr::null_mut()
            },
            resolveImageLayout: if multisampled {
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL
            } else {
                vk::VK_IMAGE_LAYOUT_UNDEFINED
            },
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: if multisampled {
                vk::VK_ATTACHMENT_STORE_OP_DONT_CARE
            } else {
                vk::VK_ATTACHMENT_STORE_OP_STORE
            },
            clearValue: vk::VkClearValue {
                color: vk::VkClearColorValue {
                    float32: [0.025, 0.035, 0.055, 1.0],
                },
            },
            ..Default::default()
        };
        let depth_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: self.depth.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            resolveMode: vk::VK_RESOLVE_MODE_NONE,
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_DONT_CARE,
            clearValue: vk::VkClearValue {
                depthStencil: vk::VkClearDepthStencilValue {
                    depth: 1.0,
                    stencil: 0,
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
            pDepthAttachment: &raw const depth_attachment,
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
                .cmd_bind_descriptor_sets
                .expect("loaded function")(
                self.command_buffer,
                vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                self.pipeline_layout,
                0,
                1,
                &raw const descriptor_set,
                0,
                ptr::null(),
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
            let vertex_offset: vk::VkDeviceSize = 0;
            self.device
                .functions
                .cmd_bind_vertex_buffers
                .expect("loaded function")(
                self.command_buffer,
                0,
                1,
                &raw const self.vertex_buffer.handle,
                &raw const vertex_offset,
            );
            self.device
                .functions
                .cmd_bind_index_buffer
                .expect("loaded function")(
                self.command_buffer,
                self.index_buffer.handle,
                0,
                vk::VK_INDEX_TYPE_UINT16,
            );
            self.device
                .functions
                .cmd_draw_indexed_indirect
                .expect("loaded function")(
                self.command_buffer,
                self.compute_indirect.handle,
                0,
                1,
                u32::try_from(mem::size_of::<vk::VkDrawIndexedIndirectCommand>())
                    .expect("indexed-indirect stride fits u32"),
            );
            self.device
                .functions
                .cmd_end_rendering
                .expect("loaded function")(self.command_buffer);
        }
        self.end_gpu_region(SCENE_QUERY_END);
        self.begin_gpu_region(c"post", [0.85, 0.30, 0.90, 1.00], POST_QUERY_START);
        self.record_postprocess(image, view, &viewport, &render_area);
        self.end_gpu_region(POST_QUERY_END);
        self.image_barrier(
            image,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_NONE,
            vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
            color_subresource_range(),
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

    pub(super) fn record_postprocess(
        &self,
        swapchain_image: vk::VkImage,
        swapchain_view: vk::VkImageView,
        viewport: &vk::VkViewport,
        render_area: &vk::VkRect2D,
    ) {
        self.image_barrier(
            self.offscreen.handle,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
            vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
            vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            color_subresource_range(),
        );
        self.image_barrier(
            swapchain_image,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_NONE,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            color_subresource_range(),
        );
        let attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: swapchain_view,
            imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            resolveMode: vk::VK_RESOLVE_MODE_NONE,
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: vk::VkClearValue {
                color: vk::VkClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                },
            },
            ..Default::default()
        };
        let rendering = vk::VkRenderingInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
            renderArea: *render_area,
            layerCount: 1,
            colorAttachmentCount: 1,
            pColorAttachments: &raw const attachment,
            ..Default::default()
        };
        // SAFETY: Both render targets, the sampled offscreen descriptor, and pipelines are live.
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
                self.post_pipeline,
            );
            self.device
                .functions
                .cmd_bind_descriptor_sets
                .expect("loaded function")(
                self.command_buffer,
                vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                self.post_pipeline_layout,
                0,
                1,
                &raw const self.post_descriptor_set,
                0,
                ptr::null(),
            );
            self.device
                .functions
                .cmd_set_viewport
                .expect("loaded function")(self.command_buffer, 0, 1, viewport);
            self.device
                .functions
                .cmd_set_scissor
                .expect("loaded function")(self.command_buffer, 0, 1, render_area);
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
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn image_barrier(
        &self,
        image: vk::VkImage,
        source_stage: vk::VkPipelineStageFlags2,
        source_access: vk::VkAccessFlags2,
        old_layout: vk::VkImageLayout,
        destination_stage: vk::VkPipelineStageFlags2,
        destination_access: vk::VkAccessFlags2,
        new_layout: vk::VkImageLayout,
        subresource_range: vk::VkImageSubresourceRange,
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
            subresourceRange: subresource_range,
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

    pub(super) fn submit(&mut self, render_finished: vk::VkSemaphore) -> Result<(), ProbeError> {
        let wait = vk::VkSemaphoreSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_SEMAPHORE_SUBMIT_INFO,
            semaphore: self.image_available,
            stageMask: vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            ..Default::default()
        };
        let command = command_buffer_submit_info(self.command_buffer);
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
        )?;
        self.frame_pending = true;
        if !self.query_pool.is_null() {
            self.gpu_timing.frame_query_pending = true;
        }
        Ok(())
    }

    pub(super) fn finish(&mut self) -> Result<(), ProbeError> {
        frame_trace("finish begin");
        self.live_resize_trace.report();
        if !self.device.adapter.swapchain_maintenance1 {
            // Base VK_KHR_swapchain lacks a portable fence for presentation completion at final
            // shutdown, so the compatibility path retains the conventional orderly-idle fallback.
            check(
                unsafe {
                    self.device
                        .functions
                        .device_wait_idle
                        .expect("loaded function")(self.device.handle)
                },
                "vkDeviceWaitIdle",
            )?;
            self.wait_for_frame()?;
            self.collect_frame_gpu_timestamps()?;
            self.report_gpu_timing();
            self.finish_present_pacing()?;
            self.save_pipeline_cache()?;
            return self.frame_abandonment.require_recovery();
        }

        self.wait_for_frame()?;
        frame_trace("final frame fence complete");
        self.collect_frame_gpu_timestamps()?;
        for (&fence, &pending) in self.present_fences.iter().zip(&self.present_pending) {
            if pending {
                frame_trace("waiting for final presentation fence");
                // SAFETY: The live fence was attached to an enqueued presentation request.
                check(
                    unsafe {
                        self.device
                            .functions
                            .wait_for_fences
                            .expect("loaded function")(
                            self.device.handle,
                            1,
                            &raw const fence,
                            vk::VK_TRUE,
                            UINT64_MAX,
                        )
                    },
                    "vkWaitForFences for presentation at shutdown",
                )?;
                frame_trace("final presentation fence complete");
            }
        }
        self.present_pending.fill(false);
        for retired in &self.retired {
            for (&fence, &pending) in retired.present_fences.iter().zip(&retired.present_pending) {
                if pending {
                    // SAFETY: The retired swapchain owns this live presentation fence.
                    check(
                        unsafe {
                            self.device
                                .functions
                                .wait_for_fences
                                .expect("loaded function")(
                                self.device.handle,
                                1,
                                &raw const fence,
                                vk::VK_TRUE,
                                UINT64_MAX,
                            )
                        },
                        "vkWaitForFences for retired presentation at shutdown",
                    )?;
                }
            }
        }
        for retired in &mut self.retired {
            retired.present_pending.fill(false);
        }
        self.report_gpu_timing();
        self.finish_present_pacing()?;
        self.save_pipeline_cache()?;
        self.frame_abandonment.require_recovery()
    }

    pub(super) fn destroy_swapchain_resources(&mut self) {
        self.retire_current_swapchain(true);
        self.destroy_all_retired_swapchains();
    }

    pub(super) unsafe fn destroy_gpu_instrumentation(&mut self) {
        if !self.query_pool.is_null() {
            // SAFETY: Shutdown completed every command that references this owned query pool.
            unsafe {
                self.device
                    .functions
                    .destroy_query_pool
                    .expect("loaded function")(
                    self.device.handle, self.query_pool, ptr::null()
                );
            }
            self.query_pool = ptr::null_mut();
        }
    }

    pub(super) unsafe fn destroy_owned_pipeline_cache(&mut self) {
        if !self.pipeline_cache.handle.is_null() {
            // SAFETY: The cache is owned by this renderer and no creation call is in progress.
            unsafe {
                self.device
                    .functions
                    .destroy_pipeline_cache
                    .expect("loaded function")(
                    self.device.handle,
                    self.pipeline_cache.handle,
                    ptr::null(),
                );
            }
            self.pipeline_cache.handle = ptr::null_mut();
        }
    }
}

fn frame_trace(message: &str) {
    if std::env::var_os("MULCIBER_VULKAN_FRAME_TRACE").is_some() {
        eprintln!("frame trace: {message}");
    }
}
