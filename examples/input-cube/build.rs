//! Selects the cube's cached native shader artifact for the input checkpoint.

fn main() {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;

    let target_os = std::env::var_os("CARGO_CFG_TARGET_OS").expect("Cargo sets target OS");
    let artifact = if target_os == OsStr::new("macos") {
        if std::env::var_os("MULCIBER_METAL_TYPECHECK_ONLY").as_deref() == Some(OsStr::new("1")) {
            let output = std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR");
            fs::write(Path::new(&output).join("cube.shaderbin"), [])
                .expect("create cross-host shader placeholder");
            println!("cargo::warning=Metal cube artifact skipped for cross-host type checking");
            return;
        }
        "../cube/artifacts/cube.metal.shaderbin"
    } else if target_os == OsStr::new("windows") || target_os == OsStr::new("linux") {
        "../cube/artifacts/cube.vulkan.shaderbin"
    } else {
        panic!("the input cube supports macOS, Windows, and Linux targets");
    };
    println!("cargo::rerun-if-changed={artifact}");
    let output = std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR");
    fs::copy(artifact, Path::new(&output).join("cube.shaderbin"))
        .expect("copy cached cube shader artifact");
}
