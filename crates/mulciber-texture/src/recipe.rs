//! A texture's sources, how they pack into RGBA, its chain, and where its bake goes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mulciber::{Device, GraphicsError, Ktx2Texture, Texture};

use crate::TextureError;
use crate::chain::{Chain, Levels, srgb_to_linear};
use crate::container::{MEAN_KEY, MIN_ALPHA_KEY, SOURCE_DIGEST_KEY};

/// Bumped when a chain filter or the packing changes, so every bake reads as stale.
const BAKE_VERSION: u8 = 1;

/// Where one channel of a texture comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum Channel {
    /// The same byte everywhere: a default for a map that was not authored.
    Constant(u8),
    /// An image's red channel.
    Red(PathBuf),
    /// An image's green channel.
    Green(PathBuf),
    /// An image's blue channel.
    Blue(PathBuf),
    /// An image's alpha channel (255 where the image has none).
    Alpha(PathBuf),
    /// An image's luminance, for a grey map such as roughness, whatever colour type it is saved
    /// as (Rec. 709 weights over its stored values for a colour image).
    Grey(PathBuf),
}

impl Channel {
    fn source(&self) -> Option<&Path> {
        match self {
            Self::Constant(_) => None,
            Self::Red(path)
            | Self::Green(path)
            | Self::Blue(path)
            | Self::Alpha(path)
            | Self::Grey(path) => Some(path),
        }
    }

    /// The channel's byte of a source texel.
    fn pick(&self, texel: [u8; 4]) -> u8 {
        match self {
            Self::Constant(value) => *value,
            Self::Red(_) => texel[0],
            Self::Green(_) => texel[1],
            Self::Blue(_) => texel[2],
            Self::Alpha(_) => texel[3],
            Self::Grey(_) => {
                let y = 0.2126 * f32::from(texel[0])
                    + 0.7152 * f32::from(texel[1])
                    + 0.0722 * f32::from(texel[2]);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let byte = y.round().clamp(0.0, 255.0) as u8;
                byte
            }
        }
    }

    const fn tag(&self) -> u8 {
        match self {
            Self::Constant(_) => 0,
            Self::Red(_) => 1,
            Self::Green(_) => 2,
            Self::Blue(_) => 3,
            Self::Alpha(_) => 4,
            Self::Grey(_) => 5,
        }
    }
}

/// One texture: four channels packed from images or constants, the [`Chain`] its mips are built
/// with, and the `.ktx2` its BC7 bake is written to. The recipe is the one place the packing and
/// filter are written down, so a bake and the fallback built from the sources when there is no
/// bake cannot differ.
#[derive(Clone, Debug, PartialEq)]
pub struct Recipe {
    /// Red, green, blue and alpha.
    pub channels: [Channel; 4],
    /// How the mips are filtered, which also decides sRGB or linear.
    pub chain: Chain,
    /// The bake's path.
    pub output: PathBuf,
}

