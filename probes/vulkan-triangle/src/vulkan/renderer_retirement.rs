use super::{
    DeviceContext, ProbeError, Renderer, RetiredSwapchain, UINT64_MAX, check, destroy_gpu_image,
    mem, ptr, vk,
};

impl Renderer {
    pub(super) fn wait_for_frame(&mut self) -> Result<(), ProbeError> {
        if !self.frame_pending {
            return Ok(());
        }
        // SAFETY: The fence is live and belongs to this device.
        check(
            unsafe {
                self.device
                    .functions
                    .wait_for_fences
                    .expect("loaded function")(
                    self.device.handle,
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

    pub(super) fn wait_and_reset_fence(
        &self,
        fence: vk::VkFence,
        description: &str,
    ) -> Result<(), ProbeError> {
        // SAFETY: The fence is live, and the caller only resets it after its signal was observed.
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
            &format!("vkWaitForFences for {description}"),
        )?;
        check(
            unsafe {
                self.device.functions.reset_fences.expect("loaded function")(
                    self.device.handle,
                    1,
                    &raw const fence,
                )
            },
            &format!("vkResetFences for {description}"),
        )
    }

    pub(super) fn retire_current_swapchain(&mut self, retire_pipeline: bool) {
        if self.swapchain.is_null() {
            return;
        }
        self.retired.push(RetiredSwapchain {
            handle: mem::replace(&mut self.swapchain, ptr::null_mut()),
            views: mem::take(&mut self.views),
            offscreen: mem::take(&mut self.offscreen),
            msaa_color: mem::take(&mut self.msaa_color),
            depth: mem::take(&mut self.depth),
            pipeline_layout: if retire_pipeline {
                mem::replace(&mut self.pipeline_layout, ptr::null_mut())
            } else {
                ptr::null_mut()
            },
            pipeline: if retire_pipeline {
                mem::replace(&mut self.pipeline, ptr::null_mut())
            } else {
                ptr::null_mut()
            },
            post_pipeline_layout: if retire_pipeline {
                mem::replace(&mut self.post_pipeline_layout, ptr::null_mut())
            } else {
                ptr::null_mut()
            },
            post_pipeline: if retire_pipeline {
                mem::replace(&mut self.post_pipeline, ptr::null_mut())
            } else {
                ptr::null_mut()
            },
            render_finished: mem::take(&mut self.render_finished),
            present_fences: mem::take(&mut self.present_fences),
            present_pending: mem::take(&mut self.present_pending),
        });
        self.images.clear();
        self.presented.clear();
    }

    pub(super) fn retired_swapchain_ready(
        &self,
        retired: &RetiredSwapchain,
    ) -> Result<bool, ProbeError> {
        for (&fence, &pending) in retired.present_fences.iter().zip(&retired.present_pending) {
            if !pending {
                continue;
            }
            // SAFETY: The presentation fence remains live while its status is queried.
            let result = unsafe {
                self.device
                    .functions
                    .get_fence_status
                    .expect("loaded function")(self.device.handle, fence)
            };
            if result == vk::VK_NOT_READY {
                return Ok(false);
            }
            check(result, "vkGetFenceStatus for retired swapchain")?;
        }
        Ok(true)
    }

    pub(super) fn collect_retired_swapchains(&mut self) -> Result<(), ProbeError> {
        if !self.device.adapter.swapchain_maintenance1 {
            return Ok(());
        }
        let mut index = 0;
        while index < self.retired.len() {
            if self.retired_swapchain_ready(&self.retired[index])? {
                let retired = self.retired.remove(index);
                Self::destroy_retired_swapchain(&self.device, retired);
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    pub(super) fn destroy_retired_swapchain(device: &DeviceContext, retired: RetiredSwapchain) {
        // SAFETY: Completion was established before this owned resource set reached this helper.
        unsafe {
            let mut offscreen = retired.offscreen;
            destroy_gpu_image(device, &mut offscreen);
            let mut msaa_color = retired.msaa_color;
            destroy_gpu_image(device, &mut msaa_color);
            let mut depth = retired.depth;
            destroy_gpu_image(device, &mut depth);
            if !retired.pipeline.is_null() {
                device.functions.destroy_pipeline.expect("loaded function")(
                    device.handle,
                    retired.pipeline,
                    ptr::null(),
                );
            }
            if !retired.pipeline_layout.is_null() {
                device
                    .functions
                    .destroy_pipeline_layout
                    .expect("loaded function")(
                    device.handle, retired.pipeline_layout, ptr::null()
                );
            }
            if !retired.post_pipeline.is_null() {
                device.functions.destroy_pipeline.expect("loaded function")(
                    device.handle,
                    retired.post_pipeline,
                    ptr::null(),
                );
            }
            if !retired.post_pipeline_layout.is_null() {
                device
                    .functions
                    .destroy_pipeline_layout
                    .expect("loaded function")(
                    device.handle,
                    retired.post_pipeline_layout,
                    ptr::null(),
                );
            }
            for view in retired.views {
                device
                    .functions
                    .destroy_image_view
                    .expect("loaded function")(device.handle, view, ptr::null());
            }
            for semaphore in retired.render_finished {
                device.functions.destroy_semaphore.expect("loaded function")(
                    device.handle,
                    semaphore,
                    ptr::null(),
                );
            }
            for fence in retired.present_fences {
                device.functions.destroy_fence.expect("loaded function")(
                    device.handle,
                    fence,
                    ptr::null(),
                );
            }
            device.functions.destroy_swapchain.expect("loaded function")(
                device.handle,
                retired.handle,
                ptr::null(),
            );
        }
    }

    pub(super) fn destroy_all_retired_swapchains(&mut self) {
        for retired in self.retired.drain(..) {
            Self::destroy_retired_swapchain(&self.device, retired);
        }
    }
}
