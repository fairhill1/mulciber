//! A physically based material as three textures, found beside its albedo by name.
//!
//! | Texture | Channels | Chain | Bake |
//! | --- | --- | --- | --- |
//! | albedo | sRGB colour, alpha = opacity | [`Chain::Color`] | `name.ktx2`, BC7 sRGB |
//! | normal + roughness | tangent-space normal (OpenGL: +Y up, green up), alpha = perceptual roughness | [`Chain::Normal`] | `name_normal.ktx2`, BC7 |
//! | metallic + occlusion | R = metallic, G = ambient occlusion, B = 0, A = 1 | [`Chain::Linear`] | `name_metal.ktx2`, BC7 |
//!
//! The normal + roughness packing is Isle of Rán's. The sources sit beside the albedo image
//! (`name.png`): `name_normal`, `name_rough`, `name_metal` and `name_ao`, each `.png`, `.jpg` or
//! `.jpeg`. The roughness, metallic and occlusion maps are grey. Any of the four may be missing and
//! is filled from [`MaterialDefaults`]; a texture none of whose sources exist is not baked at all,
//! and the game binds a 1×1 texture of the defaults ([`MaterialDefaults::normal_roughness_texel`],
//! [`MaterialDefaults::metallic_occlusion_texel`]) in its place.

use std::path::{Path, PathBuf};

use crate::TextureError;
use crate::chain::Chain;
use crate::recipe::{Channel, Recipe};

/// The suffix of a material's tangent-space normal map.
pub const NORMAL_SUFFIX: &str = "_normal";
/// The suffix of a material's perceptual roughness map.
pub const ROUGHNESS_SUFFIX: &str = "_rough";
/// The suffix of a material's metallic map.
pub const METALLIC_SUFFIX: &str = "_metal";
/// The suffix of a material's ambient occlusion map.
pub const OCCLUSION_SUFFIX: &str = "_ao";
/// Every map suffix; an image whose stem ends in one is a map, not a material's albedo.
pub const MAP_SUFFIXES: [&str; 4] = [
    NORMAL_SUFFIX,
    ROUGHNESS_SUFFIX,
    METALLIC_SUFFIX,
    OCCLUSION_SUFFIX,
];
/// The image types a source may be saved as, in the order they are looked for.
pub const IMAGE_EXTENSIONS: [&str; 3] = ["png", "jpg", "jpeg"];

/// What a material's unauthored maps hold.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialDefaults {
    /// Perceptual roughness, 0..1.
    pub roughness: f32,
    /// Metallic, 0..1.
    pub metallic: f32,
    /// Ambient occlusion, 0..1 (1 is unoccluded).
    pub occlusion: f32,
}

