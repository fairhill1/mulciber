use super::{
    Instant, OFFSCREEN_FORMAT, ProbeError, Renderer, check, ptr, shader_stage, spirv_words,
    vertex_input_descriptions, vk,
};

impl Renderer {
    pub(super) fn create_shadow_pipeline(&mut self) -> Result<(), ProbeError> {
        let layout_info = vk::VkPipelineLayoutCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
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
                    &raw mut self.shadow_pipeline_layout,
                )
            },
            "vkCreatePipelineLayout for shadow pass",
        )?;
        let vertex = self.create_shader_module(include_bytes!("../shadow.vert.spv"))?;
        let result = self.create_shadow_graphics_pipeline(vertex);
        // SAFETY: Pipeline creation has finished reading the shader module.
        unsafe {
            self.device
                .functions
                .destroy_shader_module
                .expect("loaded function")(self.device.handle, vertex, ptr::null());
        }
        result
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn create_shadow_graphics_pipeline(
        &mut self,
        vertex: vk::VkShaderModule,
    ) -> Result<(), ProbeError> {
        let stage = shader_stage(vk::VK_SHADER_STAGE_VERTEX_BIT, vertex);
        let (binding, attributes) = vertex_input_descriptions();
        let vertex_input = vk::VkPipelineVertexInputStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
            vertexBindingDescriptionCount: 1,
            pVertexBindingDescriptions: &raw const binding,
            vertexAttributeDescriptionCount: 1,
            pVertexAttributeDescriptions: attributes.as_ptr(),
            ..Default::default()
        };
        let input_assembly = vk::VkPipelineInputAssemblyStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
            topology: vk::VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,
            ..Default::default()
        };
        let viewport = vk::VkPipelineViewportStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
            viewportCount: 1,
            scissorCount: 1,
            ..Default::default()
        };
        let rasterization = vk::VkPipelineRasterizationStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
            polygonMode: vk::VK_POLYGON_MODE_FILL,
            cullMode: vk::VK_CULL_MODE_NONE as u32,
            frontFace: vk::VK_FRONT_FACE_CLOCKWISE,
            depthBiasEnable: vk::VK_TRUE,
            depthBiasConstantFactor: 1.25,
            depthBiasSlopeFactor: 1.75,
            lineWidth: 1.0,
            ..Default::default()
        };
        let multisample = vk::VkPipelineMultisampleStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
            rasterizationSamples: vk::VK_SAMPLE_COUNT_1_BIT,
            ..Default::default()
        };
        let depth_stencil = vk::VkPipelineDepthStencilStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
            depthTestEnable: vk::VK_TRUE,
            depthWriteEnable: vk::VK_TRUE,
            depthCompareOp: vk::VK_COMPARE_OP_LESS,
            minDepthBounds: 0.0,
            maxDepthBounds: 1.0,
            ..Default::default()
        };
        let blend = vk::VkPipelineColorBlendStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
            ..Default::default()
        };
        let dynamic_states = [vk::VK_DYNAMIC_STATE_VIEWPORT, vk::VK_DYNAMIC_STATE_SCISSOR];
        let dynamic = vk::VkPipelineDynamicStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
            dynamicStateCount: u32::try_from(dynamic_states.len())
                .expect("dynamic state count fits u32"),
            pDynamicStates: dynamic_states.as_ptr(),
            ..Default::default()
        };
        let rendering = vk::VkPipelineRenderingCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
            depthAttachmentFormat: self.depth_format,
            ..Default::default()
        };
        let mut feedback = vk::VkPipelineCreationFeedback::default();
        let feedback_info = vk::VkPipelineCreationFeedbackCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_CREATION_FEEDBACK_CREATE_INFO,
            pNext: (&raw const rendering).cast(),
            pPipelineCreationFeedback: &raw mut feedback,
            ..Default::default()
        };
        let info = vk::VkGraphicsPipelineCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
            pNext: (&raw const feedback_info).cast(),
            flags: self.pipeline_create_flags(),
            stageCount: 1,
            pStages: &raw const stage,
            pVertexInputState: &raw const vertex_input,
            pInputAssemblyState: &raw const input_assembly,
            pViewportState: &raw const viewport,
            pRasterizationState: &raw const rasterization,
            pMultisampleState: &raw const multisample,
            pDepthStencilState: &raw const depth_stencil,
            pColorBlendState: &raw const blend,
            pDynamicState: &raw const dynamic,
            layout: self.shadow_pipeline_layout,
            basePipelineIndex: -1,
            ..Default::default()
        };
        let started = Instant::now();
        let create_pipelines = self
            .device
            .functions
            .create_graphics_pipelines
            .expect("loaded function");
        // SAFETY: All pipeline state pointers remain live and output storage is writable.
        let result = unsafe {
            create_pipelines(
                self.device.handle,
                self.pipeline_cache.handle,
                1,
                &raw const info,
                ptr::null(),
                &raw mut self.shadow_pipeline,
            )
        };
        self.check_pipeline_feedback("shadow", result, feedback, started.elapsed())
    }

    pub(super) fn create_pipeline(&mut self) -> Result<(), ProbeError> {
        let layout_info = vk::VkPipelineLayoutCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
            setLayoutCount: 1,
            pSetLayouts: &raw const self.descriptor_set_layout,
            ..Default::default()
        };
        // SAFETY: Device/create info are valid and output is writable.
        check(
            unsafe {
                self.device
                    .functions
                    .create_pipeline_layout
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const layout_info,
                    ptr::null(),
                    &raw mut self.pipeline_layout,
                )
            },
            "vkCreatePipelineLayout",
        )?;

        let vertex = self.create_shader_module(include_bytes!("../triangle.vert.spv"))?;
        let fragment = match self.create_shader_module(include_bytes!("../triangle.frag.spv")) {
            Ok(module) => module,
            Err(error) => {
                // SAFETY: Vertex module is live and unused.
                unsafe {
                    self.device
                        .functions
                        .destroy_shader_module
                        .expect("loaded function")(
                        self.device.handle, vertex, ptr::null()
                    );
                }
                return Err(error);
            }
        };
        let result = self.create_graphics_pipeline(vertex, fragment);
        // SAFETY: Pipeline creation has finished reading both modules.
        unsafe {
            for module in [vertex, fragment] {
                self.device
                    .functions
                    .destroy_shader_module
                    .expect("loaded function")(
                    self.device.handle, module, ptr::null()
                );
            }
        }
        result?;
        self.create_post_pipeline()
    }

    pub(super) fn create_shader_module(
        &self,
        bytes: &[u8],
    ) -> Result<vk::VkShaderModule, ProbeError> {
        let words = spirv_words(bytes)?;
        let info = vk::VkShaderModuleCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
            codeSize: bytes.len(),
            pCode: words.as_ptr(),
            ..Default::default()
        };
        let mut module = ptr::null_mut();
        // SAFETY: SPIR-V words are aligned/live for the call and output is writable.
        check(
            unsafe {
                self.device
                    .functions
                    .create_shader_module
                    .expect("loaded function")(
                    self.device.handle,
                    &raw const info,
                    ptr::null(),
                    &raw mut module,
                )
            },
            "vkCreateShaderModule",
        )?;
        Ok(module)
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn create_graphics_pipeline(
        &mut self,
        vertex: vk::VkShaderModule,
        fragment: vk::VkShaderModule,
    ) -> Result<(), ProbeError> {
        let stages = [
            shader_stage(vk::VK_SHADER_STAGE_VERTEX_BIT, vertex),
            shader_stage(vk::VK_SHADER_STAGE_FRAGMENT_BIT, fragment),
        ];
        let (binding, attributes) = vertex_input_descriptions();
        let vertex_input = vk::VkPipelineVertexInputStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
            vertexBindingDescriptionCount: 1,
            pVertexBindingDescriptions: &raw const binding,
            vertexAttributeDescriptionCount: u32::try_from(attributes.len())
                .expect("vertex attribute count fits u32"),
            pVertexAttributeDescriptions: attributes.as_ptr(),
            ..Default::default()
        };
        let input_assembly = vk::VkPipelineInputAssemblyStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
            topology: vk::VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,
            ..Default::default()
        };
        let viewport = vk::VkPipelineViewportStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
            viewportCount: 1,
            scissorCount: 1,
            ..Default::default()
        };
        let rasterization = vk::VkPipelineRasterizationStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
            polygonMode: vk::VK_POLYGON_MODE_FILL,
            cullMode: vk::VK_CULL_MODE_NONE as u32,
            frontFace: vk::VK_FRONT_FACE_CLOCKWISE,
            lineWidth: 1.0,
            ..Default::default()
        };
        let multisample = vk::VkPipelineMultisampleStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
            rasterizationSamples: self.device.adapter.sample_count,
            ..Default::default()
        };
        let depth_stencil = vk::VkPipelineDepthStencilStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
            depthTestEnable: vk::VK_TRUE,
            depthWriteEnable: vk::VK_TRUE,
            depthCompareOp: vk::VK_COMPARE_OP_LESS,
            minDepthBounds: 0.0,
            maxDepthBounds: 1.0,
            ..Default::default()
        };
        let blend_attachment = vk::VkPipelineColorBlendAttachmentState {
            colorWriteMask: (vk::VK_COLOR_COMPONENT_R_BIT
                | vk::VK_COLOR_COMPONENT_G_BIT
                | vk::VK_COLOR_COMPONENT_B_BIT
                | vk::VK_COLOR_COMPONENT_A_BIT) as u32,
            ..Default::default()
        };
        let blend = vk::VkPipelineColorBlendStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
            attachmentCount: 1,
            pAttachments: &raw const blend_attachment,
            ..Default::default()
        };
        let dynamic_states = [vk::VK_DYNAMIC_STATE_VIEWPORT, vk::VK_DYNAMIC_STATE_SCISSOR];
        let dynamic = vk::VkPipelineDynamicStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
            dynamicStateCount: 2,
            pDynamicStates: dynamic_states.as_ptr(),
            ..Default::default()
        };
        let color_format = OFFSCREEN_FORMAT;
        let rendering = vk::VkPipelineRenderingCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
            colorAttachmentCount: 1,
            pColorAttachmentFormats: &raw const color_format,
            depthAttachmentFormat: self.depth_format,
            ..Default::default()
        };
        let mut feedback = vk::VkPipelineCreationFeedback::default();
        let feedback_info = vk::VkPipelineCreationFeedbackCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_CREATION_FEEDBACK_CREATE_INFO,
            pNext: (&raw const rendering).cast(),
            pPipelineCreationFeedback: &raw mut feedback,
            ..Default::default()
        };
        let info = vk::VkGraphicsPipelineCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
            pNext: (&raw const feedback_info).cast(),
            flags: self.pipeline_create_flags(),
            stageCount: 2,
            pStages: stages.as_ptr(),
            pVertexInputState: &raw const vertex_input,
            pInputAssemblyState: &raw const input_assembly,
            pViewportState: &raw const viewport,
            pRasterizationState: &raw const rasterization,
            pMultisampleState: &raw const multisample,
            pDepthStencilState: &raw const depth_stencil,
            pColorBlendState: &raw const blend,
            pDynamicState: &raw const dynamic,
            layout: self.pipeline_layout,
            basePipelineIndex: -1,
            ..Default::default()
        };
        let started = Instant::now();
        let create_pipelines = self
            .device
            .functions
            .create_graphics_pipelines
            .expect("loaded function");
        // SAFETY: All pipeline state pointers remain live and output is writable.
        let result = unsafe {
            create_pipelines(
                self.device.handle,
                self.pipeline_cache.handle,
                1,
                &raw const info,
                ptr::null(),
                &raw mut self.pipeline,
            )
        };
        let name = if self.device.adapter.sample_count == vk::VK_SAMPLE_COUNT_4_BIT {
            "scene-4x"
        } else {
            "scene-1x"
        };
        self.check_pipeline_feedback(name, result, feedback, started.elapsed())
    }

    pub(super) fn create_post_pipeline(&mut self) -> Result<(), ProbeError> {
        let layout_info = vk::VkPipelineLayoutCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
            setLayoutCount: 1,
            pSetLayouts: &raw const self.post_descriptor_set_layout,
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
                    &raw mut self.post_pipeline_layout,
                )
            },
            "vkCreatePipelineLayout for post-processing",
        )?;
        let vertex = self.create_shader_module(include_bytes!("../post.vert.spv"))?;
        let fragment = match self.create_shader_module(include_bytes!("../post.frag.spv")) {
            Ok(module) => module,
            Err(error) => {
                // SAFETY: Vertex module is live and unused.
                unsafe {
                    self.device
                        .functions
                        .destroy_shader_module
                        .expect("loaded function")(
                        self.device.handle, vertex, ptr::null()
                    );
                }
                return Err(error);
            }
        };
        let result = self.create_post_graphics_pipeline(vertex, fragment);
        // SAFETY: Pipeline creation has finished reading both modules.
        unsafe {
            for module in [vertex, fragment] {
                self.device
                    .functions
                    .destroy_shader_module
                    .expect("loaded function")(
                    self.device.handle, module, ptr::null()
                );
            }
        }
        result
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn create_post_graphics_pipeline(
        &mut self,
        vertex: vk::VkShaderModule,
        fragment: vk::VkShaderModule,
    ) -> Result<(), ProbeError> {
        let stages = [
            shader_stage(vk::VK_SHADER_STAGE_VERTEX_BIT, vertex),
            shader_stage(vk::VK_SHADER_STAGE_FRAGMENT_BIT, fragment),
        ];
        let vertex_input = vk::VkPipelineVertexInputStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
            ..Default::default()
        };
        let input_assembly = vk::VkPipelineInputAssemblyStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
            topology: vk::VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,
            ..Default::default()
        };
        let viewport = vk::VkPipelineViewportStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
            viewportCount: 1,
            scissorCount: 1,
            ..Default::default()
        };
        let rasterization = vk::VkPipelineRasterizationStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
            polygonMode: vk::VK_POLYGON_MODE_FILL,
            cullMode: vk::VK_CULL_MODE_NONE as u32,
            frontFace: vk::VK_FRONT_FACE_CLOCKWISE,
            lineWidth: 1.0,
            ..Default::default()
        };
        let multisample = vk::VkPipelineMultisampleStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
            rasterizationSamples: vk::VK_SAMPLE_COUNT_1_BIT,
            ..Default::default()
        };
        let blend_attachment = vk::VkPipelineColorBlendAttachmentState {
            colorWriteMask: (vk::VK_COLOR_COMPONENT_R_BIT
                | vk::VK_COLOR_COMPONENT_G_BIT
                | vk::VK_COLOR_COMPONENT_B_BIT
                | vk::VK_COLOR_COMPONENT_A_BIT) as u32,
            ..Default::default()
        };
        let blend = vk::VkPipelineColorBlendStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
            attachmentCount: 1,
            pAttachments: &raw const blend_attachment,
            ..Default::default()
        };
        let dynamic_states = [vk::VK_DYNAMIC_STATE_VIEWPORT, vk::VK_DYNAMIC_STATE_SCISSOR];
        let dynamic = vk::VkPipelineDynamicStateCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
            dynamicStateCount: 2,
            pDynamicStates: dynamic_states.as_ptr(),
            ..Default::default()
        };
        let rendering = vk::VkPipelineRenderingCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
            colorAttachmentCount: 1,
            pColorAttachmentFormats: &raw const self.format,
            ..Default::default()
        };
        let mut feedback = vk::VkPipelineCreationFeedback::default();
        let feedback_info = vk::VkPipelineCreationFeedbackCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_CREATION_FEEDBACK_CREATE_INFO,
            pNext: (&raw const rendering).cast(),
            pPipelineCreationFeedback: &raw mut feedback,
            ..Default::default()
        };
        let info = vk::VkGraphicsPipelineCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
            pNext: (&raw const feedback_info).cast(),
            flags: self.pipeline_create_flags(),
            stageCount: 2,
            pStages: stages.as_ptr(),
            pVertexInputState: &raw const vertex_input,
            pInputAssemblyState: &raw const input_assembly,
            pViewportState: &raw const viewport,
            pRasterizationState: &raw const rasterization,
            pMultisampleState: &raw const multisample,
            pColorBlendState: &raw const blend,
            pDynamicState: &raw const dynamic,
            layout: self.post_pipeline_layout,
            basePipelineIndex: -1,
            ..Default::default()
        };
        let started = Instant::now();
        let create_pipelines = self
            .device
            .functions
            .create_graphics_pipelines
            .expect("loaded function");
        // SAFETY: All pipeline state pointers remain live and output storage is writable.
        let result = unsafe {
            create_pipelines(
                self.device.handle,
                self.pipeline_cache.handle,
                1,
                &raw const info,
                ptr::null(),
                &raw mut self.post_pipeline,
            )
        };
        self.check_pipeline_feedback("post", result, feedback, started.elapsed())
    }
}
