use core::ffi::c_void;
use core::{mem, ptr};
use std::{format, vec::Vec};

use mulciber_platform::{SurfaceTarget, WindowMetrics};

use super::{ClearSurface, MetalFrameToken, objc, required};
use crate::{
    ClearColor, DeviceRequest, FrameAcquire, FrameDisposition, GraphicsError, SampleCount,
    ShaderArtifact, SurfaceInfo, Vertex,
};

use objc::{Object, Origin3, Region3, Size3};

const PIXEL_FORMAT_BGRA8_UNORM_SRGB: usize = 81;
const PIXEL_FORMAT_RGBA8_UNORM_SRGB: usize = 71;
const PIXEL_FORMAT_DEPTH32_FLOAT: usize = 252;
const VERTEX_FORMAT_FLOAT2: usize = 29;
const VERTEX_FORMAT_FLOAT3: usize = 30;
const LOAD_ACTION_CLEAR: usize = 2;
const STORE_ACTION_STORE: usize = 1;
const STORE_ACTION_DONT_CARE: usize = 0;
const STORE_ACTION_MULTISAMPLE_RESOLVE: usize = 2;
const PRIMITIVE_TYPE_TRIANGLE: usize = 3;
const INDEX_TYPE_UINT16: usize = 0;
const STORAGE_MODE_PRIVATE: usize = 2;
const STORAGE_MODE_MEMORYLESS: usize = 3;
const TEXTURE_TYPE_2D_MULTISAMPLE: usize = 4;
const TEXTURE_USAGE_SHADER_READ: usize = 1;
const TEXTURE_USAGE_RENDER_TARGET: usize = 4;
const COMPARE_FUNCTION_LESS: usize = 1;
const SAMPLER_FILTER_LINEAR: usize = 1;
const SAMPLER_ADDRESS_REPEAT: usize = 2;

#[link(name = "System")]
unsafe extern "C" {
    fn dispatch_data_create(
        buffer: *const c_void,
        size: usize,
        queue: Object,
        destructor: Object,
    ) -> Object;
}

struct MeshResource {
    vertices: Object,
    indices: Object,
    indirect: Object,
}

struct TextureResource {
    texture: Object,
    sampler: Object,
}

struct PipelineResource {
    pipeline: Object,
    depth_state: Object,
}

struct TargetResource {
    info: SurfaceInfo,
    multisample_color: Object,
    depth: Object,
}

pub(crate) struct TexturedFrameToken(MetalFrameToken);

impl TexturedFrameToken {
    pub(crate) const fn info(&self) -> SurfaceInfo {
        self.0.info
    }
}

pub(crate) struct TexturedSession<'window> {
    surface: ClearSurface<'window>,
    sample_count: usize,
    uniform: Object,
    meshes: Vec<MeshResource>,
    textures: Vec<TextureResource>,
    pipelines: Vec<PipelineResource>,
    targets: Vec<TargetResource>,
    deferred_token: Option<TexturedFrameToken>,
}

