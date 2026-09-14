use super::{instance_tests::windowless_device, *};

struct Commands<'a> {
    device: &'a Device,
    pool: vk::VkCommandPool,
    queries: vk::VkQueryPool,
    command: vk::VkCommandBuffer,
}

impl<'a> Commands<'a> {
    fn new(device: &'a Device) -> Self {
        let mut commands = Self {
            device,
            pool: ptr::null_mut(),
            queries: ptr::null_mut(),
            command: ptr::null_mut(),
        };
        let functions = &device.functions;
        let pool_info = vk::VkCommandPoolCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
            queueFamilyIndex: device.adapter.queue_family,
            ..Default::default()
        };
        let query_info = vk::VkQueryPoolCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO,
            queryType: vk::VK_QUERY_TYPE_TIMESTAMP,
            queryCount: 2,
            ..Default::default()
        };
        unsafe {
            // SAFETY: Live device, valid create infos and writable outputs.
            check(
                functions.create_command_pool.unwrap()(
                    device.handle,
                    &raw const pool_info,
                    ptr::null(),
                    &raw mut commands.pool,
                ),
                "test command pool",
            )
            .unwrap();
            check(
                functions.create_query_pool.unwrap()(
                    device.handle,
                    &raw const query_info,
                    ptr::null(),
                    &raw mut commands.queries,
                ),
                "test timestamp queries",
            )
            .unwrap();
            let allocation = vk::VkCommandBufferAllocateInfo {
                sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                commandPool: commands.pool,
                level: vk::VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                commandBufferCount: 1,
                ..Default::default()
            };
            check(
                functions.allocate_command_buffers.unwrap()(
                    device.handle,
                    &raw const allocation,
                    &raw mut commands.command,
                ),
                "test command buffer",
            )
            .unwrap();
        }
        commands
    }

    fn submit_timed_region(&self) {
        let functions = &self.device.functions;
        let begin = vk::VkCommandBufferBeginInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            ..Default::default()
        };
        unsafe {
            // SAFETY: The owned buffer is initial, and both owned queries are reset
            // before the same region-recording methods used by the renderer.
            check(
                functions.begin_command_buffer.unwrap()(self.command, &raw const begin),
                "begin",
            )
            .unwrap();
            functions.cmd_reset_query_pool.unwrap()(self.command, self.queries, 0, 2);
            functions.begin_gpu_region(self.command, self.queries, 0, c"SDK-free region", [1.0; 4]);
            functions.end_gpu_region(self.command, self.queries, 1);
            check(functions.end_command_buffer.unwrap()(self.command), "end").unwrap();
            let command_info = vk::VkCommandBufferSubmitInfo {
                sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO,
                commandBuffer: self.command,
                ..Default::default()
            };
            let submit = vk::VkSubmitInfo2 {
                sType: vk::VK_STRUCTURE_TYPE_SUBMIT_INFO_2,
                commandBufferInfoCount: 1,
                pCommandBufferInfos: &raw const command_info,
                ..Default::default()
            };
            check(
                functions.queue_submit2.unwrap()(
                    self.device.queue,
                    1,
                    &raw const submit,
                    ptr::null_mut(),
                ),
                "submit",
            )
            .unwrap();
            check(
                functions.device_wait_idle.unwrap()(self.device.handle),
                "wait for timestamps",
            )
            .unwrap();
            let mut results = [0_u64; 4];
            check(
                functions.get_query_pool_results.unwrap()(
                    self.device.handle,
                    self.queries,
                    0,
                    2,
                    mem::size_of_val(&results),
                    results.as_mut_ptr().cast(),
                    16,
                    (vk::VK_QUERY_RESULT_64_BIT | vk::VK_QUERY_RESULT_WITH_AVAILABILITY_BIT) as u32,
                ),
                "read timestamps",
            )
            .unwrap();
            assert_ne!(results[1], 0, "start timestamp unavailable");
            assert_ne!(results[3], 0, "end timestamp unavailable");
            assert!(
                results[0] != 0 || results[2] != 0,
                "timestamps were not written"
            );
        }
    }
}

impl Drop for Commands<'_> {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: Wait even when an assertion unwinds after submission. All
            // children are destroyed before the borrowed device can be dropped.
            let functions = &self.device.functions;
            functions.device_wait_idle.unwrap()(self.device.handle);
            if !self.pool.is_null() {
                functions.destroy_command_pool.unwrap()(self.device.handle, self.pool, ptr::null());
            }
            if !self.queries.is_null() {
                functions.destroy_query_pool.unwrap()(
                    self.device.handle,
                    self.queries,
                    ptr::null(),
                );
            }
        }
    }
}

fn timed_region(validation: bool) {
    let device = windowless_device(validation);
    assert_eq!(
        device.functions.cmd_begin_debug_utils_label.is_some(),
        validation
    );
    assert_eq!(
        device.functions.cmd_end_debug_utils_label.is_some(),
        validation
    );
    assert_ne!(
        device.adapter.timestamp_valid_bits, 0,
        "test needs GPU timestamps"
    );
    Commands::new(&device).submit_timed_region();
}

#[test]
#[ignore = "requires native Vulkan with timestamps; creates no window or surface"]
fn native_instance_device_submits_timestamps_without_debug_utils() {
    timed_region(false);
}

#[test]
#[ignore = "requires native Vulkan with timestamps and validation; creates no window or surface"]
fn native_instance_device_submits_timestamps_with_validation() {
    VALIDATION_MESSAGE_COUNT.store(0, Ordering::Relaxed);
    timed_region(true);
    assert_eq!(VALIDATION_MESSAGE_COUNT.load(Ordering::Relaxed), 0);
}
