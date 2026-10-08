//! KTX 2.0 containers of block-compressed 2D textures, parsed in place.
//!
//! The [`ktx2`] crate parses the container (Khronos' KTX 2.0: header, level index, data format
//! descriptor, key/value data) and checks every section's bounds. On top of that, Mulciber accepts
//! only what the GPU samples directly: one 2D image, not supercompressed, in one of the
//! [`BlockCompression`] encodings, holding its base level alone or the complete chain to 1×1,
//! every level exactly its extent in blocks. Everything else is refused by name rather than
//! transcoded.

use std::format;
use std::vec::Vec;

use super::{BlockCompression, GraphicsError, GraphicsErrorKind, full_mip_chain_len, mip_extent};

/// The `VkFormat` a KTX 2.0 header records for each encoding.
#[must_use]
pub const fn ktx2_vk_format(compression: BlockCompression) -> u32 {
    match compression {
        BlockCompression::Bc1Unorm => 133,
        BlockCompression::Bc1Srgb => 134,
        BlockCompression::Bc2Unorm => 135,
        BlockCompression::Bc2Srgb => 136,
        BlockCompression::Bc3Unorm => 137,
        BlockCompression::Bc3Srgb => 138,
        BlockCompression::Bc5Unorm => 141,
        BlockCompression::Bc7Unorm => 145,
        BlockCompression::Bc7Srgb => 146,
    }
}

const fn compression_of(vk_format: u32) -> Option<BlockCompression> {
    Some(match vk_format {
        133 => BlockCompression::Bc1Unorm,
        134 => BlockCompression::Bc1Srgb,
        135 => BlockCompression::Bc2Unorm,
        136 => BlockCompression::Bc2Srgb,
        137 => BlockCompression::Bc3Unorm,
        138 => BlockCompression::Bc3Srgb,
        141 => BlockCompression::Bc5Unorm,
        145 => BlockCompression::Bc7Unorm,
        146 => BlockCompression::Bc7Srgb,
        _ => return None,
    })
}

/// A KTX 2.0 file holding a block-compressed 2D texture, validated and borrowed in place.
///
/// [`parse`](Self::parse) checks the container, every level's byte count against its extent in
/// blocks, and the shape, so [`Device::create_ktx2_texture`](super::Device::create_ktx2_texture)
/// uploads the levels as they are.
#[derive(Clone)]
pub struct Ktx2Texture<'a> {
    compression: BlockCompression,
    width: u32,
    height: u32,
    level_count: u32,
    levels: Vec<&'a [u8]>,
    key_values: &'a [u8],
}

impl core::fmt::Debug for Ktx2Texture<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ktx2Texture")
            .field("compression", &self.compression)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("level_count", &self.level_count)
            .finish_non_exhaustive()
    }
}

fn malformed(what: &str) -> GraphicsError {
    GraphicsError::invalid_request(format!("KTX2: {what}"))
}

fn unsupported(what: &str) -> GraphicsError {
    GraphicsError::with_kind(GraphicsErrorKind::Unsupported, format!("KTX2: {what}"))
}

