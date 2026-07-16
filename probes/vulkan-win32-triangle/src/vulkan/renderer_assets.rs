use super::{
    FRAME_SLOT_COUNT, FrameUniform, GpuBuffer, ProbeError, Renderer, SHADOW_MAP_SIZE,
    TEXTURE_HEIGHT, TEXTURE_WIDTH, TRIANGLE_INDICES, TRIANGLE_VERTICES, UniformBuffer, check,
    color_subresource_range, depth_subresource_range, descriptor_binding, find_memory_type, mem,
    ptr, slice_bytes, vk,
};

impl Renderer {
    pub(super) fn create_geometry_buffers(&mut self) -> Result<(), ProbeError> {
        // SAFETY: `Vertex` is `repr(C)`, contains only initialized `f32` arrays, and its tested
        // 28-byte layout has no padding.
        let vertex_bytes = unsafe { slice_bytes(&TRIANGLE_VERTICES) };
        // SAFETY: `u16` has no padding and every element is initialized.
        let index_bytes = unsafe { slice_bytes(&TRIANGLE_INDICES) };
        let mut vertex_staging = self.create_staging_buffer(vertex_bytes, "vertex")?;
        let mut index_staging = match self.create_staging_buffer(index_bytes, "index") {
            Ok(buffer) => buffer,
            Err(error) => {
                // SAFETY: No commands reference this buffer because recording has not started.
                unsafe { self.destroy_buffer(&mut vertex_staging) };
                return Err(error);
            }
        };

        let upload = self.create_device_geometry_and_upload(
            &vertex_staging,
            vertex_bytes.len(),
            &index_staging,
            index_bytes.len(),
        );
        if upload.is_err() {
            // An error after queue submission can leave staging buffers referenced by unfinished
            // work. The startup failure path favors orderly cleanup over latency.
            // SAFETY: The queue belongs to this device; a device-lost error still permits cleanup.
            let _ = unsafe {
                self.device
                    .functions
                    .device_wait_idle
                    .expect("loaded function")(self.device.handle)
            };
        }
        // SAFETY: Successful upload waits for its fence; the error path attempted device idle.
        unsafe {
            self.destroy_buffer(&mut vertex_staging);
            self.destroy_buffer(&mut index_staging);
        }
        upload?;
        println!("Geometry: device-local vertex/index buffers uploaded through staging");
        Ok(())
    }

    pub(super) fn create_staging_buffer(
        &self,
        bytes: &[u8],
        description: &str,
    ) -> Result<GpuBuffer, ProbeError> {
        let required_flags = (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
            | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) as u32;
        let mut buffer = self.create_buffer(
            bytes.len(),
            vk::VK_BUFFER_USAGE_TRANSFER_SRC_BIT as u32,
            required_flags,
            &format!("{description} staging"),
        )?;
        if let Err(error) = self.write_buffer(&buffer, bytes, description) {
            // SAFETY: The buffer has not been submitted to the GPU.
            unsafe { self.destroy_buffer(&mut buffer) };
            return Err(error);
        }
        Ok(buffer)
    }

    pub(super) fn create_device_geometry_and_upload(
        &mut self,
        vertex_staging: &GpuBuffer,
        vertex_size: usize,
        index_staging: &GpuBuffer,
        index_size: usize,
    ) -> Result<(), ProbeError> {
        self.vertex_buffer = self.create_buffer(
            vertex_size,
            (vk::VK_BUFFER_USAGE_TRANSFER_DST_BIT | vk::VK_BUFFER_USAGE_VERTEX_BUFFER_BIT) as u32,
            vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            "device-local vertex",
        )?;
        self.index_buffer = self.create_buffer(
            index_size,
            (vk::VK_BUFFER_USAGE_TRANSFER_DST_BIT | vk::VK_BUFFER_USAGE_INDEX_BUFFER_BIT) as u32,
            vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            "device-local index",
        )?;
        self.upload_geometry(
            vertex_staging,
            u64::try_from(vertex_size).expect("vertex byte length fits u64"),
            index_staging,
            u64::try_from(index_size).expect("index byte length fits u64"),
        )
    }

