//! Compiles the probe's MSL source into an embedded metallib.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo::rerun-if-changed=src/shader.metal");
    if env::var_os("CARGO_CFG_TARGET_OS").as_deref() != Some(OsStr::new("macos")) {
        return;
    }

    let manifest = env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR");
    let output = env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR");
    let source = Path::new(&manifest).join("src/shader.metal");
    let output = Path::new(&output);
    let air = output.join("shader.air");
    let metallib = output.join("shader.metallib");
    let module_cache = output.join("metal-module-cache");
    fs::create_dir_all(&module_cache).expect("create Metal module cache");

    run(
        Command::new("xcrun")
            .args(["-sdk", "macosx", "metal"])
            .arg(format!("-fmodules-cache-path={}", module_cache.display()))
            .args(["-std=metal3.0", "-mmacosx-version-min=13.0", "-c"])
            .arg(&source)
            .arg("-o")
            .arg(&air),
        "compile MSL to AIR",
    );
    run(
        Command::new("xcrun")
            .args(["-sdk", "macosx", "metallib"])
            .arg(&air)
            .arg("-o")
            .arg(&metallib),
        "link AIR into a metallib",
    );
}

fn run(command: &mut Command, action: &str) {
    let status = command.status().unwrap_or_else(|error| {
        panic!("could not {action}: {error}");
    });
    assert!(status.success(), "failed to {action}: {status}");
}
