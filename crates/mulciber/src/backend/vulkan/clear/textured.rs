use core::ffi::c_void;
use core::{mem, ptr, slice};
use std::ffi::CString;
use std::{format, vec::Vec};

use mulciber_platform::{SurfaceTarget, WindowMetrics};

use super::{ClearSurface, check, color_subresource_range, error, vk};
use crate::graphics::{
    BlendMode, DepthMode, MaterialPipelineConfig, MeshIndices, SamplerAddress, SamplerFilter,
};
use crate::resource::{Arena, DestroyRequest, ResourceId, ResourceKind};
use crate::{
    ClearColor, DeviceRequest, FrameAcquire, FrameDisposition, GraphicsError, MaterialRecord,
    PresentFeedback, SampleCount, ShaderArtifact, SurfaceInfo, TexturedInstanceBatch,
    TexturedSceneDraw, Vertex, VertexFormat,
};

const DEPTH_FORMAT: vk::VkFormat = vk::VK_FORMAT_D32_SFLOAT;
const DRAW_UNIFORM_SIZE: usize = 64;
const DRAW_UNIFORM_STRIDE: usize = 256;
const INSTANCE_TRANSFORM_SIZE: usize = 64;

#[derive(Clone, Copy, Default)]
struct Buffer {
    handle: vk::VkBuffer,
    memory: vk::VkDeviceMemory,
    size: vk::VkDeviceSize,
}

#[derive(Clone, Copy, Default)]
struct Image {
    handle: vk::VkImage,
    memory: vk::VkDeviceMemory,
    view: vk::VkImageView,
}

struct MeshResource {
    vertices: Buffer,
    indices: Buffer,
    indirect: Buffer,
    index_count: u32,
    index_type: vk::VkIndexType,
}

struct TextureResource {
    image: Image,
    sampler: vk::VkSampler,
}

struct PipelineResource {
    set_layout: vk::VkDescriptorSetLayout,
    layout: vk::VkPipelineLayout,
    pipeline: vk::VkPipeline,
    descriptor_pool: vk::VkDescriptorPool,
    sampler: vk::VkSampler,
    bindings: Vec<(ResourceId, vk::VkDescriptorSet)>,
}

struct MaterialPipelineResource {
    set_layout: vk::VkDescriptorSetLayout,
    layout: vk::VkPipelineLayout,
    pipeline: vk::VkPipeline,
    descriptor_pool: vk::VkDescriptorPool,
    /// One pipeline-owned sampler per declared slot as (binding, sampler).
    samplers: Vec<(u32, vk::VkSampler)>,
    /// Declared uniform slot as (binding, size).
    uniform: Option<(u32, u32)>,
    /// Declared texture binding numbers in ascending order.
    texture_bindings: Vec<u32>,
    /// Descriptor sets cached per texture-identity tuple in slot order.
    bindings: Vec<(Vec<ResourceId>, vk::VkDescriptorSet)>,
}

struct TargetResource {
    info: SurfaceInfo,
    multisample_color: Option<Image>,
    depth: Option<Image>,
}

struct PostprocessTargetResource {
    info: SurfaceInfo,
    scene_color: Option<Image>,
    multisample_color: Option<Image>,
    depth: Option<Image>,
}

#[derive(Clone, Copy)]
struct ResolvedDraw {
    mesh: usize,
    pipeline: usize,
    descriptor: vk::VkDescriptorSet,
    dynamic_offset: u32,
}

#[derive(Clone, Copy)]
struct ResolvedInstanceBatch {
    mesh: usize,
    pipeline: usize,
    descriptor: vk::VkDescriptorSet,
    transform_offset: u64,
    instance_count: u32,
}

#[derive(Clone, Copy)]
struct ResolvedMaterialDraw {
    mesh: usize,
    pipeline: usize,
    descriptor: vk::VkDescriptorSet,
    dynamic_offset: u32,
    /// One when the pipeline declares a uniform slot, zero otherwise.
    dynamic_offset_count: u32,
}

#[derive(Clone, Copy)]
enum PreparedScene {
    Draws,
    Instances,
    Materials,
}

pub(crate) struct TexturedSession<'window> {
    surface: ClearSurface<'window>,
    sample_count: vk::VkSampleCountFlagBits,
    uniform: Buffer,
    uniform_capacity: usize,
    resolved_draws: Vec<ResolvedDraw>,
    instance_transforms: Buffer,
    instance_capacity: usize,
    resolved_instance_batches: Vec<ResolvedInstanceBatch>,
    resolved_material_draws: Vec<ResolvedMaterialDraw>,
    meshes: Arena<MeshResource>,
    textures: Arena<TextureResource>,
    pipelines: Arena<PipelineResource>,
    instanced_pipelines: Arena<PipelineResource>,
    material_pipelines: Arena<MaterialPipelineResource>,
    targets: Arena<TargetResource>,
    postprocess_pipelines: Arena<PipelineResource>,
    postprocess_targets: Arena<PostprocessTargetResource>,
    deferred_token: Option<TexturedFrameToken>,
}

#[derive(Clone, Copy)]
pub(crate) struct TexturedFrameToken {
    image_index: u32,
    info: SurfaceInfo,
}

impl TexturedFrameToken {
    pub(crate) const fn info(&self) -> SurfaceInfo {
        self.info
    }
}

impl<'window> TexturedSession<'window> {
    pub(crate) fn new(
        target: SurfaceTarget<'window>,
        metrics: WindowMetrics,
        request: DeviceRequest,
    ) -> Result<(Self, SampleCount), GraphicsError> {
        let surface = ClearSurface::new(target, metrics)?;
        let sample_count = if request.preferred_sample_count == SampleCount::Four
            && surface.device().adapter.sample_count == vk::VK_SAMPLE_COUNT_4_BIT
        {
            vk::VK_SAMPLE_COUNT_4_BIT
        } else {
            vk::VK_SAMPLE_COUNT_1_BIT
        };
        let uniform = create_buffer(
            &surface,
            DRAW_UNIFORM_STRIDE,
            vk::VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT as u32,
            &[],
        )?;
        let instance_transforms = match create_buffer(
            &surface,
            INSTANCE_TRANSFORM_SIZE,
            vk::VK_BUFFER_USAGE_VERTEX_BUFFER_BIT as u32,
            &[],
        ) {
            Ok(buffer) => buffer,
            Err(failure) => {
                destroy_buffer(&surface, uniform);
                return Err(failure);
            }
        };
        Ok((
            Self {
                surface,
                sample_count,
                uniform,
                uniform_capacity: 1,
                resolved_draws: Vec::new(),
                instance_transforms,
                instance_capacity: 1,
                resolved_instance_batches: Vec::new(),
                resolved_material_draws: Vec::new(),
                meshes: Arena::new("mesh"),
                textures: Arena::new("texture"),
                pipelines: Arena::new("textured pipeline"),
                instanced_pipelines: Arena::new("instanced textured pipeline"),
                material_pipelines: Arena::new("material pipeline"),
                targets: Arena::new("render targets"),
                postprocess_pipelines: Arena::new("postprocess pipeline"),
                postprocess_targets: Arena::new("postprocess targets"),
                deferred_token: None,
            },
            if sample_count == vk::VK_SAMPLE_COUNT_4_BIT {
                SampleCount::Four
            } else {
                SampleCount::One
            },
        ))
    }

    pub(crate) const fn info(&self) -> SurfaceInfo {
        self.surface.info()
    }

    pub(crate) fn acquire(
        &mut self,
        metrics: WindowMetrics,
    ) -> Result<FrameAcquire<TexturedFrameToken>, GraphicsError> {
        self.flush_deferred_abandon()?;
        let acquisition = self.surface.acquire_image(metrics)?;
        self.reclaim_stale_targets()?;
        let info = self.surface.info();
        Ok(acquisition.map_ready(|image_index| TexturedFrameToken { image_index, info }))
    }

    pub(crate) fn take_present_feedback(&mut self) -> PresentFeedback {
        self.surface.take_present_feedback()
    }

    pub(crate) fn create_mesh(
        &mut self,
        vertices: &[Vertex],
        indices: &[u16],
    ) -> Result<ResourceId, GraphicsError> {
        self.create_mesh_from_bytes(bytes_of_slice(vertices), MeshIndices::U16(indices))
    }

    pub(crate) fn create_mesh_from_bytes(
        &mut self,
        vertex_bytes: &[u8],
        indices: MeshIndices<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        let (index_bytes, index_type) = match indices {
            MeshIndices::U16(indices) => (bytes_of_slice(indices), vk::VK_INDEX_TYPE_UINT16),
            MeshIndices::U32(indices) => (bytes_of_slice(indices), vk::VK_INDEX_TYPE_UINT32),
        };
        let draw = vk::VkDrawIndexedIndirectCommand {
            indexCount: u32::try_from(indices.len())
                .map_err(|_| error("mesh index count exceeds u32"))?,
            instanceCount: 1,
            firstIndex: 0,
            vertexOffset: 0,
            firstInstance: 0,
        };
        let vertices = create_buffer(
            &self.surface,
            vertex_bytes.len(),
            vk::VK_BUFFER_USAGE_VERTEX_BUFFER_BIT as u32,
            vertex_bytes,
        )?;
        let indices = match create_buffer(
            &self.surface,
            index_bytes.len(),
            vk::VK_BUFFER_USAGE_INDEX_BUFFER_BIT as u32,
            index_bytes,
        ) {
            Ok(buffer) => buffer,
            Err(failure) => {
                destroy_buffer(&self.surface, vertices);
                return Err(failure);
            }
        };
        let indirect = match create_buffer(
            &self.surface,
            mem::size_of_val(&draw),
            vk::VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT as u32,
            bytes_of(&draw),
        ) {
            Ok(buffer) => buffer,
            Err(failure) => {
                destroy_buffer(&self.surface, vertices);
                destroy_buffer(&self.surface, indices);
                return Err(failure);
            }
        };
        self.meshes.insert(MeshResource {
            vertices,
            indices,
            indirect,
            index_count: draw.indexCount,
            index_type,
        })
    }

