//! The glyph atlas: the game's faces rasterised at the sizes it asks for, with a solid block for
//! flat shapes and any coverage masks or colour images it adds, packed into one RGBA8 sRGB image.

use std::collections::HashMap;
use std::fmt;

use mulciber::{
    Device, GraphicsError, MipChain, TexelData, Texture, TextureDescriptor, TextureShape,
};

/// Atlas texels between neighbours, so no sample reads into the next one.
const PADDING: u32 = 1;
/// The solid block's side in texels; flat shapes sample its middle.
const SOLID: u32 = 4;

/// Why an atlas could not be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextError {
    message: String,
}

impl TextError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// What went wrong.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for TextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TextError {}

/// A font the game supplies, parsed from its TrueType or OpenType bytes.
pub struct Font {
    inner: fontdue::Font,
}

impl Font {
    /// Parses a TrueType or OpenType font.
    ///
    /// # Errors
    ///
    /// When `bytes` are not a font fontdue can read.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TextError> {
        fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
            .map(|inner| Self { inner })
            .map_err(|error| TextError::new(format!("font: {error}")))
    }

    /// Whether the font has a glyph for `character`.
    #[must_use]
    pub fn has(&self, character: char) -> bool {
        self.inner.lookup_glyph_index(character) != 0
    }
}

impl fmt::Debug for Font {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Font")
            .field("glyphs", &self.inner.glyph_count())
            .finish_non_exhaustive()
    }
}

/// One face baked into an atlas: a font at one pixel size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceId(u32);

/// A rectangle of atlas texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Region {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// How a face sits on a line, in pixels at its baked size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceMetrics {
    /// The size the face was baked at.
    pub px: f32,
    /// How far the face reaches above the baseline.
    pub ascent: f32,
    /// How far it reaches below the baseline, as a positive distance.
    pub descent: f32,
    /// Baseline to baseline, the font's own line spacing.
    pub line_height: f32,
    /// The height of a capital `H`, for centring a label in a box; 0 without one.
    pub cap_height: f32,
}

/// A glyph's place in the atlas and on the line.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Glyph {
    pub(crate) region: Region,
    /// Left bearing: from the pen to the bitmap's left edge.
    pub(crate) xmin: f32,
    /// The bitmap's bottom above the baseline (below it when negative).
    pub(crate) ymin: f32,
    pub(crate) advance: f32,
}

/// Packs faces, coverage masks and images into an atlas, left to right in shelves a fixed width
/// across, adding rows as they fill.
///
/// Glyphs and masks are white with their coverage in alpha; images keep their colour. The atlas is
/// sRGB, which leaves alpha linear, so coverage is unchanged by it and an image's colour is decoded
/// to linear light when sampled.
#[derive(Debug)]
pub struct AtlasBuilder {
    width: u32,
    /// RGBA8 texels, `width` per row.
    texels: Vec<u8>,
    pen_x: u32,
    pen_y: u32,
    row_height: u32,
    solid: Region,
    faces: Vec<FaceMetrics>,
    glyphs: HashMap<(FaceId, char), Glyph>,
}

