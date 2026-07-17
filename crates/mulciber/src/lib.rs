#![no_std]
#![doc = "Mulciber's native Vulkan and Metal graphics layer."]
#![doc = ""]
#![doc = "The API is an unstable Gate 2 extraction from validation-backed native probes."]

mod presentation;

pub use presentation::{
    FrameAcquire, FrameDisposition, SurfaceExtent, SurfaceGeneration, SurfaceInfo,
    SurfaceUnavailable,
};
