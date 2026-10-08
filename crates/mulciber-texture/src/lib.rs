//! Texture baking for Mulciber games: Isle of Rán's texture baker as a library.
//!
//! A [`Recipe`] names a texture's sources, how they pack into RGBA ([`Channel`]), the [`Chain`]
//! its mips are filtered with, and the `.ktx2` its bake goes to. The `encode` feature (on by
//! default) bakes it: the chain is built in RGBA8, every level is encoded as BC7 by Intel's ISPC
//! encoder, and the levels are written as KTX 2.0 with the digest of the sources they came from.
//! At run time [`Recipe::prepare`] reads the bake when it is current and otherwise builds the same
//! chain from the sources in RGBA8, so a texture edited since its bake still shows, and
//! [`Prepared::upload`] hands either to the device. A game's runtime depends on this crate with
//! `default-features = false` and never links the encoder.
//!
//! [`MaterialMaps`] is a physically based material as three such textures found beside its
//! albedo image by name: albedo, normal + roughness, metallic + occlusion.

mod chain;
mod container;
#[cfg(feature = "encode")]
mod encode;
mod material;
mod recipe;

pub use chain::{
    CUTOUT_THRESHOLD, Chain, Levels, chain_len, linear_to_srgb, mip_extent, srgb_to_linear,
};
pub use container::{
    MEAN_KEY, MIN_ALPHA_KEY, SOURCE_DIGEST_KEY, ktx2_bytes, level_bytes, write_ktx2,
};
#[cfg(feature = "encode")]
pub use encode::{Baked, bake, bake_materials, encode_bc7, run};
pub use material::{
    IMAGE_EXTENSIONS, MAP_SUFFIXES, METALLIC_SUFFIX, MaterialDefaults, MaterialMaps, NORMAL_SUFFIX,
    OCCLUSION_SUFFIX, ROUGHNESS_SUFFIX, find_materials, is_albedo,
};
pub use recipe::{Channel, Fallback, Origin, Pixels, Prepared, Recipe, Stats};

use std::fmt;
use std::path::Path;

/// A texture that could not be read, built, baked or written, with what and where.
#[derive(Clone, PartialEq, Eq)]
pub struct TextureError {
    message: String,
}

impl TextureError {
    /// An error with this message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The message, prefixed with the file it concerns.
    #[must_use]
    pub fn at(self, path: &Path) -> Self {
        Self::new(format!("{}: {}", path.display(), self.message))
    }

    /// The message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for TextureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

// Printed as written, so `expect` and `?` in `main` stay readable.
impl fmt::Debug for TextureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TextureError {}