    pub(crate) fn create_texture(
        &mut self,
        width: u32,
        height: u32,
        texels: &[u8],
    ) -> Result<ResourceId, GraphicsError> {
        let staging = create_buffer(
            &self.surface,
            texels.len(),
            vk::VK_BUFFER_USAGE_TRANSFER_SRC_BIT as u32,
            texels,
        )?;
        let image = match create_image(
            &self.surface,
            width,
            height,
            vk::VK_FORMAT_R8G8B8A8_SRGB,
            (vk::VK_IMAGE_USAGE_TRANSFER_DST_BIT | vk::VK_IMAGE_USAGE_SAMPLED_BIT) as u32,
            vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
            vk::VK_SAMPLE_COUNT_1_BIT,
        ) {
            Ok(image) => image,
            Err(failure) => {
                destroy_buffer(&self.surface, staging);
                return Err(failure);
            }
        };
        let upload = self.upload_texture(&staging, &image, width, height);
        destroy_buffer(&self.surface, staging);
        if let Err(failure) = upload {
            destroy_image(&self.surface, image);
            return Err(failure);
        }
        let sampler_info = vk::VkSamplerCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,
            magFilter: vk::VK_FILTER_LINEAR,
            minFilter: vk::VK_FILTER_LINEAR,
            mipmapMode: vk::VK_SAMPLER_MIPMAP_MODE_NEAREST,
            addressModeU: vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            addressModeV: vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            addressModeW: vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            maxAnisotropy: 1.0,
            maxLod: 0.0,
            ..Default::default()
        };
        let mut sampler = ptr::null_mut();
        if let Err(failure) = check(
            unsafe {
                self.surface
                    .device()
                    .functions
                    .create_sampler
                    .expect("loaded function")(
                    self.surface.device().handle,
                    &raw const sampler_info,
                    ptr::null(),
                    &raw mut sampler,
                )
            },
            "vkCreateSampler for textured slice",
        ) {
            destroy_image(&self.surface, image);
            return Err(failure);
        }
        self.textures.insert(TextureResource { image, sampler })
    }

    pub(crate) fn create_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        let resource = create_pipeline(&self.surface, shader.payload(), self.sample_count, false)?;
        self.pipelines.insert(resource)
    }

    pub(crate) fn create_instanced_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        let resource = create_pipeline(&self.surface, shader.payload(), self.sample_count, true)?;
        self.instanced_pipelines.insert(resource)
    }

    pub(crate) fn create_postprocess_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        let resource = create_postprocess_pipeline(&self.surface, shader.payload())?;
        self.postprocess_pipelines.insert(resource)
    }

    pub(crate) fn create_material_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
        config: &MaterialPipelineConfig<'_>,
    ) -> Result<ResourceId, GraphicsError> {
        let resource =
            create_material_pipeline(&self.surface, shader.payload(), config, self.sample_count)?;
        self.material_pipelines.insert(resource)
    }

    /// Destroys image storage for render targets from superseded surface generations.
    ///
    /// The surface generation advances only inside swapchain recreation, which first waits on the
    /// in-flight frame fence, and draws reject targets that do not match the acquired generation.
    /// A target older than the current generation therefore cannot be referenced by submitted GPU
    /// work, so its storage is reclaimed instead of growing until shutdown across live resizes.
    fn reclaim_stale_targets(&mut self) -> Result<(), GraphicsError> {
        let current = self.surface.info().generation();
        let surface = &self.surface;
        for target in self.targets.iter_mut() {
            if target.info.generation().get() >= current.get() {
                continue;
            }
            if let Some(color) = target.multisample_color.take() {
                destroy_image(surface, color);
            }
            if let Some(depth) = target.depth.take() {
                destroy_image(surface, depth);
            }
        }
        let mut reclaimed_postprocess_target = false;
        for target in self.postprocess_targets.iter_mut() {
            if target.info.generation().get() >= current.get() {
                continue;
            }
            reclaimed_postprocess_target = true;
            if let Some(color) = target.multisample_color.take() {
                destroy_image(surface, color);
            }
            if let Some(color) = target.scene_color.take() {
                destroy_image(surface, color);
            }
            if let Some(depth) = target.depth.take() {
                destroy_image(surface, depth);
            }
        }
        if reclaimed_postprocess_target {
            let device = self.surface.device();
            for pipeline in self.postprocess_pipelines.iter_mut() {
                let replacement = create_postprocess_descriptor_pool(device)?;
                unsafe {
                    device
                        .functions
                        .destroy_descriptor_pool
                        .expect("loaded function")(
                        device.handle,
                        pipeline.descriptor_pool,
                        ptr::null(),
                    );
                }
                pipeline.descriptor_pool = replacement;
                pipeline.bindings.clear();
            }
        }
        Ok(())
    }

    pub(crate) fn create_render_targets(
        &mut self,
        info: SurfaceInfo,
    ) -> Result<ResourceId, GraphicsError> {
        self.reclaim_stale_targets()?;
        let mut properties = vk::VkFormatProperties::default();
        unsafe {
            self.surface
                .device()
                .instance
                .functions
                .get_physical_device_format_properties
                .expect("loaded function")(
                self.surface.device().adapter.handle,
                DEPTH_FORMAT,
                &raw mut properties,
            );
        }
        if properties.optimalTilingFeatures
            & vk::VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT as u32
            == 0
        {
            return Err(error(
                "adapter does not support D32_FLOAT depth attachments",
            ));
        }
        let extent = info.extent();
        let depth = create_image(
            &self.surface,
            extent.width(),
            extent.height(),
            DEPTH_FORMAT,
            vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT as u32,
            vk::VK_IMAGE_ASPECT_DEPTH_BIT as u32,
            self.sample_count,
        )?;
        let multisample_color = if self.sample_count == vk::VK_SAMPLE_COUNT_4_BIT {
            match create_image(
                &self.surface,
                extent.width(),
                extent.height(),
                self.surface.swapchain.format,
                vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT as u32,
                vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
                self.sample_count,
            ) {
                Ok(image) => Some(image),
                Err(failure) => {
                    destroy_image(&self.surface, depth);
                    return Err(failure);
                }
            }
        } else {
            None
        };
        self.targets.insert(TargetResource {
            info,
            multisample_color,
            depth: Some(depth),
        })
    }

    pub(crate) fn create_postprocess_targets(
        &mut self,
        info: SurfaceInfo,
    ) -> Result<ResourceId, GraphicsError> {
        self.reclaim_stale_targets()?;
        let extent = info.extent();
        let scene_color = create_image(
            &self.surface,
            extent.width(),
            extent.height(),
            self.surface.swapchain.format,
            (vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | vk::VK_IMAGE_USAGE_SAMPLED_BIT) as u32,
            vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
            vk::VK_SAMPLE_COUNT_1_BIT,
        )?;
        let depth = match create_image(
            &self.surface,
            extent.width(),
            extent.height(),
            DEPTH_FORMAT,
            vk::VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT as u32,
            vk::VK_IMAGE_ASPECT_DEPTH_BIT as u32,
            self.sample_count,
        ) {
            Ok(depth) => depth,
            Err(failure) => {
                destroy_image(&self.surface, scene_color);
                return Err(failure);
            }
        };
        let multisample_color = if self.sample_count == vk::VK_SAMPLE_COUNT_4_BIT {
            match create_image(
                &self.surface,
                extent.width(),
                extent.height(),
                self.surface.swapchain.format,
                vk::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT as u32,
                vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
                self.sample_count,
            ) {
                Ok(image) => Some(image),
                Err(failure) => {
                    destroy_image(&self.surface, depth);
                    destroy_image(&self.surface, scene_color);
                    return Err(failure);
                }
            }
        } else {
            None
        };
        self.postprocess_targets.insert(PostprocessTargetResource {
            info,
            scene_color: Some(scene_color),
            multisample_color,
            depth: Some(depth),
        })
    }

    pub(crate) fn draw_scene_and_present(
        &mut self,
        token: TexturedFrameToken,
        draws: &[TexturedSceneDraw<'_>],
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let target_index = self.targets.index_of(targets)?;
        if self.targets[target_index].info != token.info {
            return Err(error(
                "render targets do not match acquired Vulkan generation",
            ));
        }
        self.prepare_scene(draws)?;
        self.record_draw(token.image_index, target_index, PreparedScene::Draws, clear)?;
        self.surface.submit_recorded(token.image_index)
    }

    pub(crate) fn draw_scene_postprocessed_and_present(
        &mut self,
        token: TexturedFrameToken,
        draws: &[TexturedSceneDraw<'_>],
        postprocess_pipeline: ResourceId,
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let postprocess_pipeline_index =
            self.postprocess_pipelines.index_of(postprocess_pipeline)?;
        let target_index = self.postprocess_targets.index_of(targets)?;
        if self.postprocess_targets[target_index].info != token.info {
            return Err(error(
                "postprocess targets do not match acquired Vulkan generation",
            ));
        }
        self.prepare_scene(draws)?;
        let postprocess_descriptor =
            self.postprocess_descriptor_set(postprocess_pipeline_index, target_index, targets)?;
        self.record_postprocessed_draw(
            token.image_index,
            postprocess_pipeline_index,
            target_index,
            postprocess_descriptor,
            PreparedScene::Draws,
            clear,
        )?;
        self.surface.submit_recorded(token.image_index)
    }

    pub(crate) fn draw_instanced_scene_and_present(
        &mut self,
        token: TexturedFrameToken,
        batches: &[TexturedInstanceBatch<'_>],
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let target_index = self.targets.index_of(targets)?;
        if self.targets[target_index].info != token.info {
            return Err(error(
                "render targets do not match acquired Vulkan generation",
            ));
        }
        self.prepare_instanced_scene(batches)?;
        self.record_draw(
            token.image_index,
            target_index,
            PreparedScene::Instances,
            clear,
        )?;
        self.surface.submit_recorded(token.image_index)
    }

    pub(crate) fn draw_instanced_scene_postprocessed_and_present(
        &mut self,
        token: TexturedFrameToken,
        batches: &[TexturedInstanceBatch<'_>],
        postprocess_pipeline: ResourceId,
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let postprocess_pipeline_index =
            self.postprocess_pipelines.index_of(postprocess_pipeline)?;
        let target_index = self.postprocess_targets.index_of(targets)?;
        if self.postprocess_targets[target_index].info != token.info {
            return Err(error(
                "postprocess targets do not match acquired Vulkan generation",
            ));
        }
        self.prepare_instanced_scene(batches)?;
        let postprocess_descriptor =
            self.postprocess_descriptor_set(postprocess_pipeline_index, target_index, targets)?;
        self.record_postprocessed_draw(
            token.image_index,
            postprocess_pipeline_index,
            target_index,
            postprocess_descriptor,
            PreparedScene::Instances,
            clear,
        )?;
        self.surface.submit_recorded(token.image_index)
    }

    pub(crate) fn draw_material_scene_and_present(
        &mut self,
        token: TexturedFrameToken,
        records: &[MaterialRecord<'_>],
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let target_index = self.targets.index_of(targets)?;
        if self.targets[target_index].info != token.info {
            return Err(error(
                "render targets do not match acquired Vulkan generation",
            ));
        }
        self.prepare_material_scene(records)?;
        self.record_draw(
            token.image_index,
            target_index,
            PreparedScene::Materials,
            clear,
        )?;
        self.surface.submit_recorded(token.image_index)
    }

    pub(crate) fn draw_material_scene_postprocessed_and_present(
        &mut self,
        token: TexturedFrameToken,
        records: &[MaterialRecord<'_>],
        postprocess_pipeline: ResourceId,
        targets: ResourceId,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let postprocess_pipeline_index =
            self.postprocess_pipelines.index_of(postprocess_pipeline)?;
        let target_index = self.postprocess_targets.index_of(targets)?;
        if self.postprocess_targets[target_index].info != token.info {
            return Err(error(
                "postprocess targets do not match acquired Vulkan generation",
            ));
        }
        self.prepare_material_scene(records)?;
        let postprocess_descriptor =
            self.postprocess_descriptor_set(postprocess_pipeline_index, target_index, targets)?;
        self.record_postprocessed_draw(
            token.image_index,
            postprocess_pipeline_index,
            target_index,
            postprocess_descriptor,
            PreparedScene::Materials,
            clear,
        )?;
        self.surface.submit_recorded(token.image_index)
    }

    fn prepare_material_scene(
        &mut self,
        records: &[MaterialRecord<'_>],
    ) -> Result<(), GraphicsError> {
        let last_offset = records
            .len()
            .saturating_sub(1)
            .checked_mul(DRAW_UNIFORM_STRIDE)
            .ok_or_else(|| error("Vulkan material uniform offsets overflow"))?;
        u32::try_from(last_offset)
            .map_err(|_| error("Vulkan material uniform offsets exceed u32"))?;
        self.ensure_uniform_capacity(records.len())?;
        write_material_uniforms(&self.surface, &self.uniform, records)?;
        self.resolved_material_draws.clear();
        let mut texture_ids = Vec::new();
        let mut texture_indices = Vec::new();
        for (index, record) in records.iter().enumerate() {
            let mesh = self.meshes.index_of(record.mesh.id())?;
            let pipeline = self.material_pipelines.index_of(record.pipeline.id())?;
            texture_ids.clear();
            texture_indices.clear();
            for texture in record.textures {
                texture_ids.push(texture.id());
                texture_indices.push(self.textures.index_of(texture.id())?);
            }
            let descriptor =
                self.material_descriptor_set(pipeline, &texture_ids, &texture_indices)?;
            self.resolved_material_draws.push(ResolvedMaterialDraw {
                mesh,
                pipeline,
                descriptor,
                dynamic_offset: u32::try_from(index * DRAW_UNIFORM_STRIDE)
                    .expect("material offset was validated"),
                dynamic_offset_count: u32::from(
                    self.material_pipelines[pipeline].uniform.is_some(),
                ),
            });
        }
        Ok(())
    }

    fn prepare_scene(&mut self, draws: &[TexturedSceneDraw<'_>]) -> Result<(), GraphicsError> {
        let last_offset = draws
            .len()
            .saturating_sub(1)
            .checked_mul(DRAW_UNIFORM_STRIDE)
            .ok_or_else(|| error("Vulkan scene transform offsets overflow"))?;
        u32::try_from(last_offset)
            .map_err(|_| error("Vulkan scene transform offsets exceed u32"))?;
        self.ensure_uniform_capacity(draws.len())?;
        write_scene_transforms(&self.surface, &self.uniform, draws)?;
        self.resolved_draws.clear();
        for (index, draw) in draws.iter().enumerate() {
            let mesh = self.meshes.index_of(draw.mesh.id())?;
            let texture = self.textures.index_of(draw.texture.id())?;
            let pipeline = self.pipelines.index_of(draw.pipeline.id())?;
            let descriptor = self.descriptor_set(pipeline, texture, draw.texture.id(), false)?;
            self.resolved_draws.push(ResolvedDraw {
                mesh,
                pipeline,
                descriptor,
                dynamic_offset: u32::try_from(index * DRAW_UNIFORM_STRIDE)
                    .expect("scene offset was validated"),
            });
        }
        Ok(())
    }

    fn prepare_instanced_scene(
        &mut self,
        batches: &[TexturedInstanceBatch<'_>],
    ) -> Result<(), GraphicsError> {
        let instance_count = batches.iter().try_fold(0_usize, |total, batch| {
            total
                .checked_add(batch.model_view_projections.len())
                .ok_or_else(|| error("Vulkan instance count exceeds address space"))
        })?;
        self.ensure_instance_capacity(instance_count)?;
        write_instance_transforms(&self.surface, &self.instance_transforms, batches)?;
        self.resolved_instance_batches.clear();
        let mut transform_offset = 0_usize;
        for batch in batches {
            let mesh = self.meshes.index_of(batch.mesh.id())?;
            let texture = self.textures.index_of(batch.texture.id())?;
            let pipeline = self.instanced_pipelines.index_of(batch.pipeline.id())?;
            let descriptor = self.descriptor_set(pipeline, texture, batch.texture.id(), true)?;
            self.resolved_instance_batches.push(ResolvedInstanceBatch {
                mesh,
                pipeline,
                descriptor,
                transform_offset: u64::try_from(transform_offset)
                    .map_err(|_| error("Vulkan instance offset exceeds u64"))?,
                instance_count: u32::try_from(batch.model_view_projections.len())
                    .map_err(|_| error("Vulkan batch instance count exceeds u32"))?,
            });
            transform_offset = transform_offset
                .checked_add(
                    batch
                        .model_view_projections
                        .len()
                        .checked_mul(INSTANCE_TRANSFORM_SIZE)
                        .ok_or_else(|| error("Vulkan instance offset overflow"))?,
                )
                .ok_or_else(|| error("Vulkan instance offset overflow"))?;
        }
        Ok(())
    }

    fn ensure_uniform_capacity(&mut self, required: usize) -> Result<(), GraphicsError> {
        if required <= self.uniform_capacity {
            return Ok(());
        }
        let capacity = required
            .checked_next_power_of_two()
            .ok_or_else(|| error("Vulkan scene transform capacity overflow"))?;
        let bytes = capacity
            .checked_mul(DRAW_UNIFORM_STRIDE)
            .ok_or_else(|| error("Vulkan scene transform storage is too large"))?;
        self.surface.wait_for_frame()?;
        let replacement = create_buffer(
            &self.surface,
            bytes,
            vk::VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT as u32,
            &[],
        )?;
        if let Err(failure) = self
            .reset_descriptor_pools(false)
            .and_then(|()| self.reset_descriptor_pools(true))
        {
            destroy_buffer(&self.surface, replacement);
            return Err(failure);
        }
        let previous = mem::replace(&mut self.uniform, replacement);
        destroy_buffer(&self.surface, previous);
        self.uniform_capacity = capacity;
        Ok(())
    }

    fn ensure_instance_capacity(&mut self, required: usize) -> Result<(), GraphicsError> {
        if required <= self.instance_capacity {
            return Ok(());
        }
        let capacity = required
            .checked_next_power_of_two()
            .ok_or_else(|| error("Vulkan instance transform capacity overflow"))?;
        let bytes = capacity
            .checked_mul(INSTANCE_TRANSFORM_SIZE)
            .ok_or_else(|| error("Vulkan instance transform storage is too large"))?;
        self.surface.wait_for_frame()?;
        let replacement = create_buffer(
            &self.surface,
            bytes,
            vk::VK_BUFFER_USAGE_VERTEX_BUFFER_BIT as u32,
            &[],
        )?;
        let previous = mem::replace(&mut self.instance_transforms, replacement);
        destroy_buffer(&self.surface, previous);
        self.instance_capacity = capacity;
        Ok(())
    }

    pub(crate) fn abandon(
        &mut self,
        _token: TexturedFrameToken,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.surface.abandon()
    }

    pub(crate) fn defer_abandon(&mut self, token: TexturedFrameToken) {
        self.deferred_token = Some(token);
    }

    fn flush_deferred_abandon(&mut self) -> Result<(), GraphicsError> {
        if let Some(token) = self.deferred_token.take() {
            self.abandon(token)?;
        }
        Ok(())
    }

    pub(crate) fn reclaim_resources(
        &mut self,
        requests: &[DestroyRequest],
    ) -> Result<(), GraphicsError> {
        if requests.is_empty() {
            return Ok(());
        }
        self.surface.wait_for_frame()?;
        let reset_scene_descriptors = requests.iter().any(|request| {
            request.kind == ResourceKind::Texture && self.textures.get(request.id).is_ok()
        });
        let reset_postprocess_descriptors = requests.iter().any(|request| {
            request.kind == ResourceKind::PostprocessTargets
                && self.postprocess_targets.get(request.id).is_ok()
        });
        if reset_scene_descriptors {
            self.reset_descriptor_pools(false)?;
        }
        if reset_postprocess_descriptors {
            self.reset_descriptor_pools(true)?;
        }
        for &request in requests {
            self.destroy_resource_if_live(request);
        }
        Ok(())
    }

    pub(crate) fn destroy_resource(
        &mut self,
        request: DestroyRequest,
    ) -> Result<(), GraphicsError> {
        self.surface.wait_for_frame()?;
        if request.kind == ResourceKind::Texture {
            self.textures.get(request.id)?;
            self.reset_descriptor_pools(false)?;
        } else if request.kind == ResourceKind::PostprocessTargets {
            self.postprocess_targets.get(request.id)?;
            self.reset_descriptor_pools(true)?;
        }
        let device = self.surface.device();
        match request.kind {
            ResourceKind::Mesh => destroy_mesh_device(device, self.meshes.remove(request.id)?),
            ResourceKind::Texture => {
                destroy_texture_device(device, self.textures.remove(request.id)?);
            }
            ResourceKind::TexturedPipeline => {
                destroy_pipeline_device(device, self.pipelines.remove(request.id)?);
            }
            ResourceKind::InstancedTexturedPipeline => {
                destroy_pipeline_device(device, self.instanced_pipelines.remove(request.id)?);
            }
            ResourceKind::PostprocessPipeline => {
                destroy_pipeline_device(device, self.postprocess_pipelines.remove(request.id)?);
            }
            ResourceKind::MaterialPipeline => {
                destroy_material_pipeline_device(
                    device,
                    self.material_pipelines.remove(request.id)?,
                );
            }
            ResourceKind::RenderTargets => {
                destroy_target_device(device, self.targets.remove(request.id)?);
            }
            ResourceKind::PostprocessTargets => destroy_postprocess_target_device(
                device,
                self.postprocess_targets.remove(request.id)?,
            ),
        }
        Ok(())
    }

    fn destroy_resource_if_live(&mut self, request: DestroyRequest) {
        let device = self.surface.device();
        match request.kind {
            ResourceKind::Mesh => self
                .meshes
                .remove_if_live(request.id)
                .map(|resource| destroy_mesh_device(device, resource)),
            ResourceKind::Texture => self
                .textures
                .remove_if_live(request.id)
                .map(|resource| destroy_texture_device(device, resource)),
            ResourceKind::TexturedPipeline => self
                .pipelines
                .remove_if_live(request.id)
                .map(|resource| destroy_pipeline_device(device, resource)),
            ResourceKind::InstancedTexturedPipeline => self
                .instanced_pipelines
                .remove_if_live(request.id)
                .map(|resource| destroy_pipeline_device(device, resource)),
            ResourceKind::PostprocessPipeline => self
                .postprocess_pipelines
                .remove_if_live(request.id)
                .map(|resource| destroy_pipeline_device(device, resource)),
            ResourceKind::MaterialPipeline => self
                .material_pipelines
                .remove_if_live(request.id)
                .map(|resource| destroy_material_pipeline_device(device, resource)),
            ResourceKind::RenderTargets => self
                .targets
                .remove_if_live(request.id)
                .map(|resource| destroy_target_device(device, resource)),
            ResourceKind::PostprocessTargets => self
                .postprocess_targets
                .remove_if_live(request.id)
                .map(|resource| destroy_postprocess_target_device(device, resource)),
        };
    }

    fn reset_descriptor_pools(&mut self, postprocess: bool) -> Result<(), GraphicsError> {
        let device = self.surface.device();
        if postprocess {
            for pipeline in self.postprocess_pipelines.iter_mut() {
                let replacement = create_postprocess_descriptor_pool(device)?;
                unsafe {
                    device
                        .functions
                        .destroy_descriptor_pool
                        .expect("loaded function")(
                        device.handle,
                        pipeline.descriptor_pool,
                        ptr::null(),
                    );
                }
                pipeline.descriptor_pool = replacement;
                pipeline.bindings.clear();
            }
            return Ok(());
        }
        for pipeline in self
            .pipelines
            .iter_mut()
            .chain(self.instanced_pipelines.iter_mut())
        {
            let replacement = create_descriptor_pool(device)?;
            unsafe {
                device
                    .functions
                    .destroy_descriptor_pool
                    .expect("loaded function")(
                    device.handle,
                    pipeline.descriptor_pool,
                    ptr::null(),
                );
            }
            pipeline.descriptor_pool = replacement;
            pipeline.bindings.clear();
        }
        for pipeline in self.material_pipelines.iter_mut() {
            let replacement = create_material_descriptor_pool(device)?;
            unsafe {
                device
                    .functions
                    .destroy_descriptor_pool
                    .expect("loaded function")(
                    device.handle,
                    pipeline.descriptor_pool,
                    ptr::null(),
                );
            }
            pipeline.descriptor_pool = replacement;
            pipeline.bindings.clear();
        }
        Ok(())
    }

    pub(crate) fn shutdown(mut self) -> Result<(), GraphicsError> {
        self.flush_deferred_abandon()?;
        let result = self.surface.finish();
        self.destroy_resources();
        let surface = unsafe { ptr::read(&raw const self.surface) };
        mem::forget(self);
        result.and(surface.shutdown())
    }

    fn upload_texture(
        &mut self,
        staging: &Buffer,
        image: &Image,
        width: u32,
        height: u32,
    ) -> Result<(), GraphicsError> {
        self.begin_upload()?;
        let to_transfer = image_barrier(
            image.handle,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
            color_subresource_range(),
        );
        pipeline_barrier(&self.surface, &to_transfer);
        let region = vk::VkBufferImageCopy2 {
            sType: vk::VK_STRUCTURE_TYPE_BUFFER_IMAGE_COPY_2,
            imageSubresource: vk::VkImageSubresourceLayers {
                aspectMask: vk::VK_IMAGE_ASPECT_COLOR_BIT as u32,
                layerCount: 1,
                ..Default::default()
            },
            imageExtent: vk::VkExtent3D {
                width,
                height,
                depth: 1,
            },
            ..Default::default()
        };
        let copy = vk::VkCopyBufferToImageInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_COPY_BUFFER_TO_IMAGE_INFO_2,
            srcBuffer: staging.handle,
            dstImage: image.handle,
            dstImageLayout: vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            regionCount: 1,
            pRegions: &raw const region,
            ..Default::default()
        };
        unsafe {
            self.surface
                .device()
                .functions
                .cmd_copy_buffer_to_image2
                .expect("loaded function")(self.surface.command_buffer, &raw const copy);
        }
        let to_sampled = image_barrier(
            image.handle,
            vk::VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_COPY_BIT,
            vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
            vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
            vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
            color_subresource_range(),
        );
        pipeline_barrier(&self.surface, &to_sampled);
        self.end_upload()
    }

    fn begin_upload(&mut self) -> Result<(), GraphicsError> {
        self.surface.wait_for_frame()?;
        let device = self.surface.device();
        check(
            unsafe {
                device.functions.reset_fences.expect("loaded function")(
                    device.handle,
                    1,
                    &raw const self.surface.frame_fence,
                )
            },
            "vkResetFences for resource upload",
        )?;
        check(
            unsafe {
                device
                    .functions
                    .reset_command_buffer
                    .expect("loaded function")(self.surface.command_buffer, 0)
            },
            "vkResetCommandBuffer for resource upload",
        )?;
        let begin = vk::VkCommandBufferBeginInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            flags: vk::VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT as u32,
            ..Default::default()
        };
        check(
            unsafe {
                device
                    .functions
                    .begin_command_buffer
                    .expect("loaded function")(
                    self.surface.command_buffer, &raw const begin
                )
            },
            "vkBeginCommandBuffer for resource upload",
        )
    }

    fn end_upload(&mut self) -> Result<(), GraphicsError> {
        let device = self.surface.device();
        check(
            unsafe {
                device
                    .functions
                    .end_command_buffer
                    .expect("loaded function")(self.surface.command_buffer)
            },
            "vkEndCommandBuffer for resource upload",
        )?;
        let command = vk::VkCommandBufferSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO,
            commandBuffer: self.surface.command_buffer,
            ..Default::default()
        };
        let submit = vk::VkSubmitInfo2 {
            sType: vk::VK_STRUCTURE_TYPE_SUBMIT_INFO_2,
            commandBufferInfoCount: 1,
            pCommandBufferInfos: &raw const command,
            ..Default::default()
        };
        check(
            unsafe {
                device.functions.queue_submit2.expect("loaded function")(
                    device.queue,
                    1,
                    &raw const submit,
                    self.surface.frame_fence,
                )
            },
            "vkQueueSubmit2 for resource upload",
        )?;
        self.surface.frame_pending = true;
        self.surface.wait_for_frame()
    }

    fn descriptor_set(
        &mut self,
        pipeline_index: usize,
        texture_index: usize,
        texture_id: ResourceId,
        instanced: bool,
    ) -> Result<vk::VkDescriptorSet, GraphicsError> {
        let pipelines = if instanced {
            &mut self.instanced_pipelines
        } else {
            &mut self.pipelines
        };
        if let Some((_, set)) = pipelines[pipeline_index]
            .bindings
            .iter()
            .find(|(id, _)| *id == texture_id)
        {
            return Ok(*set);
        }
        let pipeline = &mut pipelines[pipeline_index];
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: pipeline.descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const pipeline.set_layout,
            ..Default::default()
        };
        let mut set = ptr::null_mut();
        check(
            unsafe {
                self.surface
                    .device()
                    .functions
                    .allocate_descriptor_sets
                    .expect("loaded function")(
                    self.surface.device().handle,
                    &raw const allocate,
                    &raw mut set,
                )
            },
            "vkAllocateDescriptorSets for texture",
        )?;
        let buffer = vk::VkDescriptorBufferInfo {
            buffer: self.uniform.handle,
            offset: 0,
            range: DRAW_UNIFORM_SIZE as u64,
        };
        let image = vk::VkDescriptorImageInfo {
            sampler: ptr::null_mut(),
            imageView: self.textures[texture_index].image.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        };
        let sampler = vk::VkDescriptorImageInfo {
            sampler: self.textures[texture_index].sampler,
            ..Default::default()
        };
        let writes = [
            descriptor_write(
                set,
                0,
                vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
                (&raw const buffer).cast(),
            ),
            descriptor_write(
                set,
                1,
                vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
                (&raw const image).cast(),
            ),
            descriptor_write(
                set,
                2,
                vk::VK_DESCRIPTOR_TYPE_SAMPLER,
                (&raw const sampler).cast(),
            ),
        ];
        unsafe {
            self.surface
                .device()
                .functions
                .update_descriptor_sets
                .expect("loaded function")(
                self.surface.device().handle,
                3,
                writes.as_ptr(),
                0,
                ptr::null(),
            );
        };
        pipeline.bindings.push((texture_id, set));
        Ok(set)
    }

    fn material_descriptor_set(
        &mut self,
        pipeline_index: usize,
        texture_ids: &[ResourceId],
        texture_indices: &[usize],
    ) -> Result<vk::VkDescriptorSet, GraphicsError> {
        if let Some((_, set)) = self.material_pipelines[pipeline_index]
            .bindings
            .iter()
            .find(|(ids, _)| ids.as_slice() == texture_ids)
        {
            return Ok(*set);
        }
        let pipeline = &self.material_pipelines[pipeline_index];
        let (set_layout, descriptor_pool) = (pipeline.set_layout, pipeline.descriptor_pool);
        let pipeline_uniform = pipeline.uniform;
        let texture_bindings = pipeline.texture_bindings.clone();
        let samplers = pipeline.samplers.clone();
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const set_layout,
            ..Default::default()
        };
        let mut set = ptr::null_mut();
        check(
            unsafe {
                self.surface
                    .device()
                    .functions
                    .allocate_descriptor_sets
                    .expect("loaded function")(
                    self.surface.device().handle,
                    &raw const allocate,
                    &raw mut set,
                )
            },
            "vkAllocateDescriptorSets for material record",
        )?;
        let buffer = vk::VkDescriptorBufferInfo {
            buffer: self.uniform.handle,
            offset: 0,
            range: pipeline_uniform.map_or(1, |(_, size)| u64::from(size)),
        };
        let images: Vec<vk::VkDescriptorImageInfo> = texture_indices
            .iter()
            .map(|&index| vk::VkDescriptorImageInfo {
                sampler: ptr::null_mut(),
                imageView: self.textures[index].image.view,
                imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            })
            .collect();
        let sampler_infos: Vec<vk::VkDescriptorImageInfo> = samplers
            .iter()
            .map(|&(_, sampler)| vk::VkDescriptorImageInfo {
                sampler,
                ..Default::default()
            })
            .collect();
        let mut writes = Vec::with_capacity(1 + images.len() + sampler_infos.len());
        if let Some((binding, _)) = pipeline_uniform {
            writes.push(descriptor_write(
                set,
                binding,
                vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
                (&raw const buffer).cast(),
            ));
        }
        for (image, &binding) in images.iter().zip(texture_bindings.iter()) {
            writes.push(descriptor_write(
                set,
                binding,
                vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
                ptr::from_ref(image).cast(),
            ));
        }
        for (info, &(binding, _)) in sampler_infos.iter().zip(samplers.iter()) {
            writes.push(descriptor_write(
                set,
                binding,
                vk::VK_DESCRIPTOR_TYPE_SAMPLER,
                ptr::from_ref(info).cast(),
            ));
        }
        unsafe {
            self.surface
                .device()
                .functions
                .update_descriptor_sets
                .expect("loaded function")(
                self.surface.device().handle,
                u32::try_from(writes.len()).expect("descriptor write count fits u32"),
                writes.as_ptr(),
                0,
                ptr::null(),
            );
        };
        self.material_pipelines[pipeline_index]
            .bindings
            .push((texture_ids.to_vec(), set));
        Ok(set)
    }

    fn postprocess_descriptor_set(
        &mut self,
        pipeline_index: usize,
        target_index: usize,
        target_id: ResourceId,
    ) -> Result<vk::VkDescriptorSet, GraphicsError> {
        if let Some((_, set)) = self.postprocess_pipelines[pipeline_index]
            .bindings
            .iter()
            .find(|(id, _)| *id == target_id)
        {
            return Ok(*set);
        }
        let scene_color = self.postprocess_targets[target_index]
            .scene_color
            .ok_or_else(|| error("postprocess targets were reclaimed by a newer generation"))?;
        let pipeline = &mut self.postprocess_pipelines[pipeline_index];
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: pipeline.descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const pipeline.set_layout,
            ..Default::default()
        };
        let mut set = ptr::null_mut();
        check(
            unsafe {
                self.surface
                    .device()
                    .functions
                    .allocate_descriptor_sets
                    .expect("loaded function")(
                    self.surface.device().handle,
                    &raw const allocate,
                    &raw mut set,
                )
            },
            "vkAllocateDescriptorSets for postprocess target",
        )?;
        let buffer = vk::VkDescriptorBufferInfo {
            buffer: self.uniform.handle,
            offset: 0,
            range: 64,
        };
        let image = vk::VkDescriptorImageInfo {
            sampler: ptr::null_mut(),
            imageView: scene_color.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        };
        let sampler = vk::VkDescriptorImageInfo {
            sampler: pipeline.sampler,
            ..Default::default()
        };
        let writes = [
            descriptor_write(
                set,
                0,
                vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
                (&raw const buffer).cast(),
            ),
            descriptor_write(
                set,
                1,
                vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
                (&raw const image).cast(),
            ),
            descriptor_write(
                set,
                2,
                vk::VK_DESCRIPTOR_TYPE_SAMPLER,
                (&raw const sampler).cast(),
            ),
        ];
        unsafe {
            self.surface
                .device()
                .functions
                .update_descriptor_sets
                .expect("loaded function")(
                self.surface.device().handle,
                3,
                writes.as_ptr(),
                0,
                ptr::null(),
            );
        }
        pipeline.bindings.push((target_id, set));
        Ok(set)
    }

    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    fn record_draw(
        &mut self,
        image_index: u32,
        target_index: usize,
        scene: PreparedScene,
        clear: ClearColor,
    ) -> Result<(), GraphicsError> {
        let slot = usize::try_from(image_index).map_err(|_| error("invalid image index"))?;
        let target_depth = self.targets[target_index]
            .depth
            .ok_or_else(|| error("render targets were reclaimed by a newer surface generation"))?;
        let multisample_color = self.targets[target_index].multisample_color;
        let image = self.surface.swapchain.images[slot];
        let view = self.surface.swapchain.views[slot];
        let old_layout = if self.surface.swapchain.initialized[slot] {
            vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR
        } else {
            vk::VK_IMAGE_LAYOUT_UNDEFINED
        };
        self.surface.wait_for_frame()?;
        let device = self.surface.device();
        check(
            unsafe {
                device.functions.reset_fences.expect("loaded function")(
                    device.handle,
                    1,
                    &raw const self.surface.frame_fence,
                )
            },
            "vkResetFences for textured frame",
        )?;
        check(
            unsafe {
                device
                    .functions
                    .reset_command_buffer
                    .expect("loaded function")(self.surface.command_buffer, 0)
            },
            "vkResetCommandBuffer for textured frame",
        )?;
        let begin = vk::VkCommandBufferBeginInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            flags: vk::VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT as u32,
            ..Default::default()
        };
        check(
            unsafe {
                device
                    .functions
                    .begin_command_buffer
                    .expect("loaded function")(
                    self.surface.command_buffer, &raw const begin
                )
            },
            "vkBeginCommandBuffer for textured frame",
        )?;
        let color_barrier = image_barrier(
            image,
            old_layout,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            if old_layout == vk::VK_IMAGE_LAYOUT_UNDEFINED {
                vk::VK_PIPELINE_STAGE_2_NONE
            } else {
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT
            },
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            color_subresource_range(),
        );
        let depth_barrier = image_barrier(
            target_depth.handle,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
            depth_subresource_range(),
        );
        let multisample_barrier = multisample_color.map(|color| {
            image_barrier(
                color.handle,
                vk::VK_IMAGE_LAYOUT_UNDEFINED,
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_NONE,
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                vk::VK_ACCESS_2_NONE,
                vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                color_subresource_range(),
            )
        });
        if let Some(multisample_barrier) = multisample_barrier {
            pipeline_barriers(
                &self.surface,
                &[color_barrier, depth_barrier, multisample_barrier],
            );
        } else {
            pipeline_barriers(&self.surface, &[color_barrier, depth_barrier]);
        }
        let color_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: multisample_color.map_or(view, |color| color.view),
            imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            resolveMode: if multisample_color.is_some() {
                vk::VK_RESOLVE_MODE_AVERAGE_BIT
            } else {
                vk::VK_RESOLVE_MODE_NONE
            },
            resolveImageView: multisample_color.map_or(ptr::null_mut(), |_| view),
            resolveImageLayout: if multisample_color.is_some() {
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL
            } else {
                vk::VK_IMAGE_LAYOUT_UNDEFINED
            },
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: vk::VkClearValue {
                color: vk::VkClearColorValue {
                    float32: clear.components(),
                },
            },
            ..Default::default()
        };
        let depth_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: target_depth.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
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
        let area = vk::VkRect2D {
            offset: vk::VkOffset2D { x: 0, y: 0 },
            extent: self.surface.swapchain.extent,
        };
        let rendering = vk::VkRenderingInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
            renderArea: area,
            layerCount: 1,
            colorAttachmentCount: 1,
            pColorAttachments: &raw const color_attachment,
            pDepthAttachment: &raw const depth_attachment,
            ..Default::default()
        };
        let viewport = vk::VkViewport {
            x: 0.0,
            y: 0.0,
            width: area.extent.width as f32,
            height: area.extent.height as f32,
            minDepth: 0.0,
            maxDepth: 1.0,
        };
        unsafe {
            let functions = &device.functions;
            functions.cmd_begin_rendering.expect("loaded function")(
                self.surface.command_buffer,
                &raw const rendering,
            );
            functions.cmd_set_viewport.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const viewport,
            );
            functions.cmd_set_scissor.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const area,
            );
            self.record_prepared_scene(scene);
            functions.cmd_end_rendering.expect("loaded function")(self.surface.command_buffer);
        }
        let present = image_barrier(
            image,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_ACCESS_2_NONE,
            color_subresource_range(),
        );
        pipeline_barrier(&self.surface, &present);
        check(
            unsafe {
                device
                    .functions
                    .end_command_buffer
                    .expect("loaded function")(self.surface.command_buffer)
            },
            "vkEndCommandBuffer for textured frame",
        )
    }

    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    fn record_postprocessed_draw(
        &mut self,
        image_index: u32,
        postprocess_pipeline_index: usize,
        target_index: usize,
        postprocess_descriptor: vk::VkDescriptorSet,
        scene: PreparedScene,
        clear: ClearColor,
    ) -> Result<(), GraphicsError> {
        let slot = usize::try_from(image_index).map_err(|_| error("invalid image index"))?;
        let target = &self.postprocess_targets[target_index];
        let scene_color = target
            .scene_color
            .ok_or_else(|| error("postprocess targets were reclaimed by a newer generation"))?;
        let target_depth = target
            .depth
            .ok_or_else(|| error("postprocess targets were reclaimed by a newer generation"))?;
        let multisample_color = target.multisample_color;
        let image = self.surface.swapchain.images[slot];
        let view = self.surface.swapchain.views[slot];
        let old_layout = if self.surface.swapchain.initialized[slot] {
            vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR
        } else {
            vk::VK_IMAGE_LAYOUT_UNDEFINED
        };
        self.surface.wait_for_frame()?;
        let device = self.surface.device();
        check(
            unsafe {
                device.functions.reset_fences.expect("loaded function")(
                    device.handle,
                    1,
                    &raw const self.surface.frame_fence,
                )
            },
            "vkResetFences for postprocessed frame",
        )?;
        check(
            unsafe {
                device
                    .functions
                    .reset_command_buffer
                    .expect("loaded function")(self.surface.command_buffer, 0)
            },
            "vkResetCommandBuffer for postprocessed frame",
        )?;
        let begin = vk::VkCommandBufferBeginInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            flags: vk::VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT as u32,
            ..Default::default()
        };
        check(
            unsafe {
                device
                    .functions
                    .begin_command_buffer
                    .expect("loaded function")(
                    self.surface.command_buffer, &raw const begin
                )
            },
            "vkBeginCommandBuffer for postprocessed frame",
        )?;
        let swapchain_barrier = image_barrier(
            image,
            old_layout,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            if old_layout == vk::VK_IMAGE_LAYOUT_UNDEFINED {
                vk::VK_PIPELINE_STAGE_2_NONE
            } else {
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT
            },
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            color_subresource_range(),
        );
        let scene_barrier = image_barrier(
            scene_color.handle,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            color_subresource_range(),
        );
        let depth_barrier = image_barrier(
            target_depth.handle,
            vk::VK_IMAGE_LAYOUT_UNDEFINED,
            vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
                | vk::VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT,
            vk::VK_ACCESS_2_NONE,
            vk::VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT,
            depth_subresource_range(),
        );
        let multisample_barrier = multisample_color.map(|color| {
            image_barrier(
                color.handle,
                vk::VK_IMAGE_LAYOUT_UNDEFINED,
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                vk::VK_PIPELINE_STAGE_2_NONE,
                vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                vk::VK_ACCESS_2_NONE,
                vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
                color_subresource_range(),
            )
        });
        if let Some(multisample_barrier) = multisample_barrier {
            pipeline_barriers(
                &self.surface,
                &[
                    swapchain_barrier,
                    scene_barrier,
                    depth_barrier,
                    multisample_barrier,
                ],
            );
        } else {
            pipeline_barriers(
                &self.surface,
                &[swapchain_barrier, scene_barrier, depth_barrier],
            );
        }

        let scene_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: multisample_color.map_or(scene_color.view, |color| color.view),
            imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            resolveMode: if multisample_color.is_some() {
                vk::VK_RESOLVE_MODE_AVERAGE_BIT
            } else {
                vk::VK_RESOLVE_MODE_NONE
            },
            resolveImageView: multisample_color.map_or(ptr::null_mut(), |_| scene_color.view),
            resolveImageLayout: if multisample_color.is_some() {
                vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL
            } else {
                vk::VK_IMAGE_LAYOUT_UNDEFINED
            },
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: vk::VkClearValue {
                color: vk::VkClearColorValue {
                    float32: clear.components(),
                },
            },
            ..Default::default()
        };
        let depth_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: target_depth.view,
            imageLayout: vk::VK_IMAGE_LAYOUT_DEPTH_ATTACHMENT_OPTIMAL,
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
        let area = vk::VkRect2D {
            offset: vk::VkOffset2D { x: 0, y: 0 },
            extent: self.surface.swapchain.extent,
        };
        let scene_rendering = vk::VkRenderingInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
            renderArea: area,
            layerCount: 1,
            colorAttachmentCount: 1,
            pColorAttachments: &raw const scene_attachment,
            pDepthAttachment: &raw const depth_attachment,
            ..Default::default()
        };
        let viewport = vk::VkViewport {
            x: 0.0,
            y: 0.0,
            width: area.extent.width as f32,
            height: area.extent.height as f32,
            minDepth: 0.0,
            maxDepth: 1.0,
        };
        unsafe {
            let functions = &device.functions;
            functions.cmd_begin_rendering.expect("loaded function")(
                self.surface.command_buffer,
                &raw const scene_rendering,
            );
            functions.cmd_set_viewport.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const viewport,
            );
            functions.cmd_set_scissor.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const area,
            );
            self.record_prepared_scene(scene);
            functions.cmd_end_rendering.expect("loaded function")(self.surface.command_buffer);
        }

        let sample_scene = image_barrier(
            scene_color.handle,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
            color_subresource_range(),
        );
        pipeline_barrier(&self.surface, &sample_scene);
        let post_attachment = vk::VkRenderingAttachmentInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            imageView: view,
            imageLayout: vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            loadOp: vk::VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: vk::VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: vk::VkClearValue {
                color: vk::VkClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                },
            },
            ..Default::default()
        };
        let post_rendering = vk::VkRenderingInfo {
            sType: vk::VK_STRUCTURE_TYPE_RENDERING_INFO,
            renderArea: area,
            layerCount: 1,
            colorAttachmentCount: 1,
            pColorAttachments: &raw const post_attachment,
            ..Default::default()
        };
        let post_pipeline = &self.postprocess_pipelines[postprocess_pipeline_index];
        unsafe {
            let functions = &device.functions;
            functions.cmd_begin_rendering.expect("loaded function")(
                self.surface.command_buffer,
                &raw const post_rendering,
            );
            functions.cmd_bind_pipeline.expect("loaded function")(
                self.surface.command_buffer,
                vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                post_pipeline.pipeline,
            );
            functions.cmd_bind_descriptor_sets.expect("loaded function")(
                self.surface.command_buffer,
                vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                post_pipeline.layout,
                0,
                1,
                &raw const postprocess_descriptor,
                0,
                ptr::null(),
            );
            functions.cmd_set_viewport.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const viewport,
            );
            functions.cmd_set_scissor.expect("loaded function")(
                self.surface.command_buffer,
                0,
                1,
                &raw const area,
            );
            functions.cmd_draw.expect("loaded function")(self.surface.command_buffer, 3, 1, 0, 0);
            functions.cmd_end_rendering.expect("loaded function")(self.surface.command_buffer);
        }
        let present = image_barrier(
            image,
            vk::VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            vk::VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
            vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            vk::VK_PIPELINE_STAGE_2_NONE,
            vk::VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT,
            vk::VK_ACCESS_2_NONE,
            color_subresource_range(),
        );
        pipeline_barrier(&self.surface, &present);
        check(
            unsafe {
                device
                    .functions
                    .end_command_buffer
                    .expect("loaded function")(self.surface.command_buffer)
            },
            "vkEndCommandBuffer for postprocessed frame",
        )
    }

    #[allow(clippy::too_many_lines)]
    unsafe fn record_prepared_scene(&self, scene: PreparedScene) {
        unsafe {
            let functions = &self.surface.device().functions;
            match scene {
                PreparedScene::Draws => {
                    let offset = 0_u64;
                    for draw in &self.resolved_draws {
                        let mesh = &self.meshes[draw.mesh];
                        let pipeline = &self.pipelines[draw.pipeline];
                        functions.cmd_bind_pipeline.expect("loaded function")(
                            self.surface.command_buffer,
                            vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                            pipeline.pipeline,
                        );
                        functions.cmd_bind_descriptor_sets.expect("loaded function")(
                            self.surface.command_buffer,
                            vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                            pipeline.layout,
                            0,
                            1,
                            &raw const draw.descriptor,
                            1,
                            &raw const draw.dynamic_offset,
                        );
                        functions.cmd_bind_vertex_buffers.expect("loaded function")(
                            self.surface.command_buffer,
                            0,
                            1,
                            &raw const mesh.vertices.handle,
                            &raw const offset,
                        );
                        functions.cmd_bind_index_buffer.expect("loaded function")(
                            self.surface.command_buffer,
                            mesh.indices.handle,
                            0,
                            mesh.index_type,
                        );
                        functions
                            .cmd_draw_indexed_indirect
                            .expect("loaded function")(
                            self.surface.command_buffer,
                            mesh.indirect.handle,
                            0,
                            1,
                            u32::try_from(mem::size_of::<vk::VkDrawIndexedIndirectCommand>())
                                .expect("indirect command size fits u32"),
                        );
                    }
                }
                PreparedScene::Materials => {
                    let offset = 0_u64;
                    for draw in &self.resolved_material_draws {
                        let mesh = &self.meshes[draw.mesh];
                        let pipeline = &self.material_pipelines[draw.pipeline];
                        functions.cmd_bind_pipeline.expect("loaded function")(
                            self.surface.command_buffer,
                            vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                            pipeline.pipeline,
                        );
                        functions.cmd_bind_descriptor_sets.expect("loaded function")(
                            self.surface.command_buffer,
                            vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                            pipeline.layout,
                            0,
                            1,
                            &raw const draw.descriptor,
                            draw.dynamic_offset_count,
                            &raw const draw.dynamic_offset,
                        );
                        functions.cmd_bind_vertex_buffers.expect("loaded function")(
                            self.surface.command_buffer,
                            0,
                            1,
                            &raw const mesh.vertices.handle,
                            &raw const offset,
                        );
                        functions.cmd_bind_index_buffer.expect("loaded function")(
                            self.surface.command_buffer,
                            mesh.indices.handle,
                            0,
                            mesh.index_type,
                        );
                        functions
                            .cmd_draw_indexed_indirect
                            .expect("loaded function")(
                            self.surface.command_buffer,
                            mesh.indirect.handle,
                            0,
                            1,
                            u32::try_from(mem::size_of::<vk::VkDrawIndexedIndirectCommand>())
                                .expect("indirect command size fits u32"),
                        );
                    }
                }
                PreparedScene::Instances => {
                    let dynamic_offset = 0_u32;
                    for batch in &self.resolved_instance_batches {
                        let mesh = &self.meshes[batch.mesh];
                        let pipeline = &self.instanced_pipelines[batch.pipeline];
                        functions.cmd_bind_pipeline.expect("loaded function")(
                            self.surface.command_buffer,
                            vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                            pipeline.pipeline,
                        );
                        functions.cmd_bind_descriptor_sets.expect("loaded function")(
                            self.surface.command_buffer,
                            vk::VK_PIPELINE_BIND_POINT_GRAPHICS,
                            pipeline.layout,
                            0,
                            1,
                            &raw const batch.descriptor,
                            1,
                            &raw const dynamic_offset,
                        );
                        let buffers = [mesh.vertices.handle, self.instance_transforms.handle];
                        let offsets = [0_u64, batch.transform_offset];
                        functions.cmd_bind_vertex_buffers.expect("loaded function")(
                            self.surface.command_buffer,
                            0,
                            2,
                            buffers.as_ptr(),
                            offsets.as_ptr(),
                        );
                        functions.cmd_bind_index_buffer.expect("loaded function")(
                            self.surface.command_buffer,
                            mesh.indices.handle,
                            0,
                            mesh.index_type,
                        );
                        functions.cmd_draw_indexed.expect("loaded function")(
                            self.surface.command_buffer,
                            mesh.index_count,
                            batch.instance_count,
                            0,
                            0,
                            0,
                        );
                    }
                }
            }
        }
    }

    fn destroy_resources(&mut self) {
        let device = self.surface.device();
        for pipeline in self
            .pipelines
            .take_all()
            .into_iter()
            .chain(self.instanced_pipelines.take_all())
            .chain(self.postprocess_pipelines.take_all())
        {
            destroy_pipeline_device(device, pipeline);
        }
        for pipeline in self.material_pipelines.take_all() {
            destroy_material_pipeline_device(device, pipeline);
        }
        for texture in self.textures.take_all() {
            destroy_texture_device(device, texture);
        }
        for target in self.targets.take_all() {
            destroy_target_device(device, target);
        }
        for target in self.postprocess_targets.take_all() {
            destroy_postprocess_target_device(device, target);
        }
        for mesh in self.meshes.take_all() {
            destroy_mesh_device(device, mesh);
        }
        unsafe {
            destroy_buffer_device(device, mem::take(&mut self.uniform));
            destroy_buffer_device(device, mem::take(&mut self.instance_transforms));
        }

        // `shutdown` moves the surface out and deliberately suppresses this
        // session's destructor. Release the now-empty arenas' allocations here
        // so that path does not retain their backing storage.
        self.pipelines = Arena::new("textured pipeline");
        self.instanced_pipelines = Arena::new("instanced textured pipeline");
        self.material_pipelines = Arena::new("material pipeline");
        self.postprocess_pipelines = Arena::new("postprocess pipeline");
        self.textures = Arena::new("texture");
        self.targets = Arena::new("render targets");
        self.postprocess_targets = Arena::new("postprocess targets");
        self.meshes = Arena::new("mesh");
    }
}