impl Default for MaterialDefaults {
    /// A rough dielectric: roughness 0.8, not metal, unoccluded.
    fn default() -> Self {
        Self {
            roughness: 0.8,
            metallic: 0.0,
            occlusion: 1.0,
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn unit_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// A flat normal (0, 0, 1), encoded.
const FLAT_NORMAL: [u8; 3] = [128, 128, 255];

impl MaterialDefaults {
    /// The normal + roughness texel of a material without either map: a flat normal and the
    /// default roughness.
    #[must_use]
    pub fn normal_roughness_texel(&self) -> [u8; 4] {
        let [x, y, z] = FLAT_NORMAL;
        [x, y, z, unit_byte(self.roughness)]
    }

    /// The metallic + occlusion texel of a material without either map.
    #[must_use]
    pub fn metallic_occlusion_texel(&self) -> [u8; 4] {
        [unit_byte(self.metallic), unit_byte(self.occlusion), 0, 255]
    }
}

/// The three textures of the material whose albedo is at [`albedo`](Self::albedo)'s source.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialMaps {
    /// Albedo and opacity.
    pub albedo: Recipe,
    /// Normal and roughness, or `None` when neither map exists.
    pub normal_roughness: Option<Recipe>,
    /// Metallic and occlusion, or `None` when neither map exists.
    pub metallic_occlusion: Option<Recipe>,
}

/// `stem` + `suffix` with the first image extension that exists, if any.
fn find_map(stem: &Path, suffix: &str) -> Option<PathBuf> {
    IMAGE_EXTENSIONS.iter().find_map(|extension| {
        let mut name = stem.as_os_str().to_owned();
        name.push(suffix);
        name.push(".");
        name.push(extension);
        let path = PathBuf::from(name);
        path.exists().then_some(path)
    })
}

fn suffixed(stem: &Path, suffix: &str, extension: &str) -> PathBuf {
    let mut name = stem.as_os_str().to_owned();
    name.push(suffix);
    name.push(".");
    name.push(extension);
    PathBuf::from(name)
}

impl MaterialMaps {
    /// The material whose albedo image is `albedo`, with its maps found beside it by suffix.
    #[must_use]
    pub fn beside(albedo: &Path, defaults: &MaterialDefaults) -> Self {
        let stem = albedo.with_extension("");
        let normal = find_map(&stem, NORMAL_SUFFIX);
        let roughness = find_map(&stem, ROUGHNESS_SUFFIX);
        let metallic = find_map(&stem, METALLIC_SUFFIX);
        let occlusion = find_map(&stem, OCCLUSION_SUFFIX);
        let [nx, ny, nz, rough] = defaults.normal_roughness_texel();
        let [metal, ao, ..] = defaults.metallic_occlusion_texel();
        let normal_roughness = (normal.is_some() || roughness.is_some()).then(|| Recipe {
            channels: [
                normal.clone().map_or(Channel::Constant(nx), Channel::Red),
                normal.clone().map_or(Channel::Constant(ny), Channel::Green),
                normal.map_or(Channel::Constant(nz), Channel::Blue),
                roughness.map_or(Channel::Constant(rough), Channel::Grey),
            ],
            chain: Chain::Normal,
            output: suffixed(&stem, NORMAL_SUFFIX, "ktx2"),
        });
        let metallic_occlusion = (metallic.is_some() || occlusion.is_some()).then(|| Recipe {
            channels: [
                metallic.map_or(Channel::Constant(metal), Channel::Grey),
                occlusion.map_or(Channel::Constant(ao), Channel::Grey),
                Channel::Constant(0),
                Channel::Constant(255),
            ],
            chain: Chain::Linear,
            output: suffixed(&stem, METALLIC_SUFFIX, "ktx2"),
        });
        Self {
            albedo: Recipe::image(albedo, Chain::Color, stem.with_extension("ktx2")),
            normal_roughness,
            metallic_occlusion,
        }
    }

    /// The textures that are baked: the albedo and whichever maps exist.
    pub fn recipes(&self) -> impl Iterator<Item = &Recipe> {
        [
            Some(&self.albedo),
            self.normal_roughness.as_ref(),
            self.metallic_occlusion.as_ref(),
        ]
        .into_iter()
        .flatten()
    }
}

/// Whether `path` is a material's albedo image: an image whose stem does not end in a map suffix.
#[must_use]
pub fn is_albedo(path: &Path) -> bool {
    let image = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        IMAGE_EXTENSIONS
            .iter()
            .any(|known| e.eq_ignore_ascii_case(known))
    });
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    image && !MAP_SUFFIXES.iter().any(|suffix| stem.ends_with(suffix))
}

/// Every material albedo image under `dir`, recursively, sorted.
///
/// # Errors
///
/// When a directory cannot be read, naming it.
pub fn find_materials(dir: &Path) -> Result<Vec<PathBuf>, TextureError> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), TextureError> {
        let entries = std::fs::read_dir(dir)
            .map_err(|error| TextureError::new(format!("read {}: {error}", dir.display())))?;
        for entry in entries {
            let path = entry
                .map_err(|error| TextureError::new(format!("read {}: {error}", dir.display())))?
                .path();
            if path.is_dir() {
                walk(&path, found)?;
            } else if is_albedo(&path) {
                found.push(path);
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    walk(dir, &mut found)?;
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_are_told_from_albedos_by_suffix() {
        assert!(is_albedo(Path::new("deco/carpet_field.png")));
        assert!(is_albedo(Path::new("dev/wall.JPG")));
        for map in [
            "carpet_normal.png",
            "carpet_rough.png",
            "carpet_metal.jpg",
            "carpet_ao.jpeg",
        ] {
            assert!(!is_albedo(Path::new(map)), "{map}");
        }
        assert!(!is_albedo(Path::new("carpet.ktx2")));
        assert!(!is_albedo(Path::new("carpet.material")));
    }

    #[test]
    fn defaults_pack_a_flat_rough_dielectric() {
        let defaults = MaterialDefaults::default();
        assert_eq!(defaults.normal_roughness_texel(), [128, 128, 255, 204]);
        assert_eq!(defaults.metallic_occlusion_texel(), [0, 255, 0, 255]);
    }
}
