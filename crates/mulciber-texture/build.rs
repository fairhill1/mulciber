//! Links the C++ runtime for the BC7 encoder: `intel_tex_2` carries a C++ object (its ASTC
//! encoder) but does not link the runtime for it, so every binary using it would otherwise fail
//! to link. A library's link flags reach its dependents, so games baking through this crate need
//! nothing of their own.

fn main() {
    if std::env::var_os("CARGO_FEATURE_ENCODE").is_none() {
        return;
    }
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => println!("cargo::rustc-link-lib=stdc++"),
        Ok("macos") => println!("cargo::rustc-link-lib=c++"),
        _ => {}
    }
}