impl Drop for TexturedSession<'_> {
    fn drop(&mut self) {
        let _ = self.surface.finish();
        self.destroy_resources();
    }
}

// These helpers intentionally consume the wrapper so one call owns the native destruction
// authority. Most fields are raw handles, so Clippy cannot otherwise observe that ownership.
#[allow(clippy::needless_pass_by_value)]
fn destroy_mesh_device(device: &super::Device, mesh: MeshResource) {
    unsafe {
        destroy_buffer_device(device, mesh.vertices);
        destroy_buffer_device(device, mesh.indices);
        destroy_buffer_device(device, mesh.indirect);
    }
}

#[allow(clippy::needless_pass_by_value)]
fn destroy_texture_device(device: &super::Device, texture: TextureResource) {
    unsafe {
        device.functions.destroy_sampler.expect("loaded function")(
            device.handle,
            texture.sampler,
            ptr::null(),
        );
        destroy_image_device(device, texture.image);
    }
}

#[allow(clippy::needless_pass_by_value)]
fn destroy_pipeline_device(device: &super::Device, pipeline: PipelineResource) {
    unsafe {
        device.functions.destroy_pipeline.expect("loaded function")(
            device.handle,
            pipeline.pipeline,
            ptr::null(),
        );
        device
            .functions
            .destroy_pipeline_layout
            .expect("loaded function")(device.handle, pipeline.layout, ptr::null());
        device
            .functions
            .destroy_descriptor_pool
            .expect("loaded function")(device.handle, pipeline.descriptor_pool, ptr::null());
        device
            .functions
            .destroy_descriptor_set_layout
            .expect("loaded function")(device.handle, pipeline.set_layout, ptr::null());
        if !pipeline.sampler.is_null() {
            device.functions.destroy_sampler.expect("loaded function")(
                device.handle,
                pipeline.sampler,
                ptr::null(),
            );
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn destroy_material_pipeline_device(device: &super::Device, pipeline: MaterialPipelineResource) {
    for &(_, sampler) in &pipeline.samplers {
        unsafe {
            device.functions.destroy_sampler.expect("loaded function")(
                device.handle,
                sampler,
                ptr::null(),
            );
        }
    }
    destroy_pipeline_device(
        device,
        PipelineResource {
            set_layout: pipeline.set_layout,
            layout: pipeline.layout,
            pipeline: pipeline.pipeline,
            descriptor_pool: pipeline.descriptor_pool,
            sampler: ptr::null_mut(),
            bindings: Vec::new(),
        },
    );
}

#[allow(clippy::needless_pass_by_value)]
fn destroy_target_device(device: &super::Device, target: TargetResource) {
    unsafe {
        if let Some(color) = target.multisample_color {
            destroy_image_device(device, color);
        }
        if let Some(depth) = target.depth {
            destroy_image_device(device, depth);
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn destroy_postprocess_target_device(device: &super::Device, target: PostprocessTargetResource) {
    unsafe {
        if let Some(color) = target.multisample_color {
            destroy_image_device(device, color);
        }
        if let Some(color) = target.scene_color {
            destroy_image_device(device, color);
        }
        if let Some(depth) = target.depth {
            destroy_image_device(device, depth);
        }
    }
}

impl ClearSurface<'_> {
    fn submit_recorded(&mut self, image_index: u32) -> Result<FrameDisposition, GraphicsError> {
        let slot = usize::try_from(image_index).map_err(|_| error("invalid image index"))?;
        let render_finished = self.swapchain.render_finished[slot];
        let wait = vk::VkSemaphoreSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_SEMAPHORE_SUBMIT_INFO,
            semaphore: self.image_available,
            stageMask: vk::VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
            ..Default::default()
        };
        let command = vk::VkCommandBufferSubmitInfo {
            sType: vk::VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO,
            commandBuffer: self.command_buffer,
            ..Default::default()
        };
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
        check(
            unsafe {
                self.device()
                    .functions
                    .queue_submit2
                    .expect("loaded function")(
                    self.device().queue,
                    1,
                    &raw const submit,
                    self.frame_fence,
                )
            },
            "vkQueueSubmit2 for textured frame",
        )?;
        self.frame_pending = true;
        self.swapchain.initialized[slot] = true;
        self.queue_present_with_feedback(
            image_index,
            render_finished,
            "vkQueuePresentKHR for textured frame",
        )?;
        Ok(FrameDisposition::Presented(self.info.generation()))
    }
}

fn create_buffer(
    surface: &ClearSurface<'_>,
    size: usize,
    usage: u32,
    bytes: &[u8],
) -> Result<Buffer, GraphicsError> {
    let size =
        u64::try_from(size).map_err(|_| error("buffer size exceeds Vulkan address space"))?;
    let info = vk::VkBufferCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        size,
        usage,
        sharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
        ..Default::default()
    };
    let device = surface.device();
    let mut buffer = Buffer {
        size,
        ..Default::default()
    };
    check(
        unsafe {
            device.functions.create_buffer.expect("loaded function")(
                device.handle,
                &raw const info,
                ptr::null(),
                &raw mut buffer.handle,
            )
        },
        "vkCreateBuffer for textured slice",
    )?;
    if let Err(failure) = complete_buffer_storage(surface, &mut buffer, bytes) {
        destroy_buffer(surface, buffer);
        return Err(failure);
    }
    Ok(buffer)
}

fn complete_buffer_storage(
    surface: &ClearSurface<'_>,
    buffer: &mut Buffer,
    bytes: &[u8],
) -> Result<(), GraphicsError> {
    let device = surface.device();
    let mut requirements = vk::VkMemoryRequirements::default();
    unsafe {
        device
            .functions
            .get_buffer_memory_requirements
            .expect("loaded function")(device.handle, buffer.handle, &raw mut requirements);
    };
    let memory_type = find_memory_type(
        device,
        requirements.memoryTypeBits,
        (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) as u32,
    )
    .ok_or_else(|| error("no host-visible coherent Vulkan memory type"))?;
    let allocate = vk::VkMemoryAllocateInfo {
        sType: vk::VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        allocationSize: requirements.size,
        memoryTypeIndex: memory_type,
        ..Default::default()
    };
    check(
        unsafe {
            device.functions.allocate_memory.expect("loaded function")(
                device.handle,
                &raw const allocate,
                ptr::null(),
                &raw mut buffer.memory,
            )
        },
        "vkAllocateMemory for buffer",
    )?;
    check(
        unsafe {
            device
                .functions
                .bind_buffer_memory
                .expect("loaded function")(
                device.handle, buffer.handle, buffer.memory, 0
            )
        },
        "vkBindBufferMemory",
    )?;
    if !bytes.is_empty() {
        write_buffer(surface, buffer, bytes)?;
    }
    Ok(())
}

fn write_buffer(
    surface: &ClearSurface<'_>,
    buffer: &Buffer,
    bytes: &[u8],
) -> Result<(), GraphicsError> {
    if u64::try_from(bytes.len()).map_err(|_| error("buffer write exceeds u64"))? > buffer.size {
        return Err(error("buffer write exceeds allocation"));
    }
    let device = surface.device();
    let mut mapped = ptr::null_mut();
    check(
        unsafe {
            device.functions.map_memory.expect("loaded function")(
                device.handle,
                buffer.memory,
                0,
                buffer.size,
                0,
                &raw mut mapped,
            )
        },
        "vkMapMemory",
    )?;
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast(), bytes.len());
        device.functions.unmap_memory.expect("loaded function")(device.handle, buffer.memory);
    }
    Ok(())
}

fn write_scene_transforms(
    surface: &ClearSurface<'_>,
    buffer: &Buffer,
    draws: &[TexturedSceneDraw<'_>],
) -> Result<(), GraphicsError> {
    let required = draws
        .len()
        .saturating_sub(1)
        .checked_mul(DRAW_UNIFORM_STRIDE)
        .and_then(|offset| offset.checked_add(DRAW_UNIFORM_SIZE))
        .ok_or_else(|| error("scene transform write exceeds address space"))?;
    if u64::try_from(required).map_err(|_| error("scene transform write exceeds u64"))?
        > buffer.size
    {
        return Err(error("scene transform write exceeds allocation"));
    }
    let device = surface.device();
    let mut mapped = ptr::null_mut();
    check(
        unsafe {
            device.functions.map_memory.expect("loaded function")(
                device.handle,
                buffer.memory,
                0,
                buffer.size,
                0,
                &raw mut mapped,
            )
        },
        "vkMapMemory for scene transforms",
    )?;
    unsafe {
        for (index, draw) in draws.iter().enumerate() {
            ptr::copy_nonoverlapping(
                ptr::from_ref(&draw.model_view_projection).cast::<u8>(),
                mapped.cast::<u8>().add(index * DRAW_UNIFORM_STRIDE),
                DRAW_UNIFORM_SIZE,
            );
        }
        device.functions.unmap_memory.expect("loaded function")(device.handle, buffer.memory);
    }
    Ok(())
}

fn write_material_uniforms(
    surface: &ClearSurface<'_>,
    buffer: &Buffer,
    records: &[MaterialRecord<'_>],
) -> Result<(), GraphicsError> {
    let required = records
        .len()
        .saturating_sub(1)
        .checked_mul(DRAW_UNIFORM_STRIDE)
        .and_then(|offset| offset.checked_add(DRAW_UNIFORM_STRIDE))
        .ok_or_else(|| error("material uniform write exceeds address space"))?;
    if u64::try_from(required).map_err(|_| error("material uniform write exceeds u64"))?
        > buffer.size
    {
        return Err(error("material uniform write exceeds allocation"));
    }
    debug_assert!(
        records
            .iter()
            .all(|record| record.uniform.len() <= DRAW_UNIFORM_STRIDE),
        "uniform sizes were validated at pipeline creation"
    );
    let device = surface.device();
    let mut mapped = ptr::null_mut();
    check(
        unsafe {
            device.functions.map_memory.expect("loaded function")(
                device.handle,
                buffer.memory,
                0,
                buffer.size,
                0,
                &raw mut mapped,
            )
        },
        "vkMapMemory for material uniforms",
    )?;
    unsafe {
        for (index, record) in records.iter().enumerate() {
            ptr::copy_nonoverlapping(
                record.uniform.as_ptr(),
                mapped.cast::<u8>().add(index * DRAW_UNIFORM_STRIDE),
                record.uniform.len(),
            );
        }
        device.functions.unmap_memory.expect("loaded function")(device.handle, buffer.memory);
    }
    Ok(())
}

fn write_instance_transforms(
    surface: &ClearSurface<'_>,
    buffer: &Buffer,
    batches: &[TexturedInstanceBatch<'_>],
) -> Result<(), GraphicsError> {
    let required = batches.iter().try_fold(0_usize, |bytes, batch| {
        batch
            .model_view_projections
            .len()
            .checked_mul(INSTANCE_TRANSFORM_SIZE)
            .and_then(|batch_bytes| bytes.checked_add(batch_bytes))
            .ok_or_else(|| error("instance transform write exceeds address space"))
    })?;
    if u64::try_from(required).map_err(|_| error("instance transform write exceeds u64"))?
        > buffer.size
    {
        return Err(error("instance transform write exceeds allocation"));
    }
    let device = surface.device();
    let mut mapped = ptr::null_mut();
    check(
        unsafe {
            device.functions.map_memory.expect("loaded function")(
                device.handle,
                buffer.memory,
                0,
                buffer.size,
                0,
                &raw mut mapped,
            )
        },
        "vkMapMemory for instance transforms",
    )?;
    unsafe {
        let mut offset = 0_usize;
        for batch in batches {
            let bytes = mem::size_of_val(batch.model_view_projections);
            ptr::copy_nonoverlapping(
                batch.model_view_projections.as_ptr().cast::<u8>(),
                mapped.cast::<u8>().add(offset),
                bytes,
            );
            offset += bytes;
        }
        device.functions.unmap_memory.expect("loaded function")(device.handle, buffer.memory);
    }
    Ok(())
}

fn create_image(
    surface: &ClearSurface<'_>,
    width: u32,
    height: u32,
    format: vk::VkFormat,
    usage: u32,
    aspect: u32,
    samples: vk::VkSampleCountFlagBits,
) -> Result<Image, GraphicsError> {
    let info = vk::VkImageCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
        imageType: vk::VK_IMAGE_TYPE_2D,
        format,
        extent: vk::VkExtent3D {
            width,
            height,
            depth: 1,
        },
        mipLevels: 1,
        arrayLayers: 1,
        samples,
        tiling: vk::VK_IMAGE_TILING_OPTIMAL,
        usage,
        sharingMode: vk::VK_SHARING_MODE_EXCLUSIVE,
        initialLayout: vk::VK_IMAGE_LAYOUT_UNDEFINED,
        ..Default::default()
    };
    let device = surface.device();
    let mut image = Image::default();
    check(
        unsafe {
            device.functions.create_image.expect("loaded function")(
                device.handle,
                &raw const info,
                ptr::null(),
                &raw mut image.handle,
            )
        },
        "vkCreateImage for textured slice",
    )?;
    if let Err(failure) = complete_image_storage(device, &mut image, format, aspect) {
        // SAFETY: The device is live and the destroy helper skips null child handles left by
        // partial construction.
        unsafe { destroy_image_device(device, image) };
        return Err(failure);
    }
    Ok(image)
}

fn complete_image_storage(
    device: &super::Device,
    image: &mut Image,
    format: vk::VkFormat,
    aspect: u32,
) -> Result<(), GraphicsError> {
    let mut requirements = vk::VkMemoryRequirements::default();
    unsafe {
        device
            .functions
            .get_image_memory_requirements
            .expect("loaded function")(device.handle, image.handle, &raw mut requirements);
    };
    let memory_type = find_memory_type(
        device,
        requirements.memoryTypeBits,
        vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT as u32,
    )
    .ok_or_else(|| error("no device-local Vulkan image memory type"))?;
    let allocate = vk::VkMemoryAllocateInfo {
        sType: vk::VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        allocationSize: requirements.size,
        memoryTypeIndex: memory_type,
        ..Default::default()
    };
    check(
        unsafe {
            device.functions.allocate_memory.expect("loaded function")(
                device.handle,
                &raw const allocate,
                ptr::null(),
                &raw mut image.memory,
            )
        },
        "vkAllocateMemory for image",
    )?;
    check(
        unsafe {
            device.functions.bind_image_memory.expect("loaded function")(
                device.handle,
                image.handle,
                image.memory,
                0,
            )
        },
        "vkBindImageMemory",
    )?;
    let view_info = vk::VkImageViewCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
        image: image.handle,
        viewType: vk::VK_IMAGE_VIEW_TYPE_2D,
        format,
        subresourceRange: vk::VkImageSubresourceRange {
            aspectMask: aspect,
            levelCount: 1,
            layerCount: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    check(
        unsafe {
            device.functions.create_image_view.expect("loaded function")(
                device.handle,
                &raw const view_info,
                ptr::null(),
                &raw mut image.view,
            )
        },
        "vkCreateImageView for textured slice",
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn create_pipeline(
    surface: &ClearSurface<'_>,
    bytes: &[u8],
    sample_count: vk::VkSampleCountFlagBits,
    instanced: bool,
) -> Result<PipelineResource, GraphicsError> {
    let device = surface.device();
    let bindings = [
        layout_binding(
            0,
            vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
            vk::VK_SHADER_STAGE_VERTEX_BIT as u32,
        ),
        layout_binding(
            1,
            vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
            vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
        ),
        layout_binding(
            2,
            vk::VK_DESCRIPTOR_TYPE_SAMPLER,
            vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
        ),
    ];
    let layout_info = vk::VkDescriptorSetLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        bindingCount: 3,
        pBindings: bindings.as_ptr(),
        ..Default::default()
    };
    let mut resource = PipelineResource {
        set_layout: ptr::null_mut(),
        layout: ptr::null_mut(),
        pipeline: ptr::null_mut(),
        descriptor_pool: ptr::null_mut(),
        sampler: ptr::null_mut(),
        bindings: Vec::new(),
    };
    check(
        unsafe {
            device
                .functions
                .create_descriptor_set_layout
                .expect("loaded function")(
                device.handle,
                &raw const layout_info,
                ptr::null(),
                &raw mut resource.set_layout,
            )
        },
        "vkCreateDescriptorSetLayout for textured pipeline",
    )?;
    let pipeline_layout = vk::VkPipelineLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        setLayoutCount: 1,
        pSetLayouts: &raw const resource.set_layout,
        ..Default::default()
    };
    check(
        unsafe {
            device
                .functions
                .create_pipeline_layout
                .expect("loaded function")(
                device.handle,
                &raw const pipeline_layout,
                ptr::null(),
                &raw mut resource.layout,
            )
        },
        "vkCreatePipelineLayout for textured pipeline",
    )?;
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect();
    let module_info = vk::VkShaderModuleCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        codeSize: bytes.len(),
        pCode: words.as_ptr(),
        ..Default::default()
    };
    let mut module = ptr::null_mut();
    check(
        unsafe {
            device
                .functions
                .create_shader_module
                .expect("loaded function")(
                device.handle,
                &raw const module_info,
                ptr::null(),
                &raw mut module,
            )
        },
        "vkCreateShaderModule for cube",
    )?;
    let stages = [
        shader_stage(
            vk::VK_SHADER_STAGE_VERTEX_BIT,
            module,
            if instanced {
                c"instanced_vertex"
            } else {
                c"cube_vertex"
            },
        ),
        shader_stage(vk::VK_SHADER_STAGE_FRAGMENT_BIT, module, c"cube_fragment"),
    ];
    let bindings = [
        vk::VkVertexInputBindingDescription {
            binding: 0,
            stride: u32::try_from(mem::size_of::<Vertex>()).expect("vertex size fits u32"),
            inputRate: vk::VK_VERTEX_INPUT_RATE_VERTEX,
        },
        vk::VkVertexInputBindingDescription {
            binding: 1,
            stride: u32::try_from(INSTANCE_TRANSFORM_SIZE)
                .expect("instance transform stride fits u32"),
            inputRate: vk::VK_VERTEX_INPUT_RATE_INSTANCE,
        },
    ];
    let attributes = [
        attribute(0, 0, vk::VK_FORMAT_R32G32B32_SFLOAT, 0),
        attribute(1, 0, vk::VK_FORMAT_R32G32B32_SFLOAT, 12),
        attribute(2, 0, vk::VK_FORMAT_R32G32_SFLOAT, 24),
        attribute(3, 1, vk::VK_FORMAT_R32G32B32A32_SFLOAT, 0),
        attribute(4, 1, vk::VK_FORMAT_R32G32B32A32_SFLOAT, 16),
        attribute(5, 1, vk::VK_FORMAT_R32G32B32A32_SFLOAT, 32),
        attribute(6, 1, vk::VK_FORMAT_R32G32B32A32_SFLOAT, 48),
    ];
    let vertex_input = vk::VkPipelineVertexInputStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        vertexBindingDescriptionCount: if instanced { 2 } else { 1 },
        pVertexBindingDescriptions: bindings.as_ptr(),
        vertexAttributeDescriptionCount: if instanced { 7 } else { 3 },
        pVertexAttributeDescriptions: attributes.as_ptr(),
        ..Default::default()
    };
    let assembly = vk::VkPipelineInputAssemblyStateCreateInfo {
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
    let raster = vk::VkPipelineRasterizationStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
        polygonMode: vk::VK_POLYGON_MODE_FILL,
        cullMode: vk::VK_CULL_MODE_BACK_BIT as u32,
        frontFace: vk::VK_FRONT_FACE_COUNTER_CLOCKWISE,
        lineWidth: 1.0,
        ..Default::default()
    };
    let multisample = vk::VkPipelineMultisampleStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
        rasterizationSamples: sample_count,
        ..Default::default()
    };
    let depth = vk::VkPipelineDepthStencilStateCreateInfo {
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
    let color_format = surface.swapchain.format;
    let rendering = vk::VkPipelineRenderingCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
        colorAttachmentCount: 1,
        pColorAttachmentFormats: &raw const color_format,
        depthAttachmentFormat: DEPTH_FORMAT,
        ..Default::default()
    };
    let pipeline_info = vk::VkGraphicsPipelineCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
        pNext: (&raw const rendering).cast(),
        stageCount: 2,
        pStages: stages.as_ptr(),
        pVertexInputState: &raw const vertex_input,
        pInputAssemblyState: &raw const assembly,
        pViewportState: &raw const viewport,
        pRasterizationState: &raw const raster,
        pMultisampleState: &raw const multisample,
        pDepthStencilState: &raw const depth,
        pColorBlendState: &raw const blend,
        pDynamicState: &raw const dynamic,
        layout: resource.layout,
        basePipelineIndex: -1,
        ..Default::default()
    };
    let result = check(
        unsafe {
            device
                .functions
                .create_graphics_pipelines
                .expect("loaded function")(
                device.handle,
                ptr::null_mut(),
                1,
                &raw const pipeline_info,
                ptr::null(),
                &raw mut resource.pipeline,
            )
        },
        "vkCreateGraphicsPipelines for cube",
    );
    unsafe {
        device
            .functions
            .destroy_shader_module
            .expect("loaded function")(device.handle, module, ptr::null());
    };
    result?;
    resource.descriptor_pool = create_descriptor_pool(device)?;
    Ok(resource)
}

