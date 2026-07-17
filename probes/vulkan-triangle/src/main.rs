//! Renders a triangle through native platform windows and Vulkan 1.4 APIs.

#[cfg(target_os = "windows")]
#[path = "win32.rs"]
mod platform;
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use mulciber::integration::vulkan as vk;
#[cfg(any(target_os = "windows", target_os = "linux"))]
mod vulkan;

#[cfg(any(target_os = "windows", target_os = "linux"))]
fn main() {
    if let Err(error) = vulkan::run() {
        eprintln!("mulciber-vulkan-triangle: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn main() {
    eprintln!("mulciber-vulkan-triangle runs only on Windows and Linux");
}
