//! GPU mip generation: every level a linear blit of the one above it.
use super::{color_subresource_layers, image_barrier, mip_extent, vk};
use core::slice;

/// The logical device this records on; mip generation needs nothing from the surface.
type Device = super::super::Device;

const SHADER_STAGES: vk::VkPipelineStageFlags2 =
    vk::VK_PIPELINE_STAGE_2_VERTEX_SHADER_BIT | vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT;
/// Level 0 is written by a copy, every later level by a blit.
const TRANSFER_STAGES: vk::VkPipelineStageFlags2 =
    vk::VK_PIPELINE_STAGE_2_COPY_BIT | vk::VK_PIPELINE_STAGE_2_BLIT_BIT;

/// Records the blits that fill levels `1..mip_levels` of `image` from level 0, then leaves every
/// level shader-readable for the vertex and fragment stages.
///
/// On entry every level of every layer is in `TRANSFER_DST_OPTIMAL`, with level 0 already
/// written by a copy recorded earlier in `command`. Each blit halves the level above with a
/// linear filter (a 2×2 box for even extents), which the format's `BLIT_SRC`, `BLIT_DST` and
/// `SAMPLED_IMAGE_FILTER_LINEAR` features must allow; creation checks them.
pub(super) fn record_generated_mips(
    device: &Device,
    command: vk::VkCommandBuffer,
    image: vk::VkImage,
    extent: [u32; 2],
    mip_levels: u32,
    layers: u32,
) {
    let levels = |base: u32, count: u32| vk::VkImageSubresourceRange {
        baseMipLevel: base,
        levelCount: count,
        ..color_subresource_layers(1, layers)
    };
    let subresource = |level: u32| vk::VkImageSubresourceLayers {
        aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
        mipLevel: level,
        baseArrayLayer: 0,
        layerCount: layers,
    };
    let corner = |level: u32| vk::VkOffset3D {
        x: i32::try_from(mip_extent(extent[0], level)).expect("image extent fits i32"),
        y: i32::try_from(mip_extent(extent[1], level)).expect("image extent fits i32"),
        z: 1,
    };
    let origin = vk::VkOffset3D { x: 0, y: 0, z: 0 };
    for level in 1..mip_levels {
        let source = image_barrier(
            image,
            vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            TRANSFER_STAGES,
            vk::VK_PIPELINE_STAGE_2_BLIT_BIT,
            vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
            vk::VK_ACCESS_2_TRANSFER_READ_BIT,
            levels(level - 1, 1),
        );
        barriers(device, command, slice::from_ref(&source));
        let region = vk::VkImageBlit2 {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_BLIT_2,
            srcSubresource: subresource(level - 1),
            srcOffsets: [origin, corner(level - 1)],
            dstSubresource: subresource(level),
            dstOffsets: [origin, corner(level)],
            ..Default::default()
        };
        let blit = vk::VkBlitImageInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_BLIT_IMAGE_INFO_2,
            srcImage: image,
            srcImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            dstImage: image,
            dstImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            regionCount: 1,
            pRegions: &raw const region,
            filter: vk::VK_FILTER_LINEAR,
            ..Default::default()
        };
        unsafe {
            device.functions.cmd_blit_image2.expect("loaded function")(command, &raw const blit);
        }
    }
    // Every level above the last was read by a blit; the last was only written.
    let last = mip_levels - 1;
    let read = image_barrier(
        image,
        vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
        vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        vk::VK_PIPELINE_STAGE_2_BLIT_BIT,
        SHADER_STAGES,
        vk::VK_ACCESS_2_NONE,
        vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
        levels(0, last),
    );
    let written = image_barrier(
        image,
        vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        TRANSFER_STAGES,
        SHADER_STAGES,
        vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
        vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
        levels(last, 1),
    );
    if last == 0 {
        barriers(device, command, slice::from_ref(&written));
    } else {
        barriers(device, command, &[read, written]);
    }
}

