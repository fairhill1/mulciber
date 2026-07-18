//! Selects cached native artifacts for conformance validation.

fn main() {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;

    let target_os = std::env::var_os("CARGO_CFG_TARGET_OS").expect("Cargo sets target OS");
    let (cube_artifact, instanced_artifact) = if target_os == OsStr::new("macos") {
        if std::env::var_os("MULCIBER_METAL_TYPECHECK_ONLY").as_deref() == Some(OsStr::new("1")) {
            let output = std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR");
            fs::write(Path::new(&output).join("cube.shaderbin"), [])
                .expect("create cross-host shader placeholder");
            fs::write(Path::new(&output).join("instanced.shaderbin"), [])
                .expect("create cross-host shader placeholder");
            println!(
                "cargo::warning=Metal conformance artifact skipped for cross-host type checking"
            );
            return;
        }
        (
            "../../examples/postprocess-cube/artifacts/postprocess.metal.shaderbin",
            "../../examples/instanced-scene/artifacts/instanced.metal.shaderbin",
        )
    } else if target_os == OsStr::new("windows") || target_os == OsStr::new("linux") {
        (
            "../../examples/postprocess-cube/artifacts/postprocess.vulkan.shaderbin",
            "../../examples/instanced-scene/artifacts/instanced.vulkan.shaderbin",
        )
    } else {
        panic!("the conformance probe supports macOS, Windows, and Linux targets");
    };
    println!("cargo::rerun-if-changed={cube_artifact}");
    println!("cargo::rerun-if-changed={instanced_artifact}");
    let output = std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR");
    fs::copy(cube_artifact, Path::new(&output).join("cube.shaderbin"))
        .expect("copy cached cube shader artifact");
    fs::copy(
        instanced_artifact,
        Path::new(&output).join("instanced.shaderbin"),
    )
    .expect("copy cached instanced shader artifact");
}
