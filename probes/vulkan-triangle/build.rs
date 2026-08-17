//! Generates both halves of the host-field probe from one WGSL source.
//!
//! The compute SPIR-V and the host-callable Rust evaluator are produced here, at build time, from
//! `src/field.wgsl`. Neither is checked in: the point of the check they support is that the two
//! evaluators cannot drift, and generating both from the one source at every build is what makes
//! that true. The generated SPIR-V is validated with the pinned `spirv-val` where it is installed.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo::rerun-if-changed=src/field.wgsl");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let source = Path::new("src/field.wgsl");

    mulciber_shader::compile_host_field(
        source,
        output.join("field_host.rs"),
        &["surface_height", "sample_direction"],
    )
    .expect("generate the host-field evaluator");

    let text = fs::read_to_string(source).expect("read the host-field WGSL");
    let module = naga::front::wgsl::parse_str(&text).expect("parse the host-field WGSL");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .expect("validate the host-field WGSL");
    let words = naga::back::spv::write_vec(
        &module,
        &info,
        &naga::back::spv::Options {
            lang_version: (1, 4),
            ..Default::default()
        },
        None,
    )
    .expect("generate host-field SPIR-V");
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    let artifact = output.join("field.comp.spv");
    fs::write(&artifact, &bytes).expect("write host-field SPIR-V");

    match Command::new("spirv-val")
        .args(["--target-env", "vulkan1.3"])
        .arg(&artifact)
        .output()
    {
        Ok(result) if result.status.success() => {}
        Ok(result) => panic!(
            "spirv-val rejected the host-field module: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ),
        Err(error) => {
            println!("cargo::warning=host-field SPIR-V was not checked with spirv-val: {error}");
        }
    }
}
