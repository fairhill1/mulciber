//! Compiles every WGSL shader in `shaders/` into the target's native artifact (SPIR-V for Vulkan
//! on Windows and Linux, a Metal library on macOS), as a game would.
//!
//! The modules in `shaders/shared/` name themselves with `#define_import_path` and the shaders
//! `#import` them, as they do Mulciber's own `mulciber::` modules.

use std::path::PathBuf;

use mulciber_shader::{ShaderModules, ShaderTarget};

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let target = match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => ShaderTarget::Metal,
        _ => ShaderTarget::Vulkan,
    };
    // Type-checking a macOS build on another host has no Metal compiler to run.
    let typecheck_only = target == ShaderTarget::Metal
        && std::env::var_os("MULCIBER_METAL_TYPECHECK_ONLY").is_some_and(|value| value == "1");
    println!("cargo::rerun-if-env-changed=MULCIBER_METAL_TYPECHECK_ONLY");

    let mut modules = ShaderModules::new();
    modules
        .add_dir("shaders/shared")
        .unwrap_or_else(|error| panic!("shaders/shared: {error}"));
    modules.rerun_if_changed();
    println!("cargo::rerun-if-changed=shaders");
    for entry in std::fs::read_dir("shaders").expect("shaders/ exists") {
        let path = entry.expect("readable shaders/ entry").path();
        if path.extension().is_none_or(|extension| extension != "wgsl") {
            continue;
        }
        let name = path
            .file_stem()
            .expect("a .wgsl file has a stem")
            .to_string_lossy();
        let artifact = out.join(format!("{name}.shaderbin"));
        if typecheck_only {
            std::fs::write(&artifact, []).expect("OUT_DIR is writable");
            continue;
        }
        modules
            .shader(&path)
            .and_then(|shader| shader.compile_wgsl(&artifact, target))
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
}