fn create_descriptor_pool(device: &super::Device) -> Result<vk::VkDescriptorPool, GraphicsError> {
    create_pipeline_descriptor_pool(
        device,
        "textured",
        vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
    )
}

fn create_material_descriptor_pool(
    device: &super::Device,
) -> Result<vk::VkDescriptorPool, GraphicsError> {
    create_pipeline_descriptor_pool(
        device,
        "material",
        vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
    )
}

const fn material_filter(filter: SamplerFilter) -> vk::VkFilter {
    match filter {
        SamplerFilter::Nearest => vk::VK_FILTER_NEAREST,
        SamplerFilter::Linear => vk::VK_FILTER_LINEAR,
    }
}

const fn material_address(address: SamplerAddress) -> vk::VkSamplerAddressMode {
    match address {
        SamplerAddress::Repeat => vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
        SamplerAddress::ClampToEdge => vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
    }
}

const fn material_vertex_format(format: VertexFormat) -> vk::VkFormat {
    match format {
        VertexFormat::Float32 => vk::VK_FORMAT_R32_SFLOAT,
        VertexFormat::Float32x2 => vk::VK_FORMAT_R32G32_SFLOAT,
        VertexFormat::Float32x3 => vk::VK_FORMAT_R32G32B32_SFLOAT,
        VertexFormat::Float32x4 => vk::VK_FORMAT_R32G32B32A32_SFLOAT,
        VertexFormat::Uint32 => vk::VK_FORMAT_R32_UINT,
        VertexFormat::Uint32x2 => vk::VK_FORMAT_R32G32_UINT,
        VertexFormat::Uint32x3 => vk::VK_FORMAT_R32G32B32_UINT,
        VertexFormat::Uint32x4 => vk::VK_FORMAT_R32G32B32A32_UINT,
        VertexFormat::Sint32 => vk::VK_FORMAT_R32_SINT,
        VertexFormat::Sint32x2 => vk::VK_FORMAT_R32G32_SINT,
        VertexFormat::Sint32x3 => vk::VK_FORMAT_R32G32B32_SINT,
        VertexFormat::Sint32x4 => vk::VK_FORMAT_R32G32B32A32_SINT,
    }
}