impl AtlasBuilder {
    /// An empty atlas `width` texels across, holding only the solid block flat shapes draw from.
    /// Rows are added as things are packed, so only the width is chosen up front: wide enough for
    /// the widest glyph or image, and as wide as the device samples (1024 or 2048 suit most
    /// HUDs).
    ///
    /// # Errors
    ///
    /// When `width` is too narrow to hold the solid block.
    pub fn new(width: u32) -> Result<Self, TextError> {
        if width < SOLID + 2 * PADDING {
            return Err(TextError::new(format!(
                "an atlas {width} texels wide cannot hold its solid block"
            )));
        }
        let mut builder = Self {
            width,
            texels: Vec::new(),
            pen_x: PADDING,
            pen_y: PADDING,
            row_height: 0,
            solid: Region {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            faces: Vec::new(),
            glyphs: HashMap::new(),
        };
        let coverage = [u8::MAX; (SOLID * SOLID) as usize];
        builder.solid = builder.coverage(SOLID, SOLID, &coverage)?;
        Ok(builder)
    }

    /// Rasterises `font` at `px` pixels for each of `characters` the font has, and returns the
    /// face to measure and set text in. A character the font lacks is left out, so it measures
    /// and draws as a space (or nothing, without one) rather than as the font's missing-glyph box.
    ///
    /// # Errors
    ///
    /// When `px` is not positive and finite, or a glyph is wider than the atlas.
    ///
    /// # Panics
    ///
    /// Past `u32::MAX` faces.
    pub fn face(
        &mut self,
        font: &Font,
        px: f32,
        characters: impl IntoIterator<Item = char>,
    ) -> Result<FaceId, TextError> {
        if !(px.is_finite() && px > 0.0) {
            return Err(TextError::new(format!(
                "a face's size must be positive, not {px}"
            )));
        }
        let id = FaceId(u32::try_from(self.faces.len()).expect("fewer faces than u32 counts"));
        for character in characters {
            if self.glyphs.contains_key(&(id, character)) || !font.has(character) {
                continue;
            }
            let (metrics, coverage) = font.inner.rasterize(character, px);
            let width = u32::try_from(metrics.width).expect("a glyph fits u32 texels");
            let height = u32::try_from(metrics.height).expect("a glyph fits u32 texels");
            let region = self.coverage(width, height, &coverage).map_err(|error| {
                TextError::new(format!("{character:?} at {px} px: {}", error.message))
            })?;
            #[allow(clippy::cast_precision_loss)]
            self.glyphs.insert(
                (id, character),
                Glyph {
                    region,
                    xmin: metrics.xmin as f32,
                    ymin: metrics.ymin as f32,
                    advance: metrics.advance_width,
                },
            );
        }
        let line = font.inner.horizontal_line_metrics(px);
        #[allow(clippy::cast_precision_loss)]
        let cap_height = if font.has('H') {
            font.inner.metrics('H', px).height as f32
        } else {
            0.0
        };
        self.faces.push(FaceMetrics {
            px,
            ascent: line.map_or(px, |line| line.ascent),
            descent: line.map_or(0.0, |line| -line.descent),
            line_height: line.map_or(px, |line| line.new_line_size),
            cap_height,
        });
        Ok(id)
    }

    /// Packs a coverage mask, a byte per texel row by row, white with the mask in alpha: a shape
    /// of the game's own, a keycap's part, an icon drawn in one colour.
    ///
    /// # Errors
    ///
    /// When `coverage` is not `width` × `height` bytes, or the mask is wider than the atlas.
    pub fn coverage(
        &mut self,
        width: u32,
        height: u32,
        coverage: &[u8],
    ) -> Result<Region, TextError> {
        if coverage.len() != width as usize * height as usize {
            return Err(TextError::new(format!(
                "a {width}x{height} mask needs {} bytes, not {}",
                width as usize * height as usize,
                coverage.len()
            )));
        }
        let region = self.place(width, height)?;
        for (row, line) in coverage.chunks_exact(width.max(1) as usize).enumerate() {
            for (column, &alpha) in line.iter().enumerate() {
                let at = self.texel(region.x as usize + column, region.y as usize + row);
                self.texels[at + 3] = alpha;
            }
        }
        Ok(region)
    }

    /// Packs a colour image: RGBA8, sRGB with straight alpha, row by row. It draws in its own
    /// colour, tinted by the vertex colour.
    ///
    /// # Errors
    ///
    /// When `rgba` is not `width` × `height` × 4 bytes, or the image is wider than the atlas.
    pub fn image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<Region, TextError> {
        if rgba.len() != width as usize * height as usize * 4 {
            return Err(TextError::new(format!(
                "a {width}x{height} image needs {} bytes, not {}",
                width as usize * height as usize * 4,
                rgba.len()
            )));
        }
        let region = self.place(width, height)?;
        for (row, line) in rgba.chunks_exact((width.max(1) * 4) as usize).enumerate() {
            let at = self.texel(region.x as usize, region.y as usize + row);
            self.texels[at..at + line.len()].copy_from_slice(line);
        }
        Ok(region)
    }

    /// The finished atlas.
    #[must_use]
    pub fn build(self) -> Atlas {
        let rows = self.pen_y + self.row_height + PADDING;
        let mut texels = self.texels;
        texels.resize(rows as usize * self.width as usize * 4, 0);
        // Rows added only for the bottom padding are white like the rest.
        for pixel in texels.as_chunks_mut::<4>().0 {
            if pixel[3] == 0 {
                *pixel = [u8::MAX, u8::MAX, u8::MAX, 0];
            }
        }
        Atlas {
            width: self.width,
            height: rows,
            texels,
            solid: self.solid,
            faces: self.faces,
            glyphs: self.glyphs,
        }
    }

    /// Reserves a `width` × `height` rectangle, starting a new shelf when this one is full and
    /// adding white, transparent rows as needed.
    fn place(&mut self, width: u32, height: u32) -> Result<Region, TextError> {
        if width + 2 * PADDING > self.width {
            return Err(TextError::new(format!(
                "{width} texels across do not fit an atlas {} wide",
                self.width
            )));
        }
        if self.pen_x + width + PADDING > self.width {
            self.pen_x = PADDING;
            self.pen_y += self.row_height + PADDING;
            self.row_height = 0;
        }
        let rows_needed = (self.pen_y + height + PADDING) as usize;
        let rows_held = self.texels.len() / (self.width as usize * 4);
        if rows_needed > rows_held {
            let added = (rows_needed - rows_held) * self.width as usize;
            self.texels
                .extend(std::iter::repeat_n([u8::MAX, u8::MAX, u8::MAX, 0], added).flatten());
        }
        let region = Region {
            x: self.pen_x,
            y: self.pen_y,
            width,
            height,
        };
        self.pen_x += width + PADDING;
        self.row_height = self.row_height.max(height);
        Ok(region)
    }

    fn texel(&self, x: usize, y: usize) -> usize {
        (y * self.width as usize + x) * 4
    }
}

/// The baked faces, masks and images, as one RGBA8 sRGB image and the places in it.
///
/// Everything a layout needs is here without a device, so screens can be laid out, measured and
/// rasterised in tests; [`Atlas::upload`] makes the texture for drawing.
#[derive(Debug)]
pub struct Atlas {
    width: u32,
    height: u32,
    texels: Vec<u8>,
    solid: Region,
    faces: Vec<FaceMetrics>,
    pub(crate) glyphs: HashMap<(FaceId, char), Glyph>,
}

impl Atlas {
    /// Width in texels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in texels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The image: RGBA8, sRGB, `width` texels a row.
    #[must_use]
    pub fn texels(&self) -> &[u8] {
        &self.texels
    }

