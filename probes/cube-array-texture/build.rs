//! Select checked-in offline shaders for the native cube texture array probe.
//!
//! The sample shader is the probe's own, with a Vulkan and a Metal artifact; the composite pass is
//! the float-texture probe's.
use std::{env, fs, path::PathBuf};
fn main() {
    let flavor = if env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos" {
        "metal"
    } else {
        "vulkan"
    };
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for (name, source) in [
        ("sample", format!("artifacts/sample.{flavor}.shaderbin")),
        (
            "composite",
            format!("../float-texture/artifacts/composite.{flavor}.shaderbin"),
        ),
    ] {
        println!("cargo::rerun-if-changed={source}");
        fs::copy(source, output.join(format!("{name}.shaderbin"))).expect("copy native shader");
    }
}