    pub(super) fn create_uniform_buffers(&mut self) -> Result<(), ProbeError> {
        let required_flags = (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
            | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) as u32;
        for slot in 0..FRAME_SLOT_COUNT {
            let mut buffer = self.create_buffer(
                mem::size_of::<FrameUniform>(),
                vk::VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT as u32,
                required_flags,
                &format!("frame-slot {slot} uniform"),
            )?;
            let mut mapped = ptr::null_mut();
            let map = check(
                // SAFETY: The allocation is host-visible and the whole uniform range is valid.
                unsafe {
                    self.device.functions.map_memory.expect("loaded function")(
                        self.device.handle,
                        buffer.memory,
                        0,
                        u64::try_from(mem::size_of::<FrameUniform>())
                            .expect("uniform byte length fits u64"),
                        0,
                        &raw mut mapped,
                    )
                },
                &format!("vkMapMemory for frame-slot {slot} uniform"),
            );
            if let Err(error) = map {
                // SAFETY: Mapping failed and the buffer has never been submitted.
                unsafe { self.destroy_buffer(&mut buffer) };
                return Err(error);
            }
            self.uniform_buffers.push(UniformBuffer { buffer, mapped });
        }
        println!(
            "Uniforms: {FRAME_SLOT_COUNT} persistently mapped frame slots with transform/time data"
        );
        Ok(())
    }

