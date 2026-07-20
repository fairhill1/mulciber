#![no_std]
#![doc = "Mulciber's native Vulkan and Metal graphics layer."]
#![doc = ""]
#![doc = "The API is an unstable Gate 2 extraction from validation-backed native probes."]

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
extern crate std;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod backend;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod clear;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod graphics;
mod presentation;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod resource;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod shader;

/// Hidden native ABI shared by Mulciber's backends and validation probes.
///
/// Applications must use the safe graphics API rather than these implementation details.
#[doc(hidden)]
pub mod integration {
    #[cfg(target_os = "macos")]
    pub use crate::backend::metal::objc as metal_objc;
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub use crate::backend::vulkan::vk as vulkan;
}

pub use presentation::{
    FrameAcquire, FrameDisposition, SurfaceExtent, SurfaceGeneration, SurfaceInfo,
    SurfaceUnavailable,
};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use clear::{ClearColor, ClearFrame, ClearSurface, GraphicsError, GraphicsErrorKind};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use graphics::{
    BlendMode, CascadedShadowPass, DepthMode, Device, DeviceRequest, DeviceSelection, Frame,
    InstancedTexturedPipeline, MATERIAL_SLOT_LIMIT, MATERIAL_STORAGE_SIZE_LIMIT,
    MATERIAL_UNIFORM_SIZE_LIMIT, MaterialBinding, MaterialPipeline, MaterialPipelineDescriptor,
    MaterialRecord, Mesh, MeshIndices, OpenedGraphics, PostprocessPipeline, PostprocessTargets,
    PostprocessedDraw, PostprocessedScene, PresentFeedback, PresentedFrame, Queue, RenderScale,
    RenderTargets, SHADOW_MAP_LAYER_LIMIT, SHADOW_MAP_SIZE_LIMIT, SampleCount, SamplerAddress,
    SamplerFilter, SceneContent, SceneOutput, SceneSubmission, ShadowMap, ShadowMapArray,
    ShadowPass, ShadowPipeline, ShadowPipelineDescriptor, ShadowPrepass, ShadowRecord,
    ShadowSource, Surface, Texture, TexturedDraw, TexturedInstanceBatch, TexturedPipeline,
    TexturedScene, TexturedSceneDraw, Vertex, VertexAttribute, VertexFormat, VertexLayout,
};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use shader::ShaderArtifact;