/// A decoded source image.
struct Image {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn decode(path: &Path) -> Result<Image, TextureError> {
    let image = image::open(path)
        .map_err(|error| TextureError::new(format!("decode {}: {error}", path.display())))?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Ok(Image {
        width,
        height,
        rgba: image.into_raw(),
    })
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv1a(bytes: &[u8], seed: u64) -> u64 {
    bytes.iter().fold(seed, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

impl Recipe {
    /// An image's own RGBA, as authored, filtered by `chain`.
    #[must_use]
    pub fn image(source: impl Into<PathBuf>, chain: Chain, output: impl Into<PathBuf>) -> Self {
        let source = source.into();
        Self {
            channels: [
                Channel::Red(source.clone()),
                Channel::Green(source.clone()),
                Channel::Blue(source.clone()),
                Channel::Alpha(source),
            ],
            chain,
            output: output.into(),
        }
    }

    /// Every image the recipe reads, each once, in channel order.
    #[must_use]
    pub fn sources(&self) -> Vec<&Path> {
        let mut sources: Vec<&Path> = Vec::new();
        for path in self.channels.iter().filter_map(Channel::source) {
            if !sources.contains(&path) {
                sources.push(path);
            }
        }
        sources
    }

    /// Whether every source is on disk, which a release that ships only bakes is not.
    #[must_use]
    pub fn has_sources(&self) -> bool {
        self.sources().iter().all(|path| path.exists())
    }

    /// FNV-1a over the bake version, the chain, the packing and the sources' bytes. A bake records
    /// the digest it was built from, which is how a stale one is told apart. Paths are not part of
    /// it, so moving a material does not stale its bake.
    ///
    /// # Errors
    ///
    /// When a source cannot be read, naming it.
    pub fn digest(&self) -> Result<u64, TextureError> {
        let sources = self.sources();
        let mut hash = fnv1a(&[BAKE_VERSION, self.chain.tag()], FNV_OFFSET);
        for channel in &self.channels {
            let detail = match channel {
                Channel::Constant(value) => *value,
                other => sources
                    .iter()
                    .position(|s| Some(*s) == other.source())
                    .and_then(|index| u8::try_from(index).ok())
                    .unwrap_or(u8::MAX),
            };
            hash = fnv1a(&[channel.tag(), detail], hash);
        }
        for source in sources {
            let bytes = std::fs::read(source).map_err(|error| {
                TextureError::new(format!("read {}: {error}", source.display()))
            })?;
            hash = fnv1a(&(bytes.len() as u64).to_le_bytes(), hash);
            hash = fnv1a(&bytes, hash);
        }
        Ok(hash)
    }

    /// Decodes the sources, packs them and builds the RGBA8 chain.
    ///
    /// # Errors
    ///
    /// When a source cannot be read or decoded, or two sources differ in size, naming them.
    pub fn build(&self) -> Result<Levels, TextureError> {
        let mut images: BTreeMap<&Path, Image> = BTreeMap::new();
        for source in self.sources() {
            images.insert(source, decode(source)?);
        }
        let mut extent: Option<(u32, u32, &Path)> = None;
        for (path, image) in &images {
            match extent {
                Some((w, h, first)) if (w, h) != (image.width, image.height) => {
                    return Err(TextureError::new(format!(
                        "{} is {}x{} but {} is {w}x{h}; a texture's sources share one size",
                        path.display(),
                        image.width,
                        image.height,
                        first.display()
                    )));
                }
                Some(_) => {}
                None => extent = Some((image.width, image.height, path)),
            }
        }
        let (width, height) = extent.map_or((1, 1), |(w, h, _)| (w, h));
        let texels = width as usize * height as usize;
        let mut base = vec![0_u8; texels * 4];
        for (index, channel) in self.channels.iter().enumerate() {
            let source = channel
                .source()
                .map(|path| images[path].rgba.as_chunks::<4>().0);
            for (texel, out) in base.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                out[index] = channel.pick(source.map_or([0; 4], |texels| texels[texel]));
            }
        }
        Ok(self.chain.build(width, height, base))
    }

    /// Whether the bake on disk was built from the sources as they are now, with this recipe.
    ///
    /// # Errors
    ///
    /// When a source cannot be read.
    pub fn is_current(&self) -> Result<bool, TextureError> {
        let Ok(bytes) = std::fs::read(&self.output) else {
            return Ok(false);
        };
        let Ok(bake) = Ktx2Texture::parse(&bytes) else {
            return Ok(false);
        };
        Ok(bake.compression() == self.chain.compression()
            && read_stats(&bake).is_ok()
            && digest_of(&bake) == Some(self.digest()?))
    }

    /// The texture ready to upload: the bake when it is current, or when the sources are not
    /// there to check it against (a release); the chain built from the sources otherwise, with
    /// the reason in [`Prepared::origin`].
    ///
    /// # Errors
    ///
    /// When there is neither a usable bake nor readable sources.
    pub fn prepare(&self) -> Result<Prepared, TextureError> {
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
        let levels = self.build()?;
        let stats = Stats::of(&levels.levels[0], self.chain.is_srgb());
        Ok(Prepared {
            width: levels.width,
            height: levels.height,
            pixels: Pixels::Rgba8 {
                srgb: self.chain.is_srgb(),
                levels: levels.levels,
            },
            stats,
            origin: Origin::Sources(fallback),
        })
    }

    /// The bake, or `None` when it is stale against sources that are present.
    fn read_bake(&self, bytes: &[u8], check: bool) -> Result<Option<Prepared>, TextureError> {
        let at = |error: GraphicsError| TextureError::new(error.message()).at(&self.output);
        let bake = Ktx2Texture::parse(bytes).map_err(at)?;
        if bake.compression() != self.chain.compression() {
            if check {
                return Ok(None);
            }
            return Err(TextureError::new(format!(
                "baked as {:?}, but its recipe makes {:?}",
                bake.compression(),
                self.chain.compression()
            ))
            .at(&self.output));
        }
        let stats = read_stats(&bake).map_err(|error| error.at(&self.output))?;
        if check && digest_of(&bake) != Some(self.digest()?) {
            return Ok(None);
        }
        Ok(Some(Prepared {
            width: bake.width(),
            height: bake.height(),
            pixels: Pixels::Ktx2(bytes.to_vec()),
            stats,
            origin: Origin::Bake,
        }))
    }
}

fn digest_of(bake: &Ktx2Texture<'_>) -> Option<u64> {
    let text = core::str::from_utf8(bake.value(SOURCE_DIGEST_KEY)?).ok()?;
    u64::from_str_radix(text, 16).ok()
}

fn read_stats(bake: &Ktx2Texture<'_>) -> Result<Stats, TextureError> {
    let text = |key: &str| -> Result<&str, TextureError> {
        bake.value(key)
            .and_then(|value| core::str::from_utf8(value).ok())
            .ok_or_else(|| TextureError::new(format!("carries no {key}; rebake it")))
    };
    let parsed: Vec<f32> = text(MEAN_KEY)?
        .split(' ')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|error| TextureError::new(format!("has an unreadable {MEAN_KEY}: {error}")))?;
    let mean: [f32; 4] = parsed
        .try_into()
        .map_err(|_| TextureError::new(format!("{MEAN_KEY} is not four numbers")))?;
    let min_alpha = text(MIN_ALPHA_KEY)?.parse().map_err(|error| {
        TextureError::new(format!("has an unreadable {MIN_ALPHA_KEY}: {error}"))
    })?;
    Ok(Stats { mean, min_alpha })
}

/// What a texture's base level averages to, for whatever needs it without its texels: the
/// colour a lightmap bounces off a surface, whether it is see-through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    /// Mean RGBA in 0..1: RGB in linear light for an sRGB texture, as stored otherwise.
    pub mean: [f32; 4],
    /// The smallest alpha byte.
    pub min_alpha: u8,
}

impl Stats {
    /// The stats of an RGBA8 level.
    #[must_use]
    pub fn of(level: &[u8], srgb: bool) -> Self {
        let texels = level.as_chunks::<4>().0;
        let mut sum = [0.0_f64; 4];
        let mut min_alpha = u8::MAX;
        for texel in texels {
            for c in 0..3 {
                sum[c] += f64::from(if srgb {
                    srgb_to_linear(texel[c])
                } else {
                    f32::from(texel[c]) / 255.0
                });
            }
            sum[3] += f64::from(texel[3]) / 255.0;
            min_alpha = min_alpha.min(texel[3]);
        }
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let mean = sum.map(|s| (s / texels.len().max(1) as f64) as f32);
        Self { mean, min_alpha }
    }

