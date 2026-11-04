//! Several textures of one chain baked as the layers of one 2D texture array, so a shader samples
//! them all through one material slot ([`mulciber::MaterialBinding::TextureArray`]): every ground
//! material's albedo in one array, their normal and roughness in another.

use std::path::{Path, PathBuf};

use mulciber::{
    Device, GraphicsError, Ktx2Texture, MipChain, TexelData, Texture, TextureDescriptor,
    TextureShape,
};

use crate::TextureError;
use crate::chain::{Chain, Levels};
use crate::container::{MEAN_KEY, MIN_ALPHA_KEY, SOURCE_DIGEST_KEY};
use crate::recipe::{Fallback, Origin, Recipe, Stats};

/// The layers of a 2D texture array bake, each packed as its own [`Recipe`] says, and where the
/// bake goes. Every layer shares one [`Chain`] and one extent; each layer recipe's own `output`
/// is not used.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrayRecipe {
    layers: Vec<Recipe>,
    output: PathBuf,
}

impl ArrayRecipe {
    /// `layers` in array order, baked to `output`.
    ///
    /// # Errors
    ///
    /// For no layers, or layers of different chains (which differ in colour space or encoding).
    pub fn new(layers: Vec<Recipe>, output: impl Into<PathBuf>) -> Result<Self, TextureError> {
        let output = output.into();
        let Some(first) = layers.first() else {
            return Err(TextureError::new("a texture array needs at least one layer").at(&output));
        };
        if let Some((index, other)) = layers
            .iter()
            .enumerate()
            .find(|(_, layer)| layer.chain != first.chain)
        {
            return Err(TextureError::new(format!(
                "layer {index} is filtered as {:?} but layer 0 as {:?}; an array's layers share \
                 one chain",
                other.chain, first.chain
            ))
            .at(&output));
        }
        Ok(Self { layers, output })
    }

    /// The layers, in array order.
    #[must_use]
    pub fn layers(&self) -> &[Recipe] {
        &self.layers
    }

    /// The bake's path.
    #[must_use]
    pub fn output(&self) -> &Path {
        &self.output
    }

    /// The chain every layer is filtered with.
    #[must_use]
    pub fn chain(&self) -> Chain {
        self.layers[0].chain
    }

    /// Every image any layer reads, each once, in layer order.
    #[must_use]
    pub fn sources(&self) -> Vec<&Path> {
        let mut sources: Vec<&Path> = Vec::new();
        for path in self.layers.iter().flat_map(Recipe::sources) {
            if !sources.contains(&path) {
                sources.push(path);
            }
        }
        sources
    }

    /// Whether every source is on disk.
    #[must_use]
    pub fn has_sources(&self) -> bool {
        self.layers.iter().all(Recipe::has_sources)
    }

