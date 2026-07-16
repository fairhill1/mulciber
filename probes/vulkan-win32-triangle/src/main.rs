//! Renders a triangle through native Win32 and Vulkan 1.4 APIs.

#[cfg(target_os = "windows")]
mod vk;
#[cfg(target_os = "windows")]
mod vulkan;
#[cfg(target_os = "windows")]
mod win32;

#[cfg(target_os = "windows")]
fn main() {
    if let Err(error) = vulkan::run() {
        eprintln!("mulciber-vulkan-win32-triangle: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("mulciber-vulkan-win32-triangle only runs on Windows");
}
