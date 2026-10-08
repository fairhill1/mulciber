//! Select checked-in offline shaders for the native cube texture probe.
//!
//! The composite pass is the float-texture probe's. The sample shader's Metal artifact has not
//! been generated yet (it needs Xcode's `metal` tools): a macOS build without it gets an empty
//! placeholder and a warning, so workspace checks still pass, and the probe reports the missing
//! artifact at startup instead of rendering.
use std::{env, fs, path::Path, path::PathBuf};
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
        let destination = output.join(format!("{name}.shaderbin"));
        if flavor == "metal" && !Path::new(&source).exists() {
            fs::write(destination, []).expect("write missing-artifact placeholder");
            println!("cargo::warning={source} is not generated yet; the probe will not render");
            continue;
        }
        fs::copy(source, destination).expect("copy native shader");
    }
}