    /// Every layer's [`Recipe::digest`] in order, so reordering, adding or removing a layer, or
    /// changing one, stales the bake.
    ///
    /// # Errors
    ///
    /// When a source cannot be read, naming it.
    pub fn digest(&self) -> Result<u64, TextureError> {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut mix = |bytes: &[u8]| {
            for &byte in bytes {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
        };
        mix(b"array");
        mix(&(self.layers.len() as u64).to_le_bytes());
        for layer in &self.layers {
            mix(&layer.digest()?.to_le_bytes());
        }
        Ok(hash)
    }

    /// Every layer's RGBA8 chain, built from its sources.
    ///
    /// # Errors
    ///
    /// As [`Recipe::build`], or when two layers differ in size, naming the layer.
    pub fn build(&self) -> Result<Vec<Levels>, TextureError> {
        let mut built: Vec<Levels> = Vec::with_capacity(self.layers.len());
        for (index, layer) in self.layers.iter().enumerate() {
            let levels = layer
                .build()
                .map_err(|error| self.layer_error(index, &error))?;
            if let Some(first) = built.first()
                && (first.width, first.height) != (levels.width, levels.height)
            {
                return Err(TextureError::new(format!(
                    "layer {index} is {}x{} but layer 0 is {}x{}; an array's layers share one \
                     size",
                    levels.width, levels.height, first.width, first.height
                ))
                .at(&self.output));
            }
            built.push(levels);
        }
        Ok(built)
    }

    fn layer_error(&self, index: usize, error: &TextureError) -> TextureError {
        TextureError::new(format!("layer {index}: {}", error.message())).at(&self.output)
    }

    /// Whether the bake on disk was built from the sources as they are now, with these layers.
    ///
    /// # Errors
    ///
    /// When a source cannot be read.
    pub fn is_current(&self) -> Result<bool, TextureError> {
        let Ok(bytes) = std::fs::read(&self.output) else {
            return Ok(false);
        };
        Ok(self.read_bake(&bytes, true).ok().flatten().is_some())
    }

    /// The array ready to upload: the bake when it is current, or when the sources are not there
    /// to check it against (a release); every layer's chain built from the sources otherwise,
    /// with the reason in [`PreparedArray::origin`]. An array cannot have its mips generated, so
    /// that chain is always built on the CPU.
    ///
    /// # Errors
    ///
    /// When there is neither a usable bake nor readable sources.
    pub fn prepare(&self) -> Result<PreparedArray, TextureError> {
        let has_sources = self.has_sources();
        let fallback = match std::fs::read(&self.output) {
            Err(_) => Fallback::NoBake,
            Ok(bytes) => match self.read_bake(&bytes, has_sources) {
                Ok(Some(prepared)) => return Ok(prepared),
                Ok(None) => Fallback::Stale,
                Err(error) if has_sources => Fallback::Unreadable(error.to_string()),
                Err(error) => return Err(error),
            },
        };
        if !has_sources {
            let missing = self.sources().into_iter().find(|path| !path.exists());
            return Err(TextureError::new(format!(
                "{}: no bake, and {} is missing",
                self.output.display(),
                missing.map_or_else(|| "a source".to_owned(), |p| p.display().to_string())
            )));
        }
        let srgb = self.chain().is_srgb();
        let built = self.build()?;
        let (width, height) = (built[0].width, built[0].height);
        Ok(PreparedArray {
            width,
            height,
            stats: built
                .iter()
                .map(|l| Stats::of(&l.levels[0], srgb))
                .collect(),
            pixels: ArrayPixels::Rgba8 {
                srgb,
                layers: built.into_iter().map(|levels| levels.levels).collect(),
            },
            origin: Origin::Sources(fallback),
        })
    }

    /// The bake, or `None` when it is stale against sources that are present.
    fn read_bake(&self, bytes: &[u8], check: bool) -> Result<Option<PreparedArray>, TextureError> {
        let at = |error: GraphicsError| TextureError::new(error.message()).at(&self.output);
        let bake = Ktx2Texture::parse(bytes).map_err(at)?;
        let count = self.layers.len();
        let layers = bake.layer_count().map_or(0, |layers| layers as usize);
        let compression = self.chain().compression();
        if bake.compression() != compression || layers != count {
            if check {
                return Ok(None);
            }
            return Err(TextureError::new(format!(
                "baked as {layers} layers of {:?}, but its recipe makes {count} of {compression:?}",
                bake.compression(),
            ))
            .at(&self.output));
        }
        let stats = read_array_stats(&bake, count).map_err(|error| error.at(&self.output))?;
        if check {
            let digest = core::str::from_utf8(bake.value(SOURCE_DIGEST_KEY).unwrap_or_default())
                .ok()
                .and_then(|text| u64::from_str_radix(text, 16).ok());
            if digest != Some(self.digest()?) {
                return Ok(None);
            }
        }
        Ok(Some(PreparedArray {
            width: bake.width(),
            height: bake.height(),
            pixels: ArrayPixels::Ktx2(bytes.to_vec()),
            stats,
            origin: Origin::Bake,
        }))
    }
}

/// The key/value entries an array bake carries its layers' stats in: [`MEAN_KEY`] four numbers
/// per layer and [`MIN_ALPHA_KEY`] one per layer, layer after layer, space-separated.
#[must_use]
pub fn array_stat_entries(stats: &[Stats]) -> [(&'static str, String); 2] {
    let means: Vec<String> = stats
        .iter()
        .flat_map(|stats| stats.mean.map(|value| value.to_string()))
        .collect();
    let alphas: Vec<String> = stats.iter().map(|s| s.min_alpha.to_string()).collect();
    [
        (MEAN_KEY, means.join(" ")),
        (MIN_ALPHA_KEY, alphas.join(" ")),
    ]
}

fn read_array_stats(bake: &Ktx2Texture<'_>, layers: usize) -> Result<Vec<Stats>, TextureError> {
    let text = |key: &str| -> Result<&str, TextureError> {
        bake.value(key)
            .and_then(|value| core::str::from_utf8(value).ok())
            .ok_or_else(|| TextureError::new(format!("carries no {key}; rebake it")))
    };
    let means: Vec<f32> = text(MEAN_KEY)?
        .split(' ')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|error| TextureError::new(format!("has an unreadable {MEAN_KEY}: {error}")))?;
    let alphas: Vec<u8> = text(MIN_ALPHA_KEY)?
        .split(' ')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|error| {
            TextureError::new(format!("has an unreadable {MIN_ALPHA_KEY}: {error}"))
        })?;
    if means.len() != layers * 4 || alphas.len() != layers {
        return Err(TextureError::new(format!(
            "its stats are not {layers} layers' worth; rebake it"
        )));
    }
    Ok(means
        .as_chunks::<4>()
        .0
        .iter()
        .zip(alphas)
        .map(|(&mean, min_alpha)| Stats { mean, min_alpha })
        .collect())
}

