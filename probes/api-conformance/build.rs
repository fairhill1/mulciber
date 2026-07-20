//! Selects cached native artifacts for conformance validation.

fn main() {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;

    let target_os = std::env::var_os("CARGO_CFG_TARGET_OS").expect("Cargo sets target OS");
    let names = [
        "cube.shaderbin",
        "instanced.shaderbin",
        "material.shaderbin",
        "lava.shaderbin",
        "shadow.shaderbin",
    ];
    let flavor = if target_os == OsStr::new("macos") {
        if std::env::var_os("MULCIBER_METAL_TYPECHECK_ONLY").as_deref() == Some(OsStr::new("1")) {
            let output = std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR");
            for name in names {
                fs::write(Path::new(&output).join(name), [])
                    .expect("create cross-host shader placeholder");
            }
            println!(
                "cargo::warning=Metal conformance artifact skipped for cross-host type checking"
            );
            return;
        }
        "metal"
    } else if target_os == OsStr::new("windows") || target_os == OsStr::new("linux") {
        "vulkan"
    } else {
        panic!("the conformance probe supports macOS, Windows, and Linux targets");
    };
    let artifacts = [
        format!("../../examples/postprocess-cube/artifacts/postprocess.{flavor}.shaderbin"),
        format!("../../examples/instanced-scene/artifacts/instanced.{flavor}.shaderbin"),
        format!("../../examples/material-scene/artifacts/crystal.{flavor}.shaderbin"),
        format!("../../examples/material-scene/artifacts/lava.{flavor}.shaderbin"),
        format!("../../examples/material-scene/artifacts/shadow.{flavor}.shaderbin"),
    ];
    let output = std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR");
    for (artifact, name) in artifacts.iter().zip(names) {
        println!("cargo::rerun-if-changed={artifact}");
        fs::copy(artifact, Path::new(&output).join(name)).expect("copy cached shader artifact");
    }
}