#[allow(clippy::too_many_lines)]
fn create_material_pipeline(
    surface: &ClearSurface<'_>,
    bytes: &[u8],
    config: &MaterialPipelineConfig<'_>,
    sample_count: vk::VkSampleCountFlagBits,
) -> Result<MaterialPipelineResource, GraphicsError> {
    let device = surface.device();
    let stages = (vk::VK_SHADER_STAGE_VERTEX_BIT | vk::VK_SHADER_STAGE_FRAGMENT_BIT) as u32;
    let mut layout_bindings = Vec::new();
    if let Some((binding, _)) = config.uniform {
        layout_bindings.push(layout_binding(
            binding,
            vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC,
            stages,
        ));
    }
    for &binding in config.texture_bindings {
        layout_bindings.push(layout_binding(
            binding,
            vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
            stages,
        ));
    }
    for slot in config.sampler_bindings {
        layout_bindings.push(layout_binding(
            slot.binding,
            vk::VK_DESCRIPTOR_TYPE_SAMPLER,
            stages,
        ));
    }
    let layout_info = vk::VkDescriptorSetLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        bindingCount: u32::try_from(layout_bindings.len())
            .map_err(|_| error("material binding count exceeds u32"))?,
        pBindings: layout_bindings.as_ptr(),
        ..Default::default()
    };
    let mut resource = MaterialPipelineResource {
        set_layout: ptr::null_mut(),
        layout: ptr::null_mut(),
        pipeline: ptr::null_mut(),
        descriptor_pool: ptr::null_mut(),
        samplers: Vec::with_capacity(config.sampler_bindings.len()),
        uniform: config.uniform,
        texture_bindings: config.texture_bindings.to_vec(),
        bindings: Vec::new(),
    };
    check(
        unsafe {
            device
                .functions
                .create_descriptor_set_layout
                .expect("loaded function")(
                device.handle,
                &raw const layout_info,
                ptr::null(),
                &raw mut resource.set_layout,
            )
        },
        "vkCreateDescriptorSetLayout for material pipeline",
    )?;
    let pipeline_layout = vk::VkPipelineLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        setLayoutCount: 1,
        pSetLayouts: &raw const resource.set_layout,
        ..Default::default()
    };
    check(
        unsafe {
            device
                .functions
                .create_pipeline_layout
                .expect("loaded function")(
                device.handle,
                &raw const pipeline_layout,
                ptr::null(),
                &raw mut resource.layout,
            )
        },
        "vkCreatePipelineLayout for material pipeline",
    )?;
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect();
    let module_info = vk::VkShaderModuleCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        codeSize: bytes.len(),
        pCode: words.as_ptr(),
        ..Default::default()
    };
    let mut module = ptr::null_mut();
    check(
        unsafe {
            device
                .functions
                .create_shader_module
                .expect("loaded function")(
                device.handle,
                &raw const module_info,
                ptr::null(),
                &raw mut module,
            )
        },
        "vkCreateShaderModule for material pipeline",
    )?;
    let vertex_entry = CString::new(config.vertex_entry)
        .map_err(|_| error("material vertex entry point name contains a NUL byte"))?;
    let fragment_entry = CString::new(config.fragment_entry)
        .map_err(|_| error("material fragment entry point name contains a NUL byte"))?;
    let shader_stages = [
        shader_stage(vk::VK_SHADER_STAGE_VERTEX_BIT, module, &vertex_entry),
        shader_stage(vk::VK_SHADER_STAGE_FRAGMENT_BIT, module, &fragment_entry),
    ];
    let vertex_binding = vk::VkVertexInputBindingDescription {
        binding: 0,
        stride: config.stride,
        inputRate: vk::VK_VERTEX_INPUT_RATE_VERTEX,
    };
    let attributes: Vec<vk::VkVertexInputAttributeDescription> = config
        .attributes
        .iter()
        .map(|input| {
            attribute(
                input.location,
                0,
                material_vertex_format(input.format),
                input.offset,
            )
        })
        .collect();
    let vertex_input = vk::VkPipelineVertexInputStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        vertexBindingDescriptionCount: 1,
        pVertexBindingDescriptions: &raw const vertex_binding,
        vertexAttributeDescriptionCount: u32::try_from(attributes.len())
            .map_err(|_| error("material attribute count exceeds u32"))?,
        pVertexAttributeDescriptions: attributes.as_ptr(),
        ..Default::default()
    };
    let assembly = vk::VkPipelineInputAssemblyStateCreateInfo {
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
    let raster = vk::VkPipelineRasterizationStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
        polygonMode: vk::VK_POLYGON_MODE_FILL,
        cullMode: vk::VK_CULL_MODE_BACK_BIT as u32,
        frontFace: vk::VK_FRONT_FACE_COUNTER_CLOCKWISE,
        lineWidth: 1.0,
        ..Default::default()
    };
    let multisample = vk::VkPipelineMultisampleStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
        rasterizationSamples: sample_count,
        alphaToCoverageEnable: if matches!(config.blend, BlendMode::Cutout) {
            vk::VK_TRUE
        } else {
            vk::VK_FALSE
        },
        ..Default::default()
    };
    let (depth_test, depth_write) = match config.depth {
        DepthMode::TestWrite => (vk::VK_TRUE, vk::VK_TRUE),
        DepthMode::TestOnly => (vk::VK_TRUE, vk::VK_FALSE),
        DepthMode::Off => (vk::VK_FALSE, vk::VK_FALSE),
    };
    let depth = vk::VkPipelineDepthStencilStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
        depthTestEnable: depth_test,
        depthWriteEnable: depth_write,
        depthCompareOp: vk::VK_COMPARE_OP_LESS,
        minDepthBounds: 0.0,
        maxDepthBounds: 1.0,
        ..Default::default()
    };
    let write_mask = (vk::VK_COLOR_COMPONENT_R_BIT
        | vk::VK_COLOR_COMPONENT_G_BIT
        | vk::VK_COLOR_COMPONENT_B_BIT
        | vk::VK_COLOR_COMPONENT_A_BIT) as u32;
    let blend_attachment = if matches!(config.blend, BlendMode::PremultipliedTranslucent) {
        vk::VkPipelineColorBlendAttachmentState {
            blendEnable: vk::VK_TRUE,
            srcColorBlendFactor: vk::VK_BLEND_FACTOR_ONE,
            dstColorBlendFactor: vk::VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            colorBlendOp: vk::VK_BLEND_OP_ADD,
            srcAlphaBlendFactor: vk::VK_BLEND_FACTOR_ONE,
            dstAlphaBlendFactor: vk::VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            alphaBlendOp: vk::VK_BLEND_OP_ADD,
            colorWriteMask: write_mask,
        }
    } else {
        vk::VkPipelineColorBlendAttachmentState {
            colorWriteMask: write_mask,
            ..Default::default()
        }
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
    let color_format = surface.swapchain.format;
    let rendering = vk::VkPipelineRenderingCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
        colorAttachmentCount: 1,
        pColorAttachmentFormats: &raw const color_format,
        depthAttachmentFormat: DEPTH_FORMAT,
        ..Default::default()
    };
    let pipeline_info = vk::VkGraphicsPipelineCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
        pNext: (&raw const rendering).cast(),
        stageCount: 2,
        pStages: shader_stages.as_ptr(),
        pVertexInputState: &raw const vertex_input,
        pInputAssemblyState: &raw const assembly,
        pViewportState: &raw const viewport,
        pRasterizationState: &raw const raster,
        pMultisampleState: &raw const multisample,
        pDepthStencilState: &raw const depth,
        pColorBlendState: &raw const blend,
        pDynamicState: &raw const dynamic,
        layout: resource.layout,
        basePipelineIndex: -1,
        ..Default::default()
    };
    let result = check(
        unsafe {
            device
                .functions
                .create_graphics_pipelines
                .expect("loaded function")(
                device.handle,
                ptr::null_mut(),
                1,
                &raw const pipeline_info,
                ptr::null(),
                &raw mut resource.pipeline,
            )
        },
        "vkCreateGraphicsPipelines for material pipeline",
    );
    unsafe {
        device
            .functions
            .destroy_shader_module
            .expect("loaded function")(device.handle, module, ptr::null());
    };
    result?;
    for slot in config.sampler_bindings {
        let filter = material_filter(slot.filter);
        let address = material_address(slot.address);
        let sampler_info = vk::VkSamplerCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,
            magFilter: filter,
            minFilter: filter,
            mipmapMode: vk::VK_SAMPLER_MIPMAP_MODE_NEAREST,
            addressModeU: address,
            addressModeV: address,
            addressModeW: address,
            maxAnisotropy: 1.0,
            maxLod: 0.0,
            ..Default::default()
        };
        let mut sampler = ptr::null_mut();
        check(
            unsafe {
                device.functions.create_sampler.expect("loaded function")(
                    device.handle,
                    &raw const sampler_info,
                    ptr::null(),
                    &raw mut sampler,
                )
            },
            "vkCreateSampler for material pipeline",
        )?;
        resource.samplers.push((slot.binding, sampler));
    }
    resource.descriptor_pool = create_material_descriptor_pool(device)?;
    Ok(resource)
}