impl<'window> TexturedSession<'window> {
    pub(crate) fn new(
        target: SurfaceTarget<'window>,
        metrics: WindowMetrics,
        request: DeviceRequest,
    ) -> Result<(Self, SampleCount), GraphicsError> {
        let surface = ClearSurface::new(target, metrics)?;
        let sample_count = if request.preferred_sample_count == SampleCount::Four
            && unsafe { objc::bool_usize(surface.device, c"supportsTextureSampleCount:", 4) }
        {
            4
        } else {
            1
        };
        let uniform = unsafe {
            required(
                objc::object_two_usizes(surface.device, c"newBufferWithLength:options:", 64, 0),
                "Metal cube uniform buffer",
            )?
        };
        Ok((
            Self {
                surface,
                sample_count,
                uniform,
                meshes: Vec::new(),
                textures: Vec::new(),
                pipelines: Vec::new(),
                targets: Vec::new(),
                deferred_token: None,
            },
            if sample_count == 4 {
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
        let acquisition = self.surface.acquire_drawable(metrics)?;
        self.reclaim_stale_targets();
        Ok(acquisition.map_ready(TexturedFrameToken))
    }

    pub(crate) fn create_mesh(
        &mut self,
        vertices: &[Vertex],
        indices: &[u16],
    ) -> Result<u32, GraphicsError> {
        let draw = IndexedIndirectArguments {
            index_count: u32::try_from(indices.len())
                .map_err(|_| GraphicsError::new("mesh index count exceeds u32"))?,
            instance_count: 1,
            index_start: 0,
            base_vertex: 0,
            base_instance: 0,
        };
        unsafe {
            let vertices = required(
                objc::object_bytes(
                    self.surface.device,
                    c"newBufferWithBytes:length:options:",
                    vertices.as_ptr().cast(),
                    mem::size_of_val(vertices),
                    0,
                ),
                "Metal cube vertex buffer",
            )?;
            let indices = required(
                objc::object_bytes(
                    self.surface.device,
                    c"newBufferWithBytes:length:options:",
                    indices.as_ptr().cast(),
                    mem::size_of_val(indices),
                    0,
                ),
                "Metal cube index buffer",
            )?;
            let indirect = required(
                objc::object_bytes(
                    self.surface.device,
                    c"newBufferWithBytes:length:options:",
                    ptr::from_ref(&draw).cast(),
                    mem::size_of_val(&draw),
                    0,
                ),
                "Metal cube indirect buffer",
            )?;
            self.meshes.push(MeshResource {
                vertices,
                indices,
                indirect,
            });
        }
        resource_id(self.meshes.len(), "mesh")
    }

    pub(crate) fn create_texture(
        &mut self,
        width: u32,
        height: u32,
        texels: &[u8],
    ) -> Result<u32, GraphicsError> {
        let width = usize::try_from(width)
            .map_err(|_| GraphicsError::new("texture width exceeds usize"))?;
        let height = usize::try_from(height)
            .map_err(|_| GraphicsError::new("texture height exceeds usize"))?;
        unsafe {
            let descriptor = required(
                objc::object_three_usizes_bool(
                    objc::class(c"MTLTextureDescriptor"),
                    c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
                    PIXEL_FORMAT_RGBA8_UNORM_SRGB,
                    width,
                    height,
                    false,
                ),
                "Metal cube texture descriptor",
            )?;
            objc::void_usize(descriptor, c"setUsage:", TEXTURE_USAGE_SHADER_READ);
            let texture = required(
                objc::object_object(
                    self.surface.device,
                    c"newTextureWithDescriptor:",
                    descriptor,
                ),
                "Metal cube texture",
            )?;
            objc::void_region_usize_bytes_usize(
                texture,
                c"replaceRegion:mipmapLevel:withBytes:bytesPerRow:",
                Region3 {
                    origin: Origin3 { x: 0, y: 0, z: 0 },
                    size: Size3 {
                        width,
                        height,
                        depth: 1,
                    },
                },
                0,
                texels.as_ptr().cast(),
                width * 4,
            );
            let sampler_descriptor = required(
                objc::object(objc::class(c"MTLSamplerDescriptor"), c"new"),
                "Metal sampler descriptor",
            )?;
            objc::void_usize(sampler_descriptor, c"setMinFilter:", SAMPLER_FILTER_LINEAR);
            objc::void_usize(sampler_descriptor, c"setMagFilter:", SAMPLER_FILTER_LINEAR);
            objc::void_usize(
                sampler_descriptor,
                c"setSAddressMode:",
                SAMPLER_ADDRESS_REPEAT,
            );
            objc::void_usize(
                sampler_descriptor,
                c"setTAddressMode:",
                SAMPLER_ADDRESS_REPEAT,
            );
            let sampler = required(
                objc::object_object(
                    self.surface.device,
                    c"newSamplerStateWithDescriptor:",
                    sampler_descriptor,
                ),
                "Metal cube sampler",
            )?;
            objc::void(sampler_descriptor, c"release");
            self.textures.push(TextureResource { texture, sampler });
        }
        resource_id(self.textures.len(), "texture")
    }

    pub(crate) fn create_pipeline(
        &mut self,
        shader: ShaderArtifact<'_>,
    ) -> Result<u32, GraphicsError> {
        self.pipelines.push(create_pipeline(
            self.surface.device,
            shader.payload(),
            self.sample_count,
        )?);
        resource_id(self.pipelines.len(), "pipeline")
    }

    /// Releases the session's references to render targets from superseded surface generations.
    ///
    /// Draws reject targets that do not match the acquired generation, and committed command
    /// buffers retain the textures they reference, so releasing the session's reference cannot
    /// free storage still owned by in-flight GPU work. Reclaimed entries keep their identifiers
    /// with null textures and are rejected if drawn.
    fn reclaim_stale_targets(&mut self) {
        let current = self.surface.info().generation();
        for target in &mut self.targets {
            if target.info.generation().get() >= current.get() || target.depth.is_null() {
                continue;
            }
            unsafe {
                if !target.multisample_color.is_null() {
                    objc::void(target.multisample_color, c"release");
                }
                objc::void(target.depth, c"release");
            }
            target.multisample_color = ptr::null_mut();
            target.depth = ptr::null_mut();
        }
    }

    pub(crate) fn create_render_targets(
        &mut self,
        info: SurfaceInfo,
    ) -> Result<u32, GraphicsError> {
        self.reclaim_stale_targets();
        let width = usize::try_from(info.extent().width())
            .map_err(|_| GraphicsError::new("target width exceeds usize"))?;
        let height = usize::try_from(info.extent().height())
            .map_err(|_| GraphicsError::new("target height exceeds usize"))?;
        let depth = create_target_texture(
            self.surface.device,
            PIXEL_FORMAT_DEPTH32_FLOAT,
            width,
            height,
            self.sample_count,
        )?;
        let multisample_color = if self.sample_count == 4 {
            match create_target_texture(
                self.surface.device,
                PIXEL_FORMAT_BGRA8_UNORM_SRGB,
                width,
                height,
                4,
            ) {
                Ok(color) => color,
                Err(failure) => {
                    unsafe { objc::void(depth, c"release") };
                    return Err(failure);
                }
            }
        } else {
            ptr::null_mut()
        };
        self.targets.push(TargetResource {
            info,
            multisample_color,
            depth,
        });
        resource_id(self.targets.len(), "render target")
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_and_present(
        &mut self,
        token: TexturedFrameToken,
        mesh: u32,
        texture: u32,
        pipeline: u32,
        targets: u32,
        transform: [[f32; 4]; 4],
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        let mesh = resource_index(mesh, self.meshes.len(), "mesh")?;
        let texture = resource_index(texture, self.textures.len(), "texture")?;
        let pipeline = resource_index(pipeline, self.pipelines.len(), "pipeline")?;
        let target = resource_index(targets, self.targets.len(), "render target")?;
        if self.targets[target].info != token.info() {
            return Err(GraphicsError::new(
                "render targets do not match acquired Metal generation",
            ));
        }
        if self.targets[target].depth.is_null() {
            return Err(GraphicsError::new(
                "render targets were reclaimed by a newer surface generation",
            ));
        }
        unsafe {
            let contents = objc::pointer_value(self.uniform, c"contents");
            if contents.is_null() {
                return Err(GraphicsError::new(
                    "Metal uniform buffer has no CPU address",
                ));
            }
            ptr::copy_nonoverlapping(
                ptr::from_ref(&transform).cast::<u8>(),
                contents.cast::<u8>(),
                64,
            );
        }
        self.encode_present(token, mesh, texture, pipeline, target, clear)
    }

    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    pub(crate) fn abandon(
        &mut self,
        token: TexturedFrameToken,
    ) -> Result<FrameDisposition, GraphicsError> {
        let generation = token.info().generation();
        drop(token);
        Ok(FrameDisposition::Abandoned(generation))
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

    pub(crate) fn shutdown(mut self) -> Result<(), GraphicsError> {
        self.flush_deferred_abandon()?;
        let result = self.surface.finish_last_submission();
        self.destroy_resources();
        let surface = unsafe { ptr::read(&raw const self.surface) };
        mem::forget(self);
        result.and(surface.shutdown())
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn encode_present(
        &mut self,
        mut token: TexturedFrameToken,
        mesh: usize,
        texture: usize,
        pipeline: usize,
        target: usize,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        unsafe {
            let drawable = token.0.drawable;
            let drawable_texture = required(
                objc::object(drawable, c"texture"),
                "Metal cube drawable texture",
            )?;
            let pass = required(
                objc::object(
                    objc::class(c"MTLRenderPassDescriptor"),
                    c"renderPassDescriptor",
                ),
                "Metal cube render-pass descriptor",
            )?;
            let attachments =
                required(objc::object(pass, c"colorAttachments"), "color attachments")?;
            let color = required(
                objc::object_usize(attachments, c"objectAtIndexedSubscript:", 0),
                "color attachment zero",
            )?;
            let targets = &self.targets[target];
            if self.sample_count == 4 {
                objc::void_object(color, c"setTexture:", targets.multisample_color);
                objc::void_object(color, c"setResolveTexture:", drawable_texture);
                objc::void_usize(color, c"setStoreAction:", STORE_ACTION_MULTISAMPLE_RESOLVE);
            } else {
                objc::void_object(color, c"setTexture:", drawable_texture);
                objc::void_usize(color, c"setStoreAction:", STORE_ACTION_STORE);
            }
            objc::void_usize(color, c"setLoadAction:", LOAD_ACTION_CLEAR);
            let [red, green, blue, alpha] = clear.components();
            objc::void_clear_color(
                color,
                c"setClearColor:",
                objc::ClearColor {
                    red: f64::from(red),
                    green: f64::from(green),
                    blue: f64::from(blue),
                    alpha: f64::from(alpha),
                },
            );
            let depth = required(objc::object(pass, c"depthAttachment"), "depth attachment")?;
            objc::void_object(depth, c"setTexture:", targets.depth);
            objc::void_usize(depth, c"setLoadAction:", LOAD_ACTION_CLEAR);
            objc::void_usize(depth, c"setStoreAction:", STORE_ACTION_DONT_CARE);
            objc::void_f64(depth, c"setClearDepth:", 1.0);
            let command = required(
                objc::object(self.surface.queue, c"commandBuffer"),
                "Metal cube command buffer",
            )?;
            let encoder = required(
                objc::object_object(command, c"renderCommandEncoderWithDescriptor:", pass),
                "Metal cube render encoder",
            )?;
            let pipeline = &self.pipelines[pipeline];
            let mesh = &self.meshes[mesh];
            let texture = &self.textures[texture];
            objc::void_object(encoder, c"setRenderPipelineState:", pipeline.pipeline);
            objc::void_object(encoder, c"setDepthStencilState:", pipeline.depth_state);
            objc::void_object_two_usizes(
                encoder,
                c"setVertexBuffer:offset:atIndex:",
                self.uniform,
                0,
                0,
            );
            objc::void_object_two_usizes(
                encoder,
                c"setVertexBuffer:offset:atIndex:",
                mesh.vertices,
                0,
                1,
            );
            objc::void_object_usize(encoder, c"setFragmentTexture:atIndex:", texture.texture, 1);
            objc::void_object_usize(
                encoder,
                c"setFragmentSamplerState:atIndex:",
                texture.sampler,
                2,
            );
            objc::void_two_usizes_object_usize_object_usize(
                encoder,
                c"drawIndexedPrimitives:indexType:indexBuffer:indexBufferOffset:indirectBuffer:indirectBufferOffset:",
                PRIMITIVE_TYPE_TRIANGLE,
                INDEX_TYPE_UINT16,
                mesh.indices,
                0,
                mesh.indirect,
                0,
            );
            objc::void(encoder, c"endEncoding");
            objc::void_object(command, c"presentDrawable:", drawable);
            objc::void(command, c"retain");
            objc::void(command, c"commit");
            self.surface.last_command_buffer = command;
            token.0.drawable = ptr::null_mut();
        }
        Ok(FrameDisposition::Presented(token.info().generation()))
    }

    fn destroy_resources(&mut self) {
        unsafe {
            for pipeline in self.pipelines.drain(..) {
                objc::void(pipeline.depth_state, c"release");
                objc::void(pipeline.pipeline, c"release");
            }
            for texture in self.textures.drain(..) {
                objc::void(texture.sampler, c"release");
                objc::void(texture.texture, c"release");
            }
            for target in self.targets.drain(..) {
                if !target.multisample_color.is_null() {
                    objc::void(target.multisample_color, c"release");
                }
                if !target.depth.is_null() {
                    objc::void(target.depth, c"release");
                }
            }
            for mesh in self.meshes.drain(..) {
                objc::void(mesh.indirect, c"release");
                objc::void(mesh.indices, c"release");
                objc::void(mesh.vertices, c"release");
            }
            if !self.uniform.is_null() {
                objc::void(self.uniform, c"release");
                self.uniform = ptr::null_mut();
            }
        }

        // `shutdown` moves the surface out and deliberately suppresses this
        // session's destructor. Release the now-empty arenas' allocations here
        // so that path does not retain their backing storage.
        self.pipelines = Vec::new();
        self.textures = Vec::new();
        self.targets = Vec::new();
        self.meshes = Vec::new();
    }
}

impl Drop for TexturedSession<'_> {
    fn drop(&mut self) {
        let _ = self.surface.finish_last_submission();
        self.destroy_resources();
    }
}

#[repr(C)]
struct IndexedIndirectArguments {
    index_count: u32,
    instance_count: u32,
    index_start: u32,
    base_vertex: i32,
    base_instance: u32,
}

fn create_pipeline(
    device: Object,
    bytes: &[u8],
    sample_count: usize,
) -> Result<PipelineResource, GraphicsError> {
    unsafe {
        let data = required(
            dispatch_data_create(
                bytes.as_ptr().cast(),
                bytes.len(),
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            "Metal cube library data",
        )?;
        let mut library_error = ptr::null_mut();
        let library = objc::object_object_out(
            device,
            c"newLibraryWithData:error:",
            data,
            &raw mut library_error,
        );
        if library.is_null() {
            return Err(GraphicsError::new(format!(
                "loading cube metallib failed: {}",
                objc::description(library_error)
            )));
        }
        let vertex = required(
            objc::object_object(
                library,
                c"newFunctionWithName:",
                objc::ns_string(c"cube_vertex"),
            ),
            "Metal cube vertex function",
        )?;
        let fragment = required(
            objc::object_object(
                library,
                c"newFunctionWithName:",
                objc::ns_string(c"cube_fragment"),
            ),
            "Metal cube fragment function",
        )?;
        let descriptor = required(
            objc::object(objc::class(c"MTLRenderPipelineDescriptor"), c"new"),
            "Metal cube pipeline descriptor",
        )?;
        objc::void_object(descriptor, c"setVertexFunction:", vertex);
        objc::void_object(descriptor, c"setFragmentFunction:", fragment);
        objc::void_usize(descriptor, c"setSampleCount:", sample_count);
        objc::void_usize(
            descriptor,
            c"setDepthAttachmentPixelFormat:",
            PIXEL_FORMAT_DEPTH32_FLOAT,
        );
        let colors = required(
            objc::object(descriptor, c"colorAttachments"),
            "pipeline colors",
        )?;
        let color = required(
            objc::object_usize(colors, c"objectAtIndexedSubscript:", 0),
            "pipeline color zero",
        )?;
        objc::void_usize(color, c"setPixelFormat:", PIXEL_FORMAT_BGRA8_UNORM_SRGB);
        configure_vertex_descriptor(descriptor)?;
        let mut pipeline_error = ptr::null_mut();
        let pipeline = objc::object_object_out(
            device,
            c"newRenderPipelineStateWithDescriptor:error:",
            descriptor,
            &raw mut pipeline_error,
        );
        if pipeline.is_null() {
            return Err(GraphicsError::new(format!(
                "creating Metal cube pipeline failed: {}",
                objc::description(pipeline_error)
            )));
        }
        let depth_descriptor = required(
            objc::object(objc::class(c"MTLDepthStencilDescriptor"), c"new"),
            "Metal depth descriptor",
        )?;
        objc::void_usize(
            depth_descriptor,
            c"setDepthCompareFunction:",
            COMPARE_FUNCTION_LESS,
        );
        objc::void_bool(depth_descriptor, c"setDepthWriteEnabled:", true);
        let depth_state = required(
            objc::object_object(
                device,
                c"newDepthStencilStateWithDescriptor:",
                depth_descriptor,
            ),
            "Metal cube depth state",
        )?;
        for object in [depth_descriptor, descriptor, fragment, vertex, library] {
            objc::void(object, c"release");
        }
        Ok(PipelineResource {
            pipeline,
            depth_state,
        })
    }
}

unsafe fn configure_vertex_descriptor(descriptor: Object) -> Result<(), GraphicsError> {
    unsafe {
        let vertex = required(
            objc::object(objc::class(c"MTLVertexDescriptor"), c"vertexDescriptor"),
            "Metal vertex descriptor",
        )?;
        let attributes = required(objc::object(vertex, c"attributes"), "vertex attributes")?;
        for (index, format, offset) in [
            (0, VERTEX_FORMAT_FLOAT3, 0),
            (1, VERTEX_FORMAT_FLOAT3, 12),
            (2, VERTEX_FORMAT_FLOAT2, 24),
        ] {
            let attribute = required(
                objc::object_usize(attributes, c"objectAtIndexedSubscript:", index),
                "Metal vertex attribute",
            )?;
            objc::void_usize(attribute, c"setFormat:", format);
            objc::void_usize(attribute, c"setOffset:", offset);
            objc::void_usize(attribute, c"setBufferIndex:", 1);
        }
        let layouts = required(objc::object(vertex, c"layouts"), "vertex layouts")?;
        let layout = required(
            objc::object_usize(layouts, c"objectAtIndexedSubscript:", 1),
            "vertex layout one",
        )?;
        objc::void_usize(layout, c"setStride:", mem::size_of::<Vertex>());
        objc::void_object(descriptor, c"setVertexDescriptor:", vertex);
        Ok(())
    }
}

fn create_target_texture(
    device: Object,
    format: usize,
    width: usize,
    height: usize,
    sample_count: usize,
) -> Result<Object, GraphicsError> {
    unsafe {
        let descriptor = required(
            objc::object_three_usizes_bool(
                objc::class(c"MTLTextureDescriptor"),
                c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
                format,
                width,
                height,
                false,
            ),
            "Metal target texture descriptor",
        )?;
        if sample_count == 4 {
            objc::void_usize(descriptor, c"setTextureType:", TEXTURE_TYPE_2D_MULTISAMPLE);
            objc::void_usize(descriptor, c"setSampleCount:", 4);
            objc::void_usize(descriptor, c"setStorageMode:", STORAGE_MODE_MEMORYLESS);
        } else {
            objc::void_usize(descriptor, c"setStorageMode:", STORAGE_MODE_PRIVATE);
        }
        objc::void_usize(descriptor, c"setUsage:", TEXTURE_USAGE_RENDER_TARGET);
        required(
            objc::object_object(device, c"newTextureWithDescriptor:", descriptor),
            "Metal render target texture",
        )
    }
}

fn resource_id(length: usize, label: &str) -> Result<u32, GraphicsError> {
    u32::try_from(length)
        .map_err(|_| GraphicsError::new(format!("{label} identity space is exhausted")))
}

fn resource_index(id: u32, length: usize, label: &str) -> Result<usize, GraphicsError> {
    let index = usize::try_from(id)
        .ok()
        .and_then(|id| id.checked_sub(1))
        .ok_or_else(|| GraphicsError::new(format!("invalid {label} handle")))?;
    (index < length)
        .then_some(index)
        .ok_or_else(|| GraphicsError::new(format!("invalid {label} handle")))
}
