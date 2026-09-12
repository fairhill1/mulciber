//! Select checked-in offline shaders for the native numerical probe.
use std::{env, fs, path::PathBuf};
fn main() {
    let flavor = if env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos" {
        "metal"
    } else {
        "vulkan"
    };
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for name in ["sample", "composite"] {
        let source = format!("artifacts/{name}.{flavor}.shaderbin");
        println!("cargo::rerun-if-changed={source}");
        fs::copy(source, output.join(format!("{name}.shaderbin"))).expect("copy native shader");
    }
}
