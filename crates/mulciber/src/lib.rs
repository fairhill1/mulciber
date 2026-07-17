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
mod presentation;

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
pub use clear::{ClearColor, ClearFrame, ClearSurface, GraphicsError};
