//! Selects the instancing example's cached native shader artifact.

fn main() {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;

    let target_os = std::env::var_os("CARGO_CFG_TARGET_OS").expect("Cargo sets target OS");
    let artifact = if target_os == OsStr::new("macos") {
        "../instanced-scene/artifacts/instanced.metal.shaderbin"
    } else if target_os == OsStr::new("windows") || target_os == OsStr::new("linux") {
        "../instanced-scene/artifacts/instanced.vulkan.shaderbin"
    } else {
        panic!("the game slice supports macOS, Windows, and Linux targets");
    };
    println!("cargo::rerun-if-changed={artifact}");
    let output = std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR");
    fs::copy(artifact, Path::new(&output).join("game.shaderbin"))
        .expect("copy cached game shader artifact");
}
