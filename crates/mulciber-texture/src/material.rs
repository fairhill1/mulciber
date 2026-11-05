//! A physically based material as three textures, found beside its albedo by name.
//!
//! | Texture | Channels | Chain | Bake |
//! | --- | --- | --- | --- |
//! | albedo | sRGB colour, alpha = opacity | [`Chain::Color`] | `name.ktx2`, BC7 sRGB |
//! | normal | tangent-space normal (OpenGL: +Y up, green up); a shader reads X and Y and rebuilds Z | [`Chain::TangentNormal`] | `name_normal.ktx2`, BC5 |
//! | surface | R = metallic, G = ambient occlusion, B = perceptual roughness, A = 1 | [`Chain::Surface`] | `name_metal.ktx2`, BC7 |
//!
//! The normal has its block to itself, which BC5 spends on nothing else, and roughness rides in
//! the surface's otherwise empty blue channel, its mips still widened by the normals
//! ([`Recipe::normals`]). The sources sit beside the albedo image (`name.png`): `name_normal`,
//! `name_rough`, `name_metal` and `name_ao`, each `.png`, `.jpg` or `.jpeg`, all of one size. The
//! roughness, metallic and occlusion maps are grey. Any of the four may be missing and is filled
//! from [`MaterialDefaults`]; a material with no normal map has no normal texture and one with
//! none of the four no surface, and the game binds a 1×1 texture of the defaults
//! ([`MaterialDefaults::normal_texel`], [`MaterialDefaults::surface_texel`]) in its place, unless
//! a game shipped without its sources has the bake: then each map is read from its bake as it is.

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
    /// The normal texel of a material without a normal map: flat.
    #[must_use]
    pub const fn normal_texel(&self) -> [u8; 4] {
        let [x, y, z] = FLAT_NORMAL;
        [x, y, z, 255]
    }

    /// The surface texel of a material with none of its maps: metallic, occlusion, roughness.
    #[must_use]
    pub fn surface_texel(&self) -> [u8; 4] {
        [
            unit_byte(self.metallic),
            unit_byte(self.occlusion),
            unit_byte(self.roughness),
            255,
        ]
    }
}

/// The three textures of the material whose albedo is at [`albedo`](Self::albedo)'s source.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialMaps {
    /// Albedo and opacity.
    pub albedo: Recipe,
    /// The normal, or `None` when there is no normal map.
    pub normal: Option<Recipe>,
    /// Metallic, occlusion and roughness, or `None` when none of the four maps exists.
    pub surface: Option<Recipe>,
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
    /// The material whose albedo image is `albedo`, with its maps found beside it by suffix: by
    /// their sources, or by their bakes where none of the material's maps are there (a game shipped
    /// without them), when its recipe names the first source it would have, missing, so that
    /// [`Recipe::prepare`] reads the bake as it is.
    #[must_use]
    pub fn beside(albedo: &Path, defaults: &MaterialDefaults) -> Self {
        let stem = albedo.with_extension("");
        let normal = find_map(&stem, NORMAL_SUFFIX);
        let roughness = find_map(&stem, ROUGHNESS_SUFFIX);
        let metallic = find_map(&stem, METALLIC_SUFFIX);
        let occlusion = find_map(&stem, OCCLUSION_SUFFIX);
        let authored =
            normal.is_some() || roughness.is_some() || metallic.is_some() || occlusion.is_some();
        let shipped = |suffix: &str| {
            (!authored && suffixed(&stem, suffix, "ktx2").exists())
                .then(|| suffixed(&stem, suffix, IMAGE_EXTENSIONS[0]))
        };
        let opaque = defaults.normal_texel()[3];
        let [metal, ao, rough, _] = defaults.surface_texel();
        let surface = (authored || shipped(METALLIC_SUFFIX).is_some()).then(|| Recipe {
            channels: [
                metallic
                    .or_else(|| shipped(METALLIC_SUFFIX))
                    .map_or(Channel::Constant(metal), Channel::Grey),
                occlusion.map_or(Channel::Constant(ao), Channel::Grey),
                roughness.map_or(Channel::Constant(rough), Channel::Grey),
                Channel::Constant(opaque),
            ],
            chain: Chain::Surface,
            normals: normal.clone(),
            output: suffixed(&stem, METALLIC_SUFFIX, "ktx2"),
        });
        let normal = normal
            .or_else(|| shipped(NORMAL_SUFFIX))
            .map(|normal| Recipe {
                channels: [
                    Channel::Red(normal.clone()),
                    Channel::Green(normal.clone()),
                    Channel::Blue(normal),
                    Channel::Constant(opaque),
                ],
                chain: Chain::TangentNormal,
                normals: None,
                output: suffixed(&stem, NORMAL_SUFFIX, "ktx2"),
            });
        Self {
            albedo: Recipe::image(albedo, Chain::Color, stem.with_extension("ktx2")),
            normal,
            surface,
        }
    }

    /// The textures that are baked: the albedo and whichever maps exist.
    pub fn recipes(&self) -> impl Iterator<Item = &Recipe> {
        [
            Some(&self.albedo),
            self.normal.as_ref(),
            self.surface.as_ref(),
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
        assert_eq!(defaults.normal_texel(), [128, 128, 255, 255]);
        assert_eq!(defaults.surface_texel(), [0, 255, 204, 255]);
    }
}