impl<'a> Ktx2Texture<'a> {
    /// Parses and validates a KTX 2.0 file.
    ///
    /// # Errors
    ///
    /// `InvalidRequest` for a file that is not well-formed KTX 2.0 (the [`ktx2`] crate's checks:
    /// identifier, section and level bounds, the data format descriptor), a level whose byte count
    /// does not match its extent in blocks, or a level count that is neither one nor the complete
    /// chain to 1×1. `Unsupported` for well-formed files Mulciber does not upload: formats other
    /// than [`BlockCompression`]'s, 1D, 3D, array or cube textures, and supercompression.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, GraphicsError> {
        let reader = ktx2::Reader::new(bytes).map_err(|error| match error {
            ktx2::ParseError::BadMagic => malformed("not a KTX 2.0 file"),
            other => malformed(&format!("malformed or truncated: {other}")),
        })?;
        let header = reader.header();
        let vk_format = header.format.map_or(0, |format| format.value());
        let compression = compression_of(vk_format).ok_or_else(|| {
            unsupported(&format!(
                "VkFormat {vk_format} is not a block compression Mulciber uploads"
            ))
        })?;
        let (width, height) = (header.pixel_width, header.pixel_height);
        let (depth, layers, faces) = (header.pixel_depth, header.layer_count, header.face_count);
        if height == 0 || depth != 0 || layers != 0 || faces != 1 {
            return Err(unsupported(&format!(
                "only single 2D images upload; this is {width}x{height}x{depth} with {layers} \
                 layers and {faces} faces"
            )));
        }
        if let Some(scheme) = header.supercompression_scheme {
            return Err(unsupported(&format!(
                "supercompressed level data ({scheme:?}) does not upload"
            )));
        }
        let level_count = header.level_count;
        let full = full_mip_chain_len(width, height);
        if level_count == 0 {
            return Err(malformed(
                "level count 0 asks the loader to generate mips, which block-compressed data \
                 cannot have",
            ));
        }
        if level_count != 1 && level_count as usize != full {
            return Err(malformed(&format!(
                "{level_count} levels for {width}x{height}, which takes 1 or the complete \
                 chain of {full}"
            )));
        }
        let format = compression.sampled();
        let mut levels = Vec::with_capacity(level_count as usize);
        for (level, stored) in (0_u32..).zip(reader.levels()) {
            let (w, h) = (mip_extent(width, level), mip_extent(height, level));
            let expected = format
                .level_bytes(w, h)
                .ok_or_else(|| malformed("the texture's dimensions overflow"))?;
            let length = stored.data.len();
            if length != expected {
                return Err(malformed(&format!(
                    "level {level} ({w}x{h}) holds {length} bytes; {compression:?} needs \
                     {expected}"
                )));
            }
            if stored.uncompressed_byte_length != length as u64 {
                return Err(malformed(&format!(
                    "level {level}'s uncompressed length differs from its stored length"
                )));
            }
            // The reader borrows itself; cut the same range from the input instead.
            let start = stored.data.as_ptr().addr() - bytes.as_ptr().addr();
            levels.push(&bytes[start..start + length]);
        }
        let start = header.index.kvd_byte_offset as usize;
        let key_values = &bytes[start..start + header.index.kvd_byte_length as usize];
        Ok(Self {
            compression,
            width,
            height,
            level_count,
            levels,
            key_values,
        })
    }

    /// The block encoding the levels hold.
    #[must_use]
    pub const fn compression(&self) -> BlockCompression {
        self.compression
    }

    /// The base level's width in texels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The base level's height in texels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Levels stored: one, or the complete chain to 1×1.
    #[must_use]
    pub const fn level_count(&self) -> u32 {
        self.level_count
    }

    /// Level `level`'s tightly packed blocks, base level 0, or `None` past the last level.
    #[must_use]
    pub fn level(&self, level: u32) -> Option<&'a [u8]> {
        self.levels.get(level as usize).copied()
    }

    /// Every level's blocks, base level first.
    #[must_use]
    pub fn levels(&self) -> &[&'a [u8]] {
        &self.levels
    }

    /// The value stored under `key` in the key/value data, without its trailing NUL, or `None`.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<&'a [u8]> {
        self.key_values()
            .into_iter()
            .find_map(|(k, value)| (k == key).then_some(value))
    }

    /// Every well-formed key/value entry in file order, values without their trailing NUL.
    #[must_use]
    pub fn key_values(&self) -> Vec<(&'a str, &'a [u8])> {
        ktx2::KeyValueDataIterator::new(self.key_values)
            .map(|(key, value)| (key, value.strip_suffix(&[0]).unwrap_or(value)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::vec;
    use std::vec::Vec;

    use super::{BlockCompression, GraphicsErrorKind, Ktx2Texture, ktx2_vk_format};

    const HEADER_BYTES: usize = 80;
    const LEVEL_INDEX_BYTES: usize = 24;

    /// A minimal writer following the specification, independent of the `ktx2` crate: levels
    /// stored smallest first on 16-byte boundaries, an empty descriptor, the given entries.
    fn file(
        compression: BlockCompression,
        width: u32,
        height: u32,
        levels: &[Vec<u8>],
        entries: &[(&str, &str)],
    ) -> Vec<u8> {
        let mut kvd = Vec::new();
        for (key, value) in entries {
            let length = u32::try_from(key.len() + value.len() + 2).unwrap();
            kvd.extend_from_slice(&length.to_le_bytes());
            kvd.extend_from_slice(key.as_bytes());
            kvd.push(0);
            kvd.extend_from_slice(value.as_bytes());
            kvd.push(0);
            while kvd.len() % 4 != 0 {
                kvd.push(0);
            }
        }
        let dfd = 4_u32.to_le_bytes().to_vec();
        let dfd_offset = HEADER_BYTES + LEVEL_INDEX_BYTES * levels.len();
        let kvd_offset = dfd_offset + dfd.len();
        let mut end = kvd_offset + kvd.len();
        let mut offsets = vec![0; levels.len()];
        for level in (0..levels.len()).rev() {
            end = end.next_multiple_of(16);
            offsets[level] = end;
            end += levels[level].len();
        }
        let mut bytes = vec![
            0xAB, b'K', b'T', b'X', b' ', b'2', b'0', 0xBB, b'\r', b'\n', 0x1A, b'\n',
        ];
        for word in [
            ktx2_vk_format(compression),
            1,
            width,
            height,
            0,
            0,
            1,
            u32::try_from(levels.len()).unwrap(),
            0,
            u32::try_from(dfd_offset).unwrap(),
            u32::try_from(dfd.len()).unwrap(),
            u32::try_from(kvd_offset).unwrap(),
            u32::try_from(kvd.len()).unwrap(),
        ] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&[0; 16]);
        for (level, data) in levels.iter().enumerate() {
            let length = data.len() as u64;
            bytes.extend_from_slice(&(offsets[level] as u64).to_le_bytes());
            bytes.extend_from_slice(&length.to_le_bytes());
            bytes.extend_from_slice(&length.to_le_bytes());
        }
        bytes.extend_from_slice(&dfd);
        bytes.extend_from_slice(&kvd);
        for level in (0..levels.len()).rev() {
            bytes.resize(offsets[level], 0);
            bytes.extend_from_slice(&levels[level]);
        }
        bytes
    }

    /// A 12x8 chain: 3x2 blocks, then 6x4 (2x1), 3x2, 1x1 (one block each).
    fn chain() -> Vec<Vec<u8>> {
        [96, 32, 16, 16]
            .iter()
            .enumerate()
            .map(|(level, &len)| {
                (0..len)
                    .map(|i| u8::try_from((i * 3 + level * 50) % 256).unwrap())
                    .collect()
            })
            .collect()
    }

    fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn a_complete_chain_parses_with_its_levels_and_values() {
        let levels = chain();
        let bytes = file(
            BlockCompression::Bc7Srgb,
            12,
            8,
            &levels,
            &[("KTXwriter", "test"), ("Digest", "00ff")],
        );
        let texture = Ktx2Texture::parse(&bytes).unwrap();
        assert_eq!(texture.compression(), BlockCompression::Bc7Srgb);
        assert_eq!((texture.width(), texture.height()), (12, 8));
        assert_eq!(texture.level_count(), 4);
        let read: Vec<Vec<u8>> = texture.levels().iter().map(|l| l.to_vec()).collect();
        assert_eq!(read, levels);
        assert_eq!(texture.level(4), None);
        assert_eq!(texture.value("Digest"), Some(&b"00ff"[..]));
        assert_eq!(texture.value("Missing"), None);
        assert_eq!(
            texture.key_values(),
            vec![("KTXwriter", &b"test"[..]), ("Digest", &b"00ff"[..])]
        );
    }

    #[test]
    fn every_block_compression_round_trips_its_vk_format() {
        for compression in [
            BlockCompression::Bc7Srgb,
            BlockCompression::Bc7Unorm,
            BlockCompression::Bc5Unorm,
            BlockCompression::Bc1Srgb,
            BlockCompression::Bc1Unorm,
            BlockCompression::Bc2Srgb,
            BlockCompression::Bc2Unorm,
            BlockCompression::Bc3Srgb,
            BlockCompression::Bc3Unorm,
        ] {
            let block = compression.sampled().block_bytes();
            let bytes = file(compression, 4, 4, &[vec![7; block]], &[]);
            let texture = Ktx2Texture::parse(&bytes).unwrap();
            assert_eq!(texture.compression(), compression);
            assert_eq!(texture.levels(), &[&vec![7; block][..]]);
        }
    }

    #[test]
    fn a_base_level_alone_parses() {
        let bytes = file(BlockCompression::Bc7Unorm, 12, 8, &chain()[..1], &[]);
        assert_eq!(Ktx2Texture::parse(&bytes).unwrap().level_count(), 1);
    }

    #[test]
    fn malformed_and_unsupported_files_are_refused_by_what_is_wrong() {
        let good = file(BlockCompression::Bc7Srgb, 12, 8, &chain(), &[("K", "V")]);
        let refuse = |bytes: &[u8], kind: GraphicsErrorKind, says: &str| {
            let error = Ktx2Texture::parse(bytes).unwrap_err();
            assert_eq!(error.kind(), kind, "{}", error.message());
            assert!(error.message().contains(says), "{}", error.message());
        };
        refuse(
            b"not a texture, but long enough to hold a whole KTX2 header if it were one: \
              eighty bytes or more",
            GraphicsErrorKind::InvalidRequest,
            "not a KTX 2.0",
        );
        refuse(
            &good[..good.len() - 5],
            GraphicsErrorKind::InvalidRequest,
            "truncated",
        );
        let mut partial = file(BlockCompression::Bc7Srgb, 12, 8, &chain()[..2], &[]);
        refuse(
            &partial,
            GraphicsErrorKind::InvalidRequest,
            "complete chain of 4",
        );
        set_u32(&mut partial, 40, 0);
        refuse(&partial, GraphicsErrorKind::InvalidRequest, "generate mips");
        let mut short = good.clone();
        // Level 1's byte length and uncompressed byte length.
        set_u32(&mut short, HEADER_BYTES + LEVEL_INDEX_BYTES + 8, 16);
        set_u32(&mut short, HEADER_BYTES + LEVEL_INDEX_BYTES + 16, 16);
        refuse(
            &short,
            GraphicsErrorKind::InvalidRequest,
            "level 1 (6x4) holds 16",
        );
        let mut format = good.clone();
        set_u32(&mut format, 12, 37); // R8G8B8A8_UNORM
        refuse(&format, GraphicsErrorKind::Unsupported, "VkFormat 37");
        let mut cube = good.clone();
        set_u32(&mut cube, 36, 6);
        refuse(&cube, GraphicsErrorKind::Unsupported, "6 faces");
        let mut array = good.clone();
        set_u32(&mut array, 32, 2);
        refuse(&array, GraphicsErrorKind::Unsupported, "2 layers");
        let mut volume = good.clone();
        set_u32(&mut volume, 28, 4);
        refuse(&volume, GraphicsErrorKind::Unsupported, "12x8x4");
        let mut supercompressed = good.clone();
        set_u32(&mut supercompressed, 44, 2);
        refuse(
            &supercompressed,
            GraphicsErrorKind::Unsupported,
            "supercompressed",
        );
    }
}