#[allow(clippy::too_many_lines)]
fn create_postprocess_pipeline(
    surface: &ClearSurface<'_>,
    bytes: &[u8],
) -> Result<PipelineResource, GraphicsError> {
    let device = surface.device();
    let bindings = [
        layout_binding(
            0,
            vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
            vk::VK_SHADER_STAGE_VERTEX_BIT as u32,
        ),
        layout_binding(
            1,
            vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
            vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
        ),
        layout_binding(
            2,
            vk::VK_DESCRIPTOR_TYPE_SAMPLER,
            vk::VK_SHADER_STAGE_FRAGMENT_BIT as u32,
        ),
    ];
    let layout_info = vk::VkDescriptorSetLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        bindingCount: 3,
        pBindings: bindings.as_ptr(),
        ..Default::default()
    };
    let mut resource = PipelineResource {
        set_layout: ptr::null_mut(),
        layout: ptr::null_mut(),
        pipeline: ptr::null_mut(),
        descriptor_pool: ptr::null_mut(),
        sampler: ptr::null_mut(),
        bindings: Vec::new(),
    };
    check(
        unsafe {
            device
                .functions
                .create_descriptor_set_layout
                .expect("loaded function")(
                device.handle,
                &raw const layout_info,
                ptr::null(),
                &raw mut resource.set_layout,
            )
        },
        "vkCreateDescriptorSetLayout for postprocess pipeline",
    )?;
    let pipeline_layout = vk::VkPipelineLayoutCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        setLayoutCount: 1,
        pSetLayouts: &raw const resource.set_layout,
        ..Default::default()
    };
    check(
        unsafe {
            device
                .functions
                .create_pipeline_layout
                .expect("loaded function")(
                device.handle,
                &raw const pipeline_layout,
                ptr::null(),
                &raw mut resource.layout,
            )
        },
        "vkCreatePipelineLayout for postprocess pipeline",
    )?;
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect();
    let module_info = vk::VkShaderModuleCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        codeSize: bytes.len(),
        pCode: words.as_ptr(),
        ..Default::default()
    };
    let mut module = ptr::null_mut();
    check(
        unsafe {
            device
                .functions
                .create_shader_module
                .expect("loaded function")(
                device.handle,
                &raw const module_info,
                ptr::null(),
                &raw mut module,
            )
        },
        "vkCreateShaderModule for postprocess pipeline",
    )?;
    let stages = [
        shader_stage(vk::VK_SHADER_STAGE_VERTEX_BIT, module, c"post_vertex"),
        shader_stage(vk::VK_SHADER_STAGE_FRAGMENT_BIT, module, c"post_fragment"),
    ];
    let vertex_input = vk::VkPipelineVertexInputStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        ..Default::default()
    };
    let assembly = vk::VkPipelineInputAssemblyStateCreateInfo {
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
    let raster = vk::VkPipelineRasterizationStateCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
        polygonMode: vk::VK_POLYGON_MODE_FILL,
        cullMode: vk::VK_CULL_MODE_NONE as u32,
        frontFace: vk::VK_FRONT_FACE_COUNTER_CLOCKWISE,
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
    let color_format = surface.swapchain.format;
    let rendering = vk::VkPipelineRenderingCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
        colorAttachmentCount: 1,
        pColorAttachmentFormats: &raw const color_format,
        ..Default::default()
    };
    let pipeline_info = vk::VkGraphicsPipelineCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
        pNext: (&raw const rendering).cast(),
        stageCount: 2,
        pStages: stages.as_ptr(),
        pVertexInputState: &raw const vertex_input,
        pInputAssemblyState: &raw const assembly,
        pViewportState: &raw const viewport,
        pRasterizationState: &raw const raster,
        pMultisampleState: &raw const multisample,
        pColorBlendState: &raw const blend,
        pDynamicState: &raw const dynamic,
        layout: resource.layout,
        basePipelineIndex: -1,
        ..Default::default()
    };
    let result = check(
        unsafe {
            device
                .functions
                .create_graphics_pipelines
                .expect("loaded function")(
                device.handle,
                ptr::null_mut(),
                1,
                &raw const pipeline_info,
                ptr::null(),
                &raw mut resource.pipeline,
            )
        },
        "vkCreateGraphicsPipelines for postprocess pipeline",
    );
    unsafe {
        device
            .functions
            .destroy_shader_module
            .expect("loaded function")(device.handle, module, ptr::null());
    }
    result?;
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
        ..Default::default()
    };
    check(
        unsafe {
            device.functions.create_sampler.expect("loaded function")(
                device.handle,
                &raw const sampler_info,
                ptr::null(),
                &raw mut resource.sampler,
            )
        },
        "vkCreateSampler for postprocess pipeline",
    )?;
    resource.descriptor_pool = create_postprocess_descriptor_pool(device)?;
    Ok(resource)
}

