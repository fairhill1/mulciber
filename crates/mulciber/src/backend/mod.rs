#[cfg(target_os = "macos")]
pub(crate) mod metal;
#[cfg(target_os = "macos")]
pub(crate) use metal::{BACKEND_NAME, TexturedFrameToken, TexturedSession};
#[cfg(target_os = "macos")]
pub(crate) use metal::{ClearFrame, ClearSurface};

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) mod vulkan;
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) use vulkan::{BACKEND_NAME, TexturedFrameToken, TexturedSession};
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) use vulkan::{ClearFrame, ClearSurface};
