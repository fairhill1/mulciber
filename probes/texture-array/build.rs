//! Select checked-in offline shaders for the native 2D texture array probe.
//!
//! The sample shader is the probe's own; the composite pass is the float-texture probe's. A Metal
//! sample artifact is built on a Mac (`mulciber-shader build src/sample.wgsl --target metal`);
//! until one is checked in, the probe builds there and reports it missing when run.
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
        let bytes = fs::read(&source).unwrap_or_default();
        fs::write(output.join(format!("{name}.shaderbin")), bytes).expect("write native shader");
    }
}