fn create_postprocess_descriptor_pool(
    device: &super::Device,
) -> Result<vk::VkDescriptorPool, GraphicsError> {
    create_pipeline_descriptor_pool(device, "postprocess", vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER)
}

fn create_pipeline_descriptor_pool(
    device: &super::Device,
    label: &str,
    uniform_type: vk::VkDescriptorType,
) -> Result<vk::VkDescriptorPool, GraphicsError> {
    let pool_sizes = [
        pool_size(uniform_type),
        pool_size(vk::VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE),
        pool_size(vk::VK_DESCRIPTOR_TYPE_SAMPLER),
    ];
    let pool_info = vk::VkDescriptorPoolCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
        maxSets: 64,
        poolSizeCount: 3,
        pPoolSizes: pool_sizes.as_ptr(),
        ..Default::default()
    };
    let mut pool = ptr::null_mut();
    check(
        unsafe {
            device
                .functions
                .create_descriptor_pool
                .expect("loaded function")(
                device.handle,
                &raw const pool_info,
                ptr::null(),
                &raw mut pool,
            )
        },
        &format!("vkCreateDescriptorPool for {label} pipeline"),
    )?;
    Ok(pool)
}

fn find_memory_type(device: &super::Device, compatible: u32, required: u32) -> Option<u32> {
    let mut properties = vk::VkPhysicalDeviceMemoryProperties::default();
    unsafe {
        device
            .instance
            .functions
            .get_physical_device_memory_properties
            .expect("loaded function")(device.adapter.handle, &raw mut properties);
    };
    properties.memoryTypes[..usize::try_from(properties.memoryTypeCount).ok()?]
        .iter()
        .enumerate()
        .find(|(index, memory)| {
            compatible & (1_u32 << index) != 0 && memory.propertyFlags & required == required
        })
        .and_then(|(index, _)| u32::try_from(index).ok())
}