    /// The key/value entries a bake carries them in.
    #[must_use]
    pub fn entries(&self) -> [(&'static str, String); 2] {
        let [r, g, b, a] = self.mean;
        [
            (MEAN_KEY, format!("{r} {g} {b} {a}")),
            (MIN_ALPHA_KEY, self.min_alpha.to_string()),
        ]
    }
}

/// Why a texture was built from its sources rather than read from its bake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// There is no bake.
    NoBake,
    /// The sources or the recipe changed since it was baked.
    Stale,
    /// The bake could not be read.
    Unreadable(String),
}

/// Where a [`Prepared`] texture came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Its bake, uploaded as BC7 blocks.
    Bake,
    /// Its sources, uploaded as RGBA8.
    Sources(Fallback),
}

/// A texture's levels, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pixels {
    /// A bake: a KTX 2.0 file of BC7 levels, uploaded as stored.
    Ktx2(Vec<u8>),
    /// RGBA8 levels built from the sources.
    Rgba8 {
        /// Whether they are sRGB colour.
        srgb: bool,
        /// Base level first.
        levels: Vec<Vec<u8>>,
    },
}

/// A texture ready to upload.
#[derive(Clone, Debug, PartialEq)]
pub struct Prepared {
    /// The base level's width in texels.
    pub width: u32,
    /// The base level's height in texels.
    pub height: u32,
    /// The levels.
    pub pixels: Pixels,
    /// The base level's mean and smallest alpha.
    pub stats: Stats,
    /// Bake or sources, and why.
    pub origin: Origin,
}

impl Prepared {
    /// Uploads the levels: a bake through [`Device::create_ktx2_texture`], RGBA8 as sRGB or UNORM
    /// by the chain.
    ///
    /// # Errors
    ///
    /// The device's upload errors.
    pub fn upload(&self, device: &Device<'_>) -> Result<Texture, GraphicsError> {
        match &self.pixels {
            Pixels::Ktx2(bytes) => device.create_ktx2_texture(&Ktx2Texture::parse(bytes)?),
            Pixels::Rgba8 { srgb, levels } => {
                let slices: Vec<&[u8]> = levels.iter().map(Vec::as_slice).collect();
                if *srgb {
                    device.create_rgba8_srgb_texture_with_mips(self.width, self.height, &slices)
                } else {
                    device.create_rgba8_unorm_texture_with_mips(self.width, self.height, &slices)
                }
            }
        }
    }
}
