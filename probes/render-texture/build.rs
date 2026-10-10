//! Select checked-in offline shaders for the native render texture probe.
//!
//! All three are reused unchanged: the material scene's bindingless HUD shader, which draws
//! vertex colors into the render texture; the float-texture probe's sampler, which reads one
//! texel of it at a uniform coordinate; and that probe's HDR composite, which clamps scene color
//! into the presentable target. Each has a Vulkan and a Metal artifact.
use std::{env, fs, path::PathBuf};
fn main() {
    let flavor = if env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos" {
        "metal"
    } else {
        "vulkan"
    };
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for (name, source) in [
        (
            "hud",
            format!("../../examples/material-scene/artifacts/hud.{flavor}.shaderbin"),
        ),
        (
            "sample",
            format!("../float-texture/artifacts/sample.{flavor}.shaderbin"),
        ),
        (
            "composite",
            format!("../float-texture/artifacts/composite.{flavor}.shaderbin"),
        ),
    ] {
        println!("cargo::rerun-if-changed={source}");
        fs::copy(source, output.join(format!("{name}.shaderbin"))).expect("copy native shader");
    }
}
