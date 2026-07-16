use super::{
    COMPUTE_IMAGE_HEIGHT, COMPUTE_IMAGE_MIP_LEVELS, COMPUTE_IMAGE_WIDTH, ProbeError, Renderer,
    STORAGE_VALUE_COUNT, check, color_mip_range, compute_readback_byte_len, descriptor_binding,
    mem, ptr, storage_buffer_byte_len, vk,
};

impl Renderer {
    pub(super) fn create_postprocess_resources(&mut self) -> Result<(), ProbeError> {
        let sampler_info = vk::VkSamplerCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,
            magFilter: vk::VK_FILTER_LINEAR,
            minFilter: vk::VK_FILTER_LINEAR,
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
                    &raw const sampler_info,
                    ptr::null(),
                    &raw mut self.post_sampler,
                )
            },
            "vkCreateSampler for post-processing",
        )?;
        let binding = descriptor_binding(
            0,
            vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
        );
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
                    &raw mut self.post_descriptor_set_layout,
                )
            },
            "vkCreateDescriptorSetLayout for post-processing",
        )?;
        let pool_size = vk::VkDescriptorPoolSize {
            type_: vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
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
                    &raw mut self.post_descriptor_pool,
                )
            },
            "vkCreateDescriptorPool for post-processing",
        )?;
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: self.post_descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const self.post_descriptor_set_layout,
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
                    &raw mut self.post_descriptor_set,
                )
            },
            "vkAllocateDescriptorSets for post-processing",
        )
    }

    pub(super) fn update_postprocess_descriptor(&self) {
        let image = vk::VkDescriptorImageInfo {
            sampler: self.post_sampler,
            imageView: self.offscreen.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        };
        let write = vk::VkWriteDescriptorSet {
            sType: vk::VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
            dstSet: self.post_descriptor_set,
            dstBinding: 0,
            descriptorCount: 1,
            descriptorType: vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            pImageInfo: &raw const image,
            ..Default::default()
        };
        // SAFETY: The descriptor set and referenced offscreen image/sampler are live.
        unsafe {
            self.device
                .functions
                .update_descriptor_sets
                .expect("loaded function")(
                self.device.handle, 1, &raw const write, 0, ptr::null()
            );
        }
    }

    pub(super) fn create_compute_readback_resources(&mut self) -> Result<(), ProbeError> {
        self.create_compute_image()?;
        self.compute_storage = self.create_buffer(
            storage_buffer_byte_len(),
            (vk::VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | vk::VK_BUFFER_USAGE_TRANSFER_SRC_BIT) as u32,
            vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            "compute storage",
        )?;
        self.compute_indirect = self.create_buffer(
            mem::size_of::<vk::VkDrawIndexedIndirectCommand>(),
            (vk::VK_BUFFER_USAGE_STORAGE_BUFFER_BIT
                | vk::VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT
                | vk::VK_BUFFER_USAGE_TRANSFER_SRC_BIT) as u32,
            vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            "compute-written indexed-indirect arguments",
        )?;
        self.compute_readback = self.create_buffer(
            compute_readback_byte_len(),
            vk::VK_BUFFER_USAGE_TRANSFER_DST_BIT as u32,
            (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
                as u32,
            "compute readback",
        )?;
        self.create_compute_descriptors()?;
        self.create_compute_pipeline()?;
        self.dispatch_compute_and_verify()?;
        println!(
            "Compute: {STORAGE_VALUE_COUNT} storage values, indexed-indirect arguments, and a {COMPUTE_IMAGE_MIP_LEVELS}-level {COMPUTE_IMAGE_WIDTH}x{COMPUTE_IMAGE_HEIGHT} storage image with exact base/tail readback"
        );
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn create_compute_image(&mut self) -> Result<(), ProbeError> {
        let mut properties = vk::VkFormatProperties::default();
        // SAFETY: The selected adapter is live and properties storage is writable.
        unsafe {
            self.device
                .instance
                .functions
                .get_physical_device_format_properties
                .expect("loaded function")(
                self.device.adapter.handle,
                vk::VK_FORMAT_R8G8B8A8_UNORM,
                &raw mut properties,
            );
        }
        let required_features = (vk::VK_FORMAT_FEATURE_STORAGE_IMAGE_BIT
            | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT
            | vk::VK_FORMAT_FEATURE_TRANSFER_SRC_BIT
            | vk::VK_FORMAT_FEATURE_BLIT_SRC_BIT
            | vk::VK_FORMAT_FEATURE_BLIT_DST_BIT) as u32;
        if properties.optimalTilingFeatures & required_features != required_features {
            return Err(ProbeError(
                "R8G8B8A8_UNORM lacks required optimal-tiled storage, sampled, transfer-source, or blit support"
                    .into(),
            ));
        }
        let info = vk::VkImageCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
            imageType: vk::VK_IMAGE_TYPE_2D,
            format: vk::VK_FORMAT_R8G8B8A8_UNORM,
            extent: vk::VkExtent3D {
                width: COMPUTE_IMAGE_WIDTH,
                height: COMPUTE_IMAGE_HEIGHT,
                depth: 1,
            },
            mipLevels: COMPUTE_IMAGE_MIP_LEVELS,
            arrayLayers: 1,
            samples: vk::VK_SAMPLE_COUNT_1_BIT,
            tiling: vk::VK_IMAGE_TILING_OPTIMAL,
            usage: (vk::VK_IMAGE_USAGE_STORAGE_BIT
                | vk::VK_IMAGE_USAGE_SAMPLED_BIT
                | vk::VK_IMAGE_USAGE_TRANSFER_SRC_BIT
                | vk::VK_IMAGE_USAGE_TRANSFER_DST_BIT) as u32,
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
                    &raw mut self.compute_image.handle,
                )
            },
            "vkCreateImage for compute storage image",
        )?;
        let mut requirements = vk::VkMemoryRequirements::default();
        // SAFETY: The image is live and requirements storage is writable.
        unsafe {
            self.device
                .functions
                .get_image_memory_requirements
                .expect("loaded function")(
                self.device.handle,
                self.compute_image.handle,
                &raw mut requirements,
            );
        }
        let memory_type = self
            .find_memory_type(
                requirements.memoryTypeBits,
                vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
            )
            .ok_or_else(|| {
                ProbeError("adapter exposes no device-local compute image memory type".into())
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
                    &raw mut self.compute_image.memory,
                )
            },
            "vkAllocateMemory for compute storage image",
        )?;
        check(
            // SAFETY: Image and allocation are compatible at offset zero.
            unsafe {
                self.device
                    .functions
                    .bind_image_memory
                    .expect("loaded function")(
                    self.device.handle,
                    self.compute_image.handle,
                    self.compute_image.memory,
                    0,
                )
            },
            "vkBindImageMemory for compute storage image",
        )?;
        self.compute_image.view = self.create_compute_image_view(0, 1, "compute storage image")?;
        self.compute_sampled_view =
            self.create_compute_image_view(0, COMPUTE_IMAGE_MIP_LEVELS, "compute image mip chain")?;
        Ok(())
    }

    pub(super) fn create_compute_image_view(
        &self,
        base_mip_level: u32,
        level_count: u32,
        description: &str,
    ) -> Result<vk::VkImageView, ProbeError> {
        let info = vk::VkImageViewCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
            image: self.compute_image.handle,
            viewType: vk::VK_IMAGE_VIEW_TYPE_2D,
            format: vk::VK_FORMAT_R8G8B8A8_UNORM,
            components: vk::VkComponentMapping {
                r: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                g: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                b: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
                a: vk::VK_COMPONENT_SWIZZLE_IDENTITY,
            },
            subresourceRange: color_mip_range(base_mip_level, level_count),
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
            &format!("vkCreateImageView for {description}"),
        )?;
        Ok(view)
    }

    pub(super) fn create_compute_descriptors(&mut self) -> Result<(), ProbeError> {
        let bindings = [
            vk::VkDescriptorSetLayoutBinding {
                binding: 0,
                descriptorType: vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                descriptorCount: 1,
                stageFlags: vk::VK_SHADER_STAGE_COMPUTE_BIT as u32,
                ..Default::default()
            },
            vk::VkDescriptorSetLayoutBinding {
                binding: 1,
                descriptorType: vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                descriptorCount: 1,
                stageFlags: vk::VK_SHADER_STAGE_COMPUTE_BIT as u32,
                ..Default::default()
            },
            vk::VkDescriptorSetLayoutBinding {
                binding: 2,
                descriptorType: vk::VK_DESCRIPTOR_TYPE_STORAGE_IMAGE,
                descriptorCount: 1,
                stageFlags: vk::VK_SHADER_STAGE_COMPUTE_BIT as u32,
                ..Default::default()
            },
        ];
        let layout_info = vk::VkDescriptorSetLayoutCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
            bindingCount: u32::try_from(bindings.len()).expect("compute binding count fits u32"),
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
                    &raw mut self.compute_descriptor_set_layout,
                )
            },
            "vkCreateDescriptorSetLayout for compute storage",
        )?;
        let pool_sizes = [
            vk::VkDescriptorPoolSize {
                type_: vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                descriptorCount: 2,
            },
            vk::VkDescriptorPoolSize {
                type_: vk::VK_DESCRIPTOR_TYPE_STORAGE_IMAGE,
                descriptorCount: 1,
            },
        ];
        let pool_info = vk::VkDescriptorPoolCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
            maxSets: 1,
            poolSizeCount: u32::try_from(pool_sizes.len())
                .expect("compute descriptor pool size count fits u32"),
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
                    &raw mut self.compute_descriptor_pool,
                )
            },
            "vkCreateDescriptorPool for compute storage",
        )?;
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: self.compute_descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const self.compute_descriptor_set_layout,
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
                    &raw mut self.compute_descriptor_set,
                )
            },
            "vkAllocateDescriptorSets for compute storage",
        )?;
        self.update_compute_descriptors();
        Ok(())
    }

    pub(super) fn update_compute_descriptors(&self) {
        let buffers = [
            vk::VkDescriptorBufferInfo {
                buffer: self.compute_storage.handle,
                offset: 0,
                range: u64::try_from(storage_buffer_byte_len())
                    .expect("storage buffer byte length fits u64"),
            },
            vk::VkDescriptorBufferInfo {
                buffer: self.compute_indirect.handle,
                offset: 0,
                range: u64::try_from(mem::size_of::<vk::VkDrawIndexedIndirectCommand>())
                    .expect("indirect command byte length fits u64"),
            },
        ];
        let image = vk::VkDescriptorImageInfo {
            sampler: ptr::null_mut(),
            imageView: self.compute_image.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_GENERAL,
        };
        let writes = [
            vk::VkWriteDescriptorSet {
                sType: vk::VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                dstSet: self.compute_descriptor_set,
                dstBinding: 0,
                descriptorCount: 1,
                descriptorType: vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                pBufferInfo: &raw const buffers[0],
                ..Default::default()
            },
            vk::VkWriteDescriptorSet {
                sType: vk::VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                dstSet: self.compute_descriptor_set,
                dstBinding: 1,
                descriptorCount: 1,
                descriptorType: vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                pBufferInfo: &raw const buffers[1],
                ..Default::default()
            },
            vk::VkWriteDescriptorSet {
                sType: vk::VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                dstSet: self.compute_descriptor_set,
                dstBinding: 2,
                descriptorCount: 1,
                descriptorType: vk::VK_DESCRIPTOR_TYPE_STORAGE_IMAGE,
                pImageInfo: &raw const image,
                ..Default::default()
            },
        ];
        // SAFETY: The descriptor set and referenced storage buffers/image are live.
        unsafe {
            self.device
                .functions
                .update_descriptor_sets
                .expect("loaded function")(
                self.device.handle,
                u32::try_from(writes.len()).expect("compute descriptor write count fits u32"),
                writes.as_ptr(),
                0,
                ptr::null(),
            );
        }
    }
}