    pub(super) fn create_texture_resources(&mut self) -> Result<(), ProbeError> {
        let texture_path = self.device.adapter.texture.path;
        let upload_bytes = texture_path.upload_bytes();
        let mut staging = self.create_staging_buffer(upload_bytes, "texture")?;
        let readback_result = self.create_buffer(
            upload_bytes.len(),
            vk::VK_BUFFER_USAGE_TRANSFER_DST_BIT as u32,
            (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
                as u32,
            "texture readback",
        );
        let mut readback = match readback_result {
            Ok(buffer) => buffer,
            Err(error) => {
                // SAFETY: The staging buffer has not been submitted.
                unsafe { self.destroy_buffer(&mut staging) };
                return Err(error);
            }
        };
        let result = self.create_texture_and_upload(&staging, &readback);
        if result.is_err() {
            // SAFETY: If submission started, waiting idle prevents the staging buffer from being
            // destroyed while referenced by the queue. The same applies to the readback buffer.
            let _ = unsafe {
                self.device
                    .functions
                    .device_wait_idle
                    .expect("loaded function")(self.device.handle)
            };
        }
        // SAFETY: Successful upload waited for completion; the error path attempted device idle.
        unsafe {
            self.destroy_buffer(&mut staging);
            self.destroy_buffer(&mut readback);
        }
        result?;
        self.create_texture_sampler()?;
        println!(
            "Texture: device-local {TEXTURE_WIDTH}x{TEXTURE_HEIGHT} {} image uploaded and sampled",
            texture_path.diagnostic_name()
        );
        Ok(())
    }

    pub(super) fn create_texture_and_upload(
        &mut self,
        staging: &GpuBuffer,
        readback: &GpuBuffer,
    ) -> Result<(), ProbeError> {
        let texture_path = self.device.adapter.texture.path;
        let info = vk::VkImageCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
            imageType: vk::VK_IMAGE_TYPE_2D,
            format: texture_path.format(),
            extent: vk::VkExtent3D {
                width: TEXTURE_WIDTH,
                height: TEXTURE_HEIGHT,
                depth: 1,
            },
            mipLevels: 1,
            arrayLayers: 1,
            samples: vk::VK_SAMPLE_COUNT_1_BIT,
            tiling: vk::VK_IMAGE_TILING_OPTIMAL,
            usage: (vk::VK_IMAGE_USAGE_TRANSFER_SRC_BIT
                | vk::VK_IMAGE_USAGE_TRANSFER_DST_BIT
                | vk::VK_IMAGE_USAGE_SAMPLED_BIT) as u32,
            sharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
            initialLayout: vk::VK_IMAGE_LAYOUT_UNDEFINED,
            ..Default::default()
        };
        check(
            // SAFETY: Device/create info are valid and owned output storage is writable.
            unsafe {
                self.device.functions.create_image.expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut self.texture.handle,
                )
            },
            "vkCreateImage for sampled texture",
        )?;
        let mut requirements = vk::VkMemoryRequirements::default();
        // SAFETY: The image is live and requirements storage is writable.
        unsafe {
            self.device
                .functions
                .get_image_memory_requirements
                .expect("loaded function")(
                self.device.handle,
                self.texture.handle,
                &raw mut requirements,
            );
        }
        let memory_type = self
            .find_memory_type(
                requirements.memoryTypeBits,
                vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            )
            .ok_or_else(|| {
                ProbeError("adapter exposes no device-local texture memory type".into())
            })?;
        let allocation = vk::VkMemoryAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
            allocationSize: requirements.size,
            memoryTypeIndex: memory_type,
            ..Default::default()
        };
        check(
            // SAFETY: Allocation info and output memory storage are valid.
            unsafe {
                self.device
                    .functions
                    .allocate_memory
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const allocation,
                    ptr::null(),
                    &raw mut self.texture.memory,
                )
            },
            "vkAllocateMemory for sampled texture",
        )?;
        check(
            // SAFETY: Image and allocation are compatible at offset zero.
            unsafe {
                self.device
                    .functions
                    .bind_image_memory
                    .expect("loaded function")(
                    self.device.handle,
                    self.texture.handle,
                    self.texture.memory,
                    0,
                )
            },
            "vkBindImageMemory for sampled texture",
        )?;
        self.upload_texture(staging, readback)?;
        self.verify_texture_readback(readback)?;
        self.texture.view = self.create_texture_view()?;
        Ok(())
    }

    pub(super) fn find_memory_type(
        &self,
        compatible_bits: u32,
        required_flags: u32,
    ) -> Option<u32> {
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
        find_memory_type(&properties, compatible_bits, required_flags)
    }

    pub(super) fn create_texture_view(&self) -> Result<vk::VkImageView, ProbeError> {
        let info = vk::VkImageViewCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
            image: self.texture.handle,
            viewType: vk::VK_IMAGE_VIEW_TYPE_2D,
            format: self.device.adapter.texture.path.format(),
            components: vk::VkComponentMapping {
                r: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                g: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                b: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                a: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
            },
            subresourceRange: color_subresource_range(),
            ..Default::default()
        };
        let mut view = ptr::null_mut();
        check(
            // SAFETY: Image/create info are valid and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_image_view
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut view,
                )
            },
            "vkCreateImageView for sampled texture",
        )?;
        Ok(view)
    }

    pub(super) fn create_texture_sampler(&mut self) -> Result<(), ProbeError> {
        let info = vk::VkSamplerCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,
            magFilter: vk::VK_FILTER_NEAREST,
            minFilter: vk::VK_FILTER_NEAREST,
            mipmapMode: vk::VK_SAMPLER_MIPMAP_MODE_NEAREST,
            addressModeU: vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            addressModeV: vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            addressModeW: vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            maxAnisotropy: 1.0,
            maxLod: 3.0,
            borderColor: vk::VK_BORDER_COLOR_INT_OPAQUE_BLACK,
            ..Default::default()
        };
        check(
            // SAFETY: Device/create info are valid and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_sampler
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut self.texture_sampler,
                )
            },
            "vkCreateSampler",
        )
    }

    pub(super) fn create_shadow_resources(&mut self) -> Result<(), ProbeError> {
        self.create_shadow_map()?;
        self.create_shadow_sampler()?;
        self.create_shadow_pipeline()
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn create_shadow_map(&mut self) -> Result<(), ProbeError> {
        let info = vk::VkImageCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
            imageType: vk::VK_IMAGE_TYPE_2D,
            format: self.depth_format,
            extent: vk::VkExtent3D {
                width: SHADOW_MAP_SIZE,
                height: SHADOW_MAP_SIZE,
                depth: 1,
            },
            mipLevels: 1,
            arrayLayers: 1,
            samples: vk::VK_SAMPLE_COUNT_1_BIT,
            tiling: vk::VK_IMAGE_TILING_OPTIMAL,
            usage: (vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT
                | vk::VK_IMAGE_USAGE_SAMPLED_BIT) as u32,
            sharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
            initialLayout: vk::VK_IMAGE_LAYOUT_UNDEFINED,
            ..Default::default()
        };
        check(
            // SAFETY: Device/create info are valid and output storage is writable.
            unsafe {
                self.device.functions.create_image.expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut self.shadow_map.handle,
                )
            },
            "vkCreateImage for shadow map",
        )?;
        let mut requirements = vk::VkMemoryRequirements::default();
        // SAFETY: The shadow image is live and requirements storage is writable.
        unsafe {
            self.device
                .functions
                .get_image_memory_requirements
                .expect("loaded function")(
                self.device.handle,
                self.shadow_map.handle,
                &raw mut requirements,
            );
        }
        let memory_type = self
            .find_memory_type(
                requirements.memoryTypeBits,
                vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            )
            .ok_or_else(|| {
                ProbeError("adapter exposes no device-local shadow-map memory type".into())
            })?;
        let allocation = vk::VkMemoryAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
            allocationSize: requirements.size,
            memoryTypeIndex: memory_type,
            ..Default::default()
        };
        check(
            // SAFETY: Allocation info and output memory storage are valid.
            unsafe {
                self.device
                    .functions
                    .allocate_memory
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const allocation,
                    ptr::null(),
                    &raw mut self.shadow_map.memory,
                )
            },
            "vkAllocateMemory for shadow map",
        )?;
        check(
            // SAFETY: Image and allocation are compatible at offset zero.
            unsafe {
                self.device
                    .functions
                    .bind_image_memory
                    .expect("loaded function")(
                    self.device.handle,
                    self.shadow_map.handle,
                    self.shadow_map.memory,
                    0,
                )
            },
            "vkBindImageMemory for shadow map",
        )?;
        let view_info = vk::VkImageViewCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
            image: self.shadow_map.handle,
            viewType: vk::VK_IMAGE_VIEW_TYPE_2D,
            format: self.depth_format,
            components: vk::VkComponentMapping {
                r: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                g: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                b: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                a: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
            },
            subresourceRange: depth_subresource_range(),
            ..Default::default()
        };
        check(
            // SAFETY: The shadow image/create info are valid and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_image_view
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const view_info,
                    ptr::null(),
                    &raw mut self.shadow_map.view,
                )
            },
            "vkCreateImageView for shadow map",
        )
    }

    pub(super) fn create_shadow_sampler(&mut self) -> Result<(), ProbeError> {
        let info = vk::VkSamplerCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,
            magFilter: vk::VK_FILTER_NEAREST,
            minFilter: vk::VK_FILTER_NEAREST,
            mipmapMode: vk::VK_SAMPLER_MIPMAP_MODE_NEAREST,
            addressModeU: vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
            addressModeV: vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
            addressModeW: vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
            maxAnisotropy: 1.0,
            maxLod: 0.0,
            borderColor: vk::VK_BORDER_COLOR_INT_OPAQUE_BLACK,
            ..Default::default()
        };
        check(
            // SAFETY: Device/create info are valid and output storage is writable.
            unsafe {
                self.device
                    .functions
                    .create_sampler
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut self.shadow_sampler,
                )
            },
            "vkCreateSampler for shadow map",
        )
    }

    pub(super) fn create_texture_descriptors(&mut self) -> Result<(), ProbeError> {
        let bindings = [
            descriptor_binding(
                0,
                vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
                vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
            ),
            descriptor_binding(
                1,
                vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
                (vk::VK_SHADER_STAGE_VERTEX_BIT | vk::VK_SHADER_STAGE_FRAGMENT_BIT) as u32,
            ),
            descriptor_binding(
                2,
                vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
                vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
            ),
            descriptor_binding(
                3,
                vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
                vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
            ),
        ];
        let layout_info = vk::VkDescriptorSetLayoutCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
            bindingCount: u32::try_from(bindings.len()).expect("descriptor binding count fits u32"),
            pBindings: bindings.as_ptr(),
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
                    &raw mut self.descriptor_set_layout,
                )
            },
            "vkCreateDescriptorSetLayout",
        )?;
        let descriptor_count = u32::try_from(FRAME_SLOT_COUNT).expect("frame slot count fits u32");
        let pool_sizes = [
            vk::VkDescriptorPoolSize {
                type_: vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
                descriptorCount: descriptor_count * 3,
            },
            vk::VkDescriptorPoolSize {
                type_: vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
                descriptorCount: descriptor_count,
            },
        ];
        let pool_info = vk::VkDescriptorPoolCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
            maxSets: descriptor_count,
            poolSizeCount: u32::try_from(pool_sizes.len())
                .expect("descriptor pool size count fits u32"),
            pPoolSizes: pool_sizes.as_ptr(),
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
                    &raw mut self.descriptor_pool,
                )
            },
            "vkCreateDescriptorPool",
        )?;
        let layouts = [self.descriptor_set_layout; FRAME_SLOT_COUNT];
        self.descriptor_sets = vec![ptr::null_mut(); FRAME_SLOT_COUNT];
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: self.descriptor_pool,
            descriptorSetCount: descriptor_count,
            pSetLayouts: layouts.as_ptr(),
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
                    self.descriptor_sets.as_mut_ptr(),
                )
            },
            "vkAllocateDescriptorSets",
        )?;
        self.update_frame_descriptors();
        Ok(())
    }

    pub(super) fn update_frame_descriptors(&self) {
        for (&descriptor_set, uniform) in self.descriptor_sets.iter().zip(&self.uniform_buffers) {
            let image = vk::VkDescriptorImageInfo {
                sampler: self.texture_sampler,
                imageView: self.texture.view,
                imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            };
            let generated_image = vk::VkDescriptorImageInfo {
                sampler: self.texture_sampler,
                imageView: self.compute_sampled_view,
                imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            };
            let shadow_map = vk::VkDescriptorImageInfo {
                sampler: self.shadow_sampler,
                imageView: self.shadow_map.view,
                imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            };
            let buffer = vk::VkDescriptorBufferInfo {
                buffer: uniform.buffer.handle,
                offset: 0,
                range: u64::try_from(mem::size_of::<FrameUniform>())
                    .expect("uniform byte length fits u64"),
            };
            let writes = [
                vk::VkWriteDescriptorSet {
                    sType: vk::VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                    dstSet: descriptor_set,
                    dstBinding: 0,
                    descriptorCount: 1,
                    descriptorType: vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
                    pImageInfo: &raw const image,
                    ..Default::default()
                },
                vk::VkWriteDescriptorSet {
                    sType: vk::VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                    dstSet: descriptor_set,
                    dstBinding: 1,
                    descriptorCount: 1,
                    descriptorType: vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
                    pBufferInfo: &raw const buffer,
                    ..Default::default()
                },
                vk::VkWriteDescriptorSet {
                    sType: vk::VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                    dstSet: descriptor_set,
                    dstBinding: 2,
                    descriptorCount: 1,
                    descriptorType: vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
                    pImageInfo: &raw const generated_image,
                    ..Default::default()
                },
                vk::VkWriteDescriptorSet {
                    sType: vk::VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                    dstSet: descriptor_set,
                    dstBinding: 3,
                    descriptorCount: 1,
                    descriptorType: vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
                    pImageInfo: &raw const shadow_map,
                    ..Default::default()
                },
            ];
            // SAFETY: The set and referenced image/sampler/buffer are live for this update.
            unsafe {
                self.device
                    .functions
                    .update_descriptor_sets
                    .expect("loaded function")(
                    self.device.handle,
                    u32::try_from(writes.len()).expect("descriptor write count fits u32"),
                    writes.as_ptr(),
                    0,
                    ptr::null(),
                );
            }
        }
    }
}