fn destroy_buffer(surface: &ClearSurface<'_>, buffer: Buffer) {
    unsafe { destroy_buffer_device(surface.device(), buffer) }
}
unsafe fn destroy_buffer_device(device: &super::Device, buffer: Buffer) {
    unsafe {
        if !buffer.handle.is_null() {
            device.functions.destroy_buffer.expect("loaded function")(
                device.handle,
                buffer.handle,
                ptr::null(),
            );
        }
        if !buffer.memory.is_null() {
            device.functions.free_memory.expect("loaded function")(
                device.handle,
                buffer.memory,
                ptr::null(),
            );
        }
    }
}
fn destroy_image(surface: &ClearSurface<'_>, image: Image) {
    unsafe { destroy_image_device(surface.device(), image) }
}
unsafe fn destroy_image_device(device: &super::Device, image: Image) {
    unsafe {
        if !image.view.is_null() {
            device
                .functions
                .destroy_image_view
                .expect("loaded function")(device.handle, image.view, ptr::null());
        }
        if !image.handle.is_null() {
            device.functions.destroy_image.expect("loaded function")(
                device.handle,
                image.handle,
                ptr::null(),
            );
        }
        if !image.memory.is_null() {
            device.functions.free_memory.expect("loaded function")(
                device.handle,
                image.memory,
                ptr::null(),
            );
        }
    }
}

fn bytes_of<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(ptr::from_ref(value).cast(), mem::size_of::<T>()) }
}
fn bytes_of_slice<T>(values: &[T]) -> &[u8] {
    unsafe { slice::from_raw_parts(values.as_ptr().cast(), mem::size_of_val(values)) }
}
const fn depth_subresource_range() -> vk::VkImageSubresourceRange {
    vk::VkImageSubresourceRange {
        aspectMask: vk::VK_IMAGE_ASPECT_DEPTH_BIT as u32,
        baseMipLevel: 0,
        levelCount: 1,
        baseArrayLayer: 0,
        layerCount: 1,
    }
}
#[allow(clippy::too_many_arguments)]
fn image_barrier(
    image: vk::VkImage,
    old: vk::VkImageLayout,
    new: vk::VkImageLayout,
    src_stage: vk::VkPipelineStageFlags2,
    dst_stage: vk::VkPipelineStageFlags2,
    src_access: vk::VkAccessFlags2,
    dst_access: vk::VkAccessFlags2,
    range: vk::VkImageSubresourceRange,
) -> vk::VkImageMemoryBarrier2 {
    vk::VkImageMemoryBarrier2 {
        sType: vk::VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
        srcStageMask: src_stage,
        srcAccessMask: src_access,
        dstStageMask: dst_stage,
        dstAccessMask: dst_access,
        oldLayout: old,
        newLayout: new,
        srcQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
        dstQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
        image,
        subresourceRange: range,
        ..Default::default()
    }
}
fn pipeline_barrier(surface: &ClearSurface<'_>, barrier: &vk::VkImageMemoryBarrier2) {
    pipeline_barriers(surface, slice::from_ref(barrier));
}
fn pipeline_barriers(surface: &ClearSurface<'_>, barriers: &[vk::VkImageMemoryBarrier2]) {
    let dependency = vk::VkDependencyInfo {
        sType: vk::VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
        imageMemoryBarrierCount: u32::try_from(barriers.len()).expect("barrier count fits u32"),
        pImageMemoryBarriers: barriers.as_ptr(),
        ..Default::default()
    };
    unsafe {
        surface
            .device()
            .functions
            .cmd_pipeline_barrier2
            .expect("loaded function")(surface.command_buffer, &raw const dependency);
    }
}
fn descriptor_write(
    set: vk::VkDescriptorSet,
    binding: u32,
    descriptor_type: vk::VkDescriptorType,
    info: *const c_void,
) -> vk::VkWriteDescriptorSet {
    let mut write = vk::VkWriteDescriptorSet {
        sType: vk::VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
        dstSet: set,
        dstBinding: binding,
        descriptorCount: 1,
        descriptorType: descriptor_type,
        ..Default::default()
    };
    if matches!(
        descriptor_type,
        vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER | vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC
    ) {
        write.pBufferInfo = info.cast();
    } else {
        write.pImageInfo = info.cast();
    }
    write
}
const fn layout_binding(
    binding: u32,
    descriptor_type: vk::VkDescriptorType,
    stages: u32,
) -> vk::VkDescriptorSetLayoutBinding {
    vk::VkDescriptorSetLayoutBinding {
        binding,
        descriptorType: descriptor_type,
        descriptorCount: 1,
        stageFlags: stages,
        pImmutableSamplers: ptr::null(),
    }
}
const fn pool_size(descriptor_type: vk::VkDescriptorType) -> vk::VkDescriptorPoolSize {
    vk::VkDescriptorPoolSize {
        type_: descriptor_type,
        descriptorCount: 64,
    }
}
const fn attribute(
    location: u32,
    binding: u32,
    format: vk::VkFormat,
    offset: u32,
) -> vk::VkVertexInputAttributeDescription {
    vk::VkVertexInputAttributeDescription {
        location,
        binding,
        format,
        offset,
    }
}
fn shader_stage(
    stage: vk::VkShaderStageFlagBits,
    module: vk::VkShaderModule,
    name: &core::ffi::CStr,
) -> vk::VkPipelineShaderStageCreateInfo {
    vk::VkPipelineShaderStageCreateInfo {
        sType: vk::VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
        stage,
        module,
        pName: name.as_ptr(),
        ..Default::default()
    }
}
