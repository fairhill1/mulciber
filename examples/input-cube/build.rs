//! Compiles the cube's WGSL into the target's native shader artifact: SPIR-V for Vulkan on Windows
//! and Linux (validated with `spirv-val`), a Metal library on macOS (built with Xcode's tools).

use std::path::PathBuf;

use mulciber_shader::{ShaderModules, ShaderTarget};

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let artifact = out.join("cube.shaderbin");
    let target = match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => ShaderTarget::Metal,
        _ => ShaderTarget::Vulkan,
    };
    println!("cargo::rerun-if-changed=src/cube.wgsl");
    println!("cargo::rerun-if-env-changed=MULCIBER_METAL_TYPECHECK_ONLY");
    // Type-checking a macOS build on another host has no Metal compiler to run.
    if target == ShaderTarget::Metal
        && std::env::var_os("MULCIBER_METAL_TYPECHECK_ONLY").is_some_and(|value| value == "1")
    {
        std::fs::write(&artifact, []).expect("OUT_DIR is writable");
        return;
    }
    ShaderModules::new()
        .shader("src/cube.wgsl")
        .expect("read src/cube.wgsl")
        .compile_wgsl(&artifact, target)
        .unwrap_or_else(|error| panic!("src/cube.wgsl: {error}"));
}
