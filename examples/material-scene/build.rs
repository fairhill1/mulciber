//! Selects cached native artifacts generated from the two application-authored WGSL modules.

fn main() {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;

    let target_os = std::env::var_os("CARGO_CFG_TARGET_OS").expect("Cargo sets target OS");
    let output = std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR");
    let modules = [
        "crystal",
        "hud",
        "lava",
        "shadow",
        "skinned",
        "skinned-shadow",
    ];
    let flavor = if target_os == OsStr::new("macos") {
        if std::env::var_os("MULCIBER_METAL_TYPECHECK_ONLY").as_deref() == Some(OsStr::new("1")) {
            for module in modules {
                fs::write(Path::new(&output).join(format!("{module}.shaderbin")), [])
                    .expect("create cross-host shader placeholder");
            }
            println!(
                "cargo::warning=Metal material artifacts skipped for cross-host type checking"
            );
            return;
        }
        "metal"
    } else if target_os == OsStr::new("windows") || target_os == OsStr::new("linux") {
        "vulkan"
    } else {
        panic!("the material checkpoint supports macOS, Windows, and Linux targets");
    };
    for module in modules {
        let artifact = format!("artifacts/{module}.{flavor}.shaderbin");
        println!("cargo::rerun-if-changed={artifact}");
        fs::copy(
            &artifact,
            Path::new(&output).join(format!("{module}.shaderbin")),
        )
        .expect("copy cached material shader artifact");
    }
}
