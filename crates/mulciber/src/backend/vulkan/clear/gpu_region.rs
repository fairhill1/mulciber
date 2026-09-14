use super::{CStr, DeviceFns, vk};

impl DeviceFns {
    /// Record a timestamp whether or not development labels are enabled.
    ///
    /// # Safety
    /// The command buffer is recording on this device and the timestamp query is reset.
    pub(super) unsafe fn begin_gpu_region(
        &self,
        command: vk::VkCommandBuffer,
        pool: vk::VkQueryPool,
        query: u32,
        name: &CStr,
        color: [f32; 4],
    ) {
        unsafe {
            if let Some(begin_label) = self.cmd_begin_debug_utils_label {
                let label = vk::VkDebugUtilsLabelEXT {
                    sType: vk::VK_STRUCTURE_TYPE_DEBUG_UTILS_LABEL_EXT,
                    pLabelName: name.as_ptr(),
                    color,
                    ..Default::default()
                };
                begin_label(command, &raw const label);
            }
            self.cmd_write_timestamp2.expect("loaded function")(
                command,
                vk::VK_PIPELINE_STAGE_2_TOP_OF_PIPE_BIT,
                pool,
                query,
            );
        }
    }

    /// # Safety
    /// The command buffer is recording with a matching begun region and a reset query.
    pub(super) unsafe fn end_gpu_region(
        &self,
        command: vk::VkCommandBuffer,
        pool: vk::VkQueryPool,
        query: u32,
    ) {
        unsafe {
            self.cmd_write_timestamp2.expect("loaded function")(
                command,
                vk::VK_PIPELINE_STAGE_2_BOTTOM_OF_PIPE_BIT,
                pool,
                query,
            );
            if let Some(end_label) = self.cmd_end_debug_utils_label {
                end_label(command);
            }
        }
    }
}
