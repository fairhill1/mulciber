//! Reports Vulkan device and native presentation capabilities relevant to Mulciber.

#![allow(clippy::missing_errors_doc)]

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod native;
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod native;
#[cfg(any(target_os = "windows", target_os = "linux"))]
#[path = "../../vulkan-triangle/src/vk.rs"]
mod vk;

mod report;

#[cfg(any(target_os = "windows", target_os = "linux"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    report::run().map_err(Into::into)
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn main() {
    eprintln!("mulciber-vulkan-info is available only on Windows and Linux");
}