/// A texture array's levels, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArrayPixels {
    /// A bake: a KTX 2.0 array of block-compressed levels, uploaded as stored.
    Ktx2(Vec<u8>),
    /// Every layer's RGBA8 chain built from the sources on the CPU.
    Rgba8 {
        /// Whether they are sRGB colour.
        srgb: bool,
        /// Each layer's levels, base level first.
        layers: Vec<Vec<Vec<u8>>>,
    },
}

/// A texture array ready to upload.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedArray {
    /// The base level's width in texels.
    pub width: u32,
    /// The base level's height in texels.
    pub height: u32,
    /// The levels.
    pub pixels: ArrayPixels,
    /// Each layer's base level mean and smallest alpha.
    pub stats: Vec<Stats>,
    /// Bake or sources, and why.
    pub origin: Origin,
}

impl PreparedArray {
    /// Uploads the array through [`Device::create_texture`] as a
    /// [`TextureShape::D2Array`]: a bake as its KTX 2.0 [`descriptor`](Ktx2Texture::descriptor),
    /// RGBA8 as sRGB or UNORM with every layer's chain supplied.
    ///
    /// # Errors
    ///
    /// The device's upload errors.
    pub fn upload(&self, device: &Device<'_>) -> Result<Texture, GraphicsError> {
        match &self.pixels {
            ArrayPixels::Ktx2(bytes) => {
                device.create_texture(&Ktx2Texture::parse(bytes)?.descriptor())
            }
            ArrayPixels::Rgba8 { srgb, layers } => {
                let images: Vec<&[u8]> = layers.iter().flatten().map(Vec::as_slice).collect();
                // More than any device holds, which it refuses by name, if past u32.
                let count = u32::try_from(layers.len()).unwrap_or(u32::MAX);
                device.create_texture(&TextureDescriptor {
                    shape: TextureShape::D2Array {
                        width: self.width,
                        height: self.height,
                        layers: count,
                    },
                    mips: MipChain::Supplied,
                    data: if *srgb {
                        TexelData::Rgba8Srgb(&images)
                    } else {
                        TexelData::Rgba8Unorm(&images)
                    },
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::Chain;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mulciber-texture-array-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn image(path: &Path, size: u32, rgba: [u8; 4]) {
        image::RgbaImage::from_pixel(size, size, image::Rgba(rgba))
            .save(path)
            .unwrap();
    }

    #[test]
    fn layers_share_a_chain_and_a_size() {
        let dir = scratch("share");
        let (grass, rock, small) = (
            dir.join("grass.png"),
            dir.join("rock.png"),
            dir.join("s.png"),
        );
        image(&grass, 8, [40, 160, 40, 255]);
        image(&rock, 8, [120, 120, 120, 255]);
        image(&small, 4, [0, 0, 0, 255]);
        let layer = |path: &Path, chain| Recipe::image(path, chain, dir.join("unused.ktx2"));
        let error = ArrayRecipe::new(
            vec![layer(&grass, Chain::Color), layer(&rock, Chain::Linear)],
            dir.join("ground.ktx2"),
        )
        .unwrap_err();
        assert!(error.message().contains("layer 1 is filtered as Linear"));
        assert!(ArrayRecipe::new(Vec::new(), dir.join("none.ktx2")).is_err());
        let mixed = ArrayRecipe::new(
            vec![layer(&grass, Chain::Color), layer(&small, Chain::Color)],
            dir.join("mixed.ktx2"),
        )
        .unwrap();
        let error = mixed.build().unwrap_err();
        assert!(
            error
                .message()
                .contains("layer 1 is 4x4 but layer 0 is 8x8")
        );
    }

    #[test]
    fn an_unbaked_array_builds_every_layers_chain_from_its_sources() {
        let dir = scratch("sources");
        let (grass, rock) = (dir.join("grass.png"), dir.join("rock.png"));
        image(&grass, 8, [40, 160, 40, 255]);
        image(&rock, 8, [120, 120, 120, 200]);
        let layer = |path: &Path| Recipe::image(path, Chain::Color, dir.join("unused.ktx2"));
        let array =
            ArrayRecipe::new(vec![layer(&grass), layer(&rock)], dir.join("ground.ktx2")).unwrap();
        assert!(!array.is_current().unwrap());
        let prepared = array.prepare().unwrap();
        assert_eq!(prepared.origin, Origin::Sources(Fallback::NoBake));
        let ArrayPixels::Rgba8 { srgb, layers } = &prepared.pixels else {
            panic!("built from sources");
        };
        assert!(srgb);
        assert_eq!(layers.len(), 2);
        // 8x8 down to 1x1.
        assert!(layers.iter().all(|levels| levels.len() == 4));
        assert_eq!(layers[1][3], [120, 120, 120, 200]);
        assert_eq!(prepared.stats[1].min_alpha, 200);
        // Reordering the layers is another array.
        let swapped =
            ArrayRecipe::new(vec![layer(&rock), layer(&grass)], dir.join("ground.ktx2")).unwrap();
        assert_ne!(swapped.digest().unwrap(), array.digest().unwrap());
    }

    #[test]
    fn stats_round_trip_through_their_entries() {
        let stats = [
            Stats {
                mean: [0.25, 0.5, 0.75, 1.0],
                min_alpha: 255,
            },
            Stats {
                mean: [0.0, 0.125, 0.5, 0.5],
                min_alpha: 3,
            },
        ];
        let entries = array_stat_entries(&stats);
        let pairs: Vec<(&str, &str)> = entries.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let layers = vec![vec![vec![0_u8; 16]]; 2];
        let bytes = crate::container::ktx2_array_bytes(
            mulciber::BlockCompression::Bc7Srgb,
            4,
            4,
            &layers,
            &pairs,
        )
        .unwrap();
        let bake = Ktx2Texture::parse(&bytes).unwrap();
        assert_eq!(read_array_stats(&bake, 2).unwrap(), stats);
        assert!(read_array_stats(&bake, 3).is_err());
    }
}