fn barriers(device: &Device, command: vk::VkCommandBuffer, barriers: &[vk::VkImageMemoryBarrier2]) {
    let dependency = vk::VkDependencyInfo {
        sType: vk::VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
        imageMemoryBarrierCount: u32::try_from(barriers.len()).expect("barrier count fits u32"),
        pImageMemoryBarriers: barriers.as_ptr(),
        ..Default::default()
    };
    unsafe {
        device
            .functions
            .cmd_pipeline_barrier2
            .expect("loaded function")(command, &raw const dependency);
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{VALIDATION_MESSAGE_COUNT, instance_tests::windowless_device};
    use super::super::{
        Image, ImageShape, check, complete_image_storage, destroy_image_device, find_memory_type,
    };
    use super::{Device, record_generated_mips, vk};
    use core::{ptr, slice};
    use std::sync::atomic::Ordering;
    use std::vec::Vec;

    const WIDTH: u32 = 4;
    const HEIGHT: u32 = 2;
    const LEVELS: u32 = 3;
    /// Level bytes at eight per texel: 4×2, 2×1 and 1×1, back to back.
    const OFFSETS: [u64; 3] = [0, 64, 80];
    const SIZE: u64 = 88;
    const HALVES: usize = 44;

    /// A host-visible transfer buffer, mapped for its whole life.
    struct HostBuffer<'a> {
        device: &'a Device,
        handle: vk::VkBuffer,
        memory: vk::VkDeviceMemory,
        mapped: *mut u8,
    }

    impl<'a> HostBuffer<'a> {
        fn new(device: &'a Device) -> Self {
            let functions = &device.functions;
            let info = vk::VkBufferCreateInfo {
                sType: vk::VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
                size: SIZE,
                usage: (vk::VK_BUFFER_USAGE_TRANSFER_SRC_BIT
                    | vk::VK_BUFFER_USAGE_TRANSFER_DST_BIT)
                    .cast_unsigned(),
                sharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
                ..Default::default()
            };
            let mut buffer = Self {
                device,
                handle: ptr::null_mut(),
                memory: ptr::null_mut(),
                mapped: ptr::null_mut(),
            };
            unsafe {
                // SAFETY: Live device, valid create infos and writable outputs.
                check(
                    functions.create_buffer.unwrap()(
                        device.handle,
                        &raw const info,
                        ptr::null(),
                        &raw mut buffer.handle,
                    ),
                    "test buffer",
                )
                .unwrap();
                let mut requirements = vk::VkMemoryRequirements::default();
                functions.get_buffer_memory_requirements.unwrap()(
                    device.handle,
                    buffer.handle,
                    &raw mut requirements,
                );
                let allocation = vk::VkMemoryAllocateInfo {
                    sType: vk::VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                    allocationSize: requirements.size,
                    memoryTypeIndex: find_memory_type(
                        device,
                        requirements.memoryTypeBits,
                        (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
                            | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
                            .cast_unsigned(),
                    )
                    .unwrap(),
                    ..Default::default()
                };
                check(
                    functions.allocate_memory.unwrap()(
                        device.handle,
                        &raw const allocation,
                        ptr::null(),
                        &raw mut buffer.memory,
                    ),
                    "test buffer memory",
                )
                .unwrap();
                check(
                    functions.bind_buffer_memory.unwrap()(
                        device.handle,
                        buffer.handle,
                        buffer.memory,
                        0,
                    ),
                    "bind test buffer",
                )
                .unwrap();
                let mut mapped = ptr::null_mut();
                check(
                    functions.map_memory.unwrap()(
                        device.handle,
                        buffer.memory,
                        0,
                        SIZE,
                        0,
                        &raw mut mapped,
                    ),
                    "map test buffer",
                )
                .unwrap();
                buffer.mapped = mapped.cast();
            }
            buffer
        }

        fn halves(&self) -> &[u16] {
            // SAFETY: The coherent mapping spans `SIZE` bytes, aligned for u16.
            unsafe { slice::from_raw_parts(self.mapped.cast(), HALVES) }
        }

        fn halves_mut(&mut self) -> &mut [u16] {
            // SAFETY: As `halves`, and the GPU is idle while the host writes.
            unsafe { slice::from_raw_parts_mut(self.mapped.cast(), HALVES) }
        }
    }

    impl Drop for HostBuffer<'_> {
        fn drop(&mut self) {
            let functions = &self.device.functions;
            unsafe {
                // SAFETY: The device is idle and the handles are owned; null handles are
                // ignored by Vulkan.
                functions.destroy_buffer.unwrap()(self.device.handle, self.handle, ptr::null());
                functions.free_memory.unwrap()(self.device.handle, self.memory, ptr::null());
            }
        }
    }

    fn half_to_f32(half: u16) -> f32 {
        let exponent = i32::from((half >> 10) & 0x1f);
        let mantissa = f32::from(half & 0x3ff);
        let magnitude = if exponent == 0 {
            mantissa * (2.0_f32).powi(-24)
        } else {
            (1.0 + mantissa / 1024.0) * (2.0_f32).powi(exponent - 15)
        };
        if half & 0x8000 == 0 {
            magnitude
        } else {
            -magnitude
        }
    }

    fn region(level: u32) -> vk::VkBufferImageCopy2 {
        vk::VkBufferImageCopy2 {
            sType: vk::VK_STRUCTURE_TYPE_BUFFER_IMAGE_COPY_2,
            bufferOffset: OFFSETS[level as usize],
            imageSubresource: vk::VkImageSubresourceLayers {
                aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT.cast_unsigned(),
                mipLevel: level,
                baseArrayLayer: 0,
                layerCount: 1,
            },
            imageExtent: vk::VkExtent3D {
                width: (WIDTH >> level).max(1),
                height: (HEIGHT >> level).max(1),
                depth: 1,
            },
            ..Default::default()
        }
    }

    /// Uploads level 0, generates the chain as texture creation does, and reads every level
    /// back: each must be the 2×2 box average of the one above.
    #[allow(clippy::too_many_lines)] // One linear native sequence reads best in one place.
    #[test]
    #[ignore = "requires native Vulkan with validation; creates no window or surface"]
    fn generated_mips_average_each_level_under_validation() {
        VALIDATION_MESSAGE_COUNT.store(0, Ordering::Relaxed);
        let device = windowless_device(true);
        let functions = &device.functions;
        let format = vk::VK_FORMAT_R16G16B16A16_SFLOAT;
        let mut buffer = HostBuffer::new(&device);
        // Texel (x, y) holds x + 4y in every channel: 0..=7 are exact halves.
        let base: [u16; 8] = [
            0x0000, 0x3c00, 0x4000, 0x4200, 0x4400, 0x4500, 0x4600, 0x4700,
        ];
        for (texel, value) in buffer.halves_mut()[..32]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(base)
        {
            texel.fill(value);
        }
        let info = vk::VkImageCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
            imageType: vk::VK_IMAGE_TYPE_2D,
            format,
            extent: vk::VkExtent3D {
                width: WIDTH,
                height: HEIGHT,
                depth: 1,
            },
            mipLevels: LEVELS,
            arrayLayers: 1,
            samples: vk::VK_SAMPLE_COUNT_1_BIT,
            tiling: vk::VK_IMAGE_TILING_OPTIMAL,
            usage: super::super::sampled_texture::usage(true),
            sharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
            initialLayout: vk::VK_IMAGE_LAYOUT_UNDEFINED,
            ..Default::default()
        };
        let mut image = Image::default();
        let mut pool = ptr::null_mut();
        let mut command = ptr::null_mut();
        unsafe {
            // SAFETY: Live device, valid create infos and writable outputs; every handle is
            // destroyed after the device idles below.
            check(
                functions.create_image.unwrap()(
                    device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut image.handle,
                ),
                "test image",
            )
            .unwrap();
            complete_image_storage(
                &device,
                &mut image,
                format,
                vk::VK_IMAGE_ASPECT_COLOR_BIT.cast_unsigned(),
                LEVELS,
                ImageShape::Single,
            )
            .unwrap();
            let pool_info = vk::VkCommandPoolCreateInfo {
                sType: vk::VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
                queueFamilyIndex: device.adapter.queue_family,
                ..Default::default()
            };
            check(
                functions.create_command_pool.unwrap()(
                    device.handle,
                    &raw const pool_info,
                    ptr::null(),
                    &raw mut pool,
                ),
                "test command pool",
            )
            .unwrap();
            let allocation = vk::VkCommandBufferAllocateInfo {
                sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                commandPool: pool,
                level: vk::VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                commandBufferCount: 1,
                ..Default::default()
            };
            check(
                functions.allocate_command_buffers.unwrap()(
                    device.handle,
                    &raw const allocation,
                    &raw mut command,
                ),
                "test command buffer",
            )
            .unwrap();
            let begin = vk::VkCommandBufferBeginInfo {
                sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
                ..Default::default()
            };
            check(
                functions.begin_command_buffer.unwrap()(command, &raw const begin),
                "begin",
            )
            .unwrap();
            let all_levels = super::color_subresource_layers(LEVELS, 1);
            super::barriers(
                &device,
                command,
                &[super::image_barrier(
                    image.handle,
                    vk::VK_IMAGE_LAYOUT_UNDEFINED,
                    vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                    vk::VK_PIPELINE_STAGE_2_NONE,
                    vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                    vk::VK_ACCESS_2_NONE,
                    vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
                    all_levels,
                )],
            );
            let upload = region(0);
            let copy = vk::VkCopyBufferToImageInfo2 {
                sType: vk::VK_STRUCTURE_TYPE_COPY_BUFFER_TO_IMAGE_INFO_2,
                srcBuffer: buffer.handle,
                dstImage: image.handle,
                dstImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                regionCount: 1,
                pRegions: &raw const upload,
                ..Default::default()
            };
            functions.cmd_copy_buffer_to_image2.unwrap()(command, &raw const copy);
            record_generated_mips(&device, command, image.handle, [WIDTH, HEIGHT], LEVELS, 1);
            super::barriers(
                &device,
                command,
                &[super::image_barrier(
                    image.handle,
                    vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                    vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                    super::SHADER_STAGES,
                    vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                    vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
                    vk::VK_ACCESS_2_TRANSFER_READ_BIT,
                    all_levels,
                )],
            );
            let readback: Vec<_> = (1..LEVELS).map(region).collect();
            let copy = vk::VkCopyImageToBufferInfo2 {
                sType: vk::VK_STRUCTURE_TYPE_COPY_IMAGE_TO_BUFFER_INFO_2,
                srcImage: image.handle,
                srcImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                dstBuffer: buffer.handle,
                regionCount: u32::try_from(readback.len()).unwrap(),
                pRegions: readback.as_ptr(),
                ..Default::default()
            };
            functions.cmd_copy_image_to_buffer2.unwrap()(command, &raw const copy);
            check(functions.end_command_buffer.unwrap()(command), "end").unwrap();
            let command_info = vk::VkCommandBufferSubmitInfo {
                sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO,
                commandBuffer: command,
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
                    device.queue,
                    1,
                    &raw const submit,
                    ptr::null_mut(),
                ),
                "submit",
            )
            .unwrap();
            check(
                functions.device_wait_idle.unwrap()(device.handle),
                "wait for mips",
            )
            .unwrap();
        }
        let halves = buffer.halves();
        let texel = |offset: u64| -> Vec<f32> {
            let start = usize::try_from(offset / 2).unwrap();
            halves[start..start + 4]
                .iter()
                .copied()
                .map(half_to_f32)
                .collect()
        };
        assert_eq!(texel(OFFSETS[1]), [2.5; 4], "level 1, texel 0");
        assert_eq!(texel(OFFSETS[1] + 8), [4.5; 4], "level 1, texel 1");
        assert_eq!(texel(OFFSETS[2]), [3.5; 4], "level 2");
        unsafe {
            // SAFETY: The device is idle; the pool frees its command buffer.
            functions.destroy_command_pool.unwrap()(device.handle, pool, ptr::null());
            destroy_image_device(&device, image);
        }
        drop(buffer);
        assert_eq!(VALIDATION_MESSAGE_COUNT.load(Ordering::Relaxed), 0);
    }
}