    /// Uploads the atlas as a single-level RGBA8 sRGB texture, to bind to the overlay pipeline's
    /// texture slot.
    ///
    /// # Errors
    ///
    /// The device's upload errors.
    pub fn upload(&self, device: &Device<'_>) -> Result<Texture, GraphicsError> {
        device.create_texture(&TextureDescriptor {
            shape: TextureShape::D2 {
                width: self.width,
                height: self.height,
            },
            mips: MipChain::Single,
            data: TexelData::Rgba8Srgb(&[&self.texels]),
        })
    }

    /// How `face` sits on a line.
    ///
    /// # Panics
    ///
    /// When `face` is from another atlas with more faces than this one.
    #[must_use]
    pub fn metrics(&self, face: FaceId) -> FaceMetrics {
        self.faces[face.0 as usize]
    }

    /// Whether `face` has a glyph for `character`.
    #[must_use]
    pub fn has_glyph(&self, face: FaceId, character: char) -> bool {
        self.glyphs.contains_key(&(face, character))
    }

    /// The texture coordinates of `region`: left, top, right, bottom.
    #[must_use]
    pub fn uv(&self, region: Region) -> [f32; 4] {
        #[allow(clippy::cast_precision_loss)]
        let (width, height) = (self.width as f32, self.height as f32);
        #[allow(clippy::cast_precision_loss)]
        [
            region.x as f32 / width,
            region.y as f32 / height,
            (region.x + region.width) as f32 / width,
            (region.y + region.height) as f32 / height,
        ]
    }

    /// Where flat shapes sample: the middle of the solid block, opaque white.
    pub(crate) fn solid_uv(&self) -> [f32; 2] {
        let [left, top, right, bottom] = self.uv(self.solid);
        [(left + right) * 0.5, (top + bottom) * 0.5]
    }

    /// The glyph `character` draws with in `face`: its own, or the space's in its place.
    pub(crate) fn glyph(&self, face: FaceId, character: char) -> Option<&Glyph> {
        self.glyphs
            .get(&(face, character))
            .or_else(|| self.glyphs.get(&(face, ' ')))
    }
}
