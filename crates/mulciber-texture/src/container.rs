//! The writer side of the KTX 2.0 container that [`mulciber::Ktx2Texture`] reads. The [`ktx2`]
//! crate serialises the header, the level index and the data format descriptor (generated from the
//! format); this lays the sections out and writes the key/value data.

use std::path::Path;

use mulciber::{BlockCompression, ktx2_vk_format};

use crate::TextureError;
use crate::chain::{chain_len, mip_extent};

/// The key a bake records its [`Recipe::digest`](crate::Recipe::digest) under, as 16 hex digits.
pub const SOURCE_DIGEST_KEY: &str = "MulciberSourceDigest";
/// The key a bake records its base level's mean under: four decimal numbers in 0..1, RGB in linear
/// light for an sRGB texture and as stored otherwise, then alpha.
pub const MEAN_KEY: &str = "MulciberMean";
/// The key a bake records its base level's smallest alpha byte under.
pub const MIN_ALPHA_KEY: &str = "MulciberMinAlpha";

const WRITER_KEY: &str = "KTXwriter";
/// Level data starts on this boundary: the least common multiple of the block size and four.
const LEVEL_ALIGNMENT: usize = 16;

/// Bytes a level of this extent takes in `compression`: whole 4×4 blocks.
#[must_use]
pub fn level_bytes(compression: BlockCompression, width: u32, height: u32) -> usize {
    let block = match compression {
        BlockCompression::Bc1Srgb | BlockCompression::Bc1Unorm => 8,
        _ => 16,
    };
    width.div_ceil(4) as usize * height.div_ceil(4) as usize * block
}

fn key_value_data(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut block = Vec::new();
    for (key, value) in entries {
        let length = key.len() + 1 + value.len() + 1;
        block.extend_from_slice(&u32::try_from(length).expect("a short entry").to_le_bytes());
        block.extend_from_slice(key.as_bytes());
        block.push(0);
        block.extend_from_slice(value.as_bytes());
        block.push(0);
        while block.len() % 4 != 0 {
            block.push(0);
        }
    }
    block
}

/// A KTX 2.0 file of a block-compressed 2D texture: the base level alone or its complete chain to
/// 1×1, each level [`level_bytes`] long, with `entries` in the key/value block after `KTXwriter`. Levels
/// are stored smallest first on sixteen-byte boundaries, as the specification recommends.
///
/// # Errors
///
/// For a level count that is neither one nor the complete chain, or a
/// level of the wrong size.
pub fn ktx2_bytes(
    compression: BlockCompression,
    width: u32,
    height: u32,
    levels: &[Vec<u8>],
    entries: &[(&str, &str)],
) -> Result<Vec<u8>, TextureError> {
    let level_count = levels.len();
    if width == 0 || height == 0 || (level_count != 1 && level_count != chain_len(width, height)) {
        return Err(TextureError::new(format!(
            "{level_count} levels for {width}x{height}; a bake holds 1 or the complete chain of {}",
            chain_len(width, height)
        )));
    }
    for (level, blocks) in (0_u32..).zip(levels) {
        let expected = level_bytes(
            compression,
            mip_extent(width, level),
            mip_extent(height, level),
        );
        if blocks.len() != expected {
            return Err(TextureError::new(format!(
                "level {level} holds {} bytes, expected {expected}",
                blocks.len()
            )));
        }
    }
    let format = ktx2::Format::new(ktx2_vk_format(compression))
        .ok_or_else(|| TextureError::new(format!("no VkFormat for {compression:?}")))?;
    let word = |value: usize| u32::try_from(value).map_err(|_| TextureError::new("bake too large"));
    let (descriptor, type_size) = ktx2::dfd::Basic::from_format(format)
        .map_err(|error| TextureError::new(format!("describe {compression:?}: {error}")))?;
    let block = ktx2::dfd::Block::Basic(descriptor).to_vec();
    let mut dfd = word(4 + block.len())?.to_le_bytes().to_vec();
    dfd.extend_from_slice(&block);
    let mut all = vec![(WRITER_KEY, "mulciber-texture")];
    all.extend_from_slice(entries);
    let kvd = key_value_data(&all);
    let dfd_offset = ktx2::Header::LENGTH + ktx2::LevelIndex::LENGTH * level_count;
    let kvd_offset = dfd_offset + dfd.len();
    let mut end = kvd_offset + kvd.len();
    let mut offsets = vec![0_usize; level_count];
    for level in (0..level_count).rev() {
        end = end.next_multiple_of(LEVEL_ALIGNMENT);
        offsets[level] = end;
        end += levels[level].len();
    }
    let header = ktx2::Header {
        format: Some(format),
        type_size,
        pixel_width: width,
        pixel_height: height,
        pixel_depth: 0,
        layer_count: 0,
        face_count: 1,
        level_count: word(level_count)?,
        supercompression_scheme: None,
        index: ktx2::Index {
            dfd_byte_offset: word(dfd_offset)?,
            dfd_byte_length: word(dfd.len())?,
            kvd_byte_offset: word(kvd_offset)?,
            kvd_byte_length: word(kvd.len())?,
            sgd_byte_offset: 0,
            sgd_byte_length: 0,
        },
    };
    let mut file = Vec::with_capacity(end);
    file.extend_from_slice(&header.as_bytes());
    for (offset, blocks) in offsets.iter().zip(levels) {
        let index = ktx2::LevelIndex {
            byte_offset: *offset as u64,
            byte_length: blocks.len() as u64,
            uncompressed_byte_length: blocks.len() as u64,
        };
        file.extend_from_slice(&index.as_bytes());
    }
    file.extend_from_slice(&dfd);
    file.extend_from_slice(&kvd);
    for level in (0..level_count).rev() {
        file.resize(offsets[level], 0);
        file.extend_from_slice(&levels[level]);
    }
    Ok(file)
}

/// Writes [`ktx2_bytes`] to `path`.
///
/// # Errors
///
/// Those of [`ktx2_bytes`], and a failed write, naming the path.
pub fn write_ktx2(
    path: &Path,
    compression: BlockCompression,
    width: u32,
    height: u32,
    levels: &[Vec<u8>],
    entries: &[(&str, &str)],
) -> Result<(), TextureError> {
    let bytes =
        ktx2_bytes(compression, width, height, levels, entries).map_err(|error| error.at(path))?;
    std::fs::write(path, bytes)
        .map_err(|error| TextureError::new(format!("write {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use mulciber::Ktx2Texture;

    use super::*;

    #[test]
    fn a_bake_reads_back_through_mulcibers_reader() {
        // 8x8 down to 1x1: four blocks, then one each for 4x4, 2x2, 1x1.
        let levels: Vec<Vec<u8>> = [64, 16, 16, 16]
            .iter()
            .enumerate()
            .map(|(level, &len)| {
                (0..len)
                    .map(|i| u8::try_from((i * 7 + level * 31) % 256).unwrap())
                    .collect()
            })
            .collect();
        for compression in [
            BlockCompression::Bc7Srgb,
            BlockCompression::Bc7Unorm,
            BlockCompression::Bc5Unorm,
            BlockCompression::Bc3Srgb,
            BlockCompression::Bc2Unorm,
        ] {
            let bytes =
                ktx2_bytes(compression, 8, 8, &levels, &[(SOURCE_DIGEST_KEY, "beef")]).unwrap();
            let read = Ktx2Texture::parse(&bytes).unwrap();
            assert_eq!(read.compression(), compression);
            assert_eq!((read.width(), read.height(), read.level_count()), (8, 8, 4));
            assert_eq!(
                read.levels(),
                levels.iter().map(Vec::as_slice).collect::<Vec<_>>()
            );
            // The descriptor the ktx2 crate generated reads back as BC7, BC5... with the
            // transfer function the format implies.
            let reader = ktx2::Reader::new(bytes.as_slice()).unwrap();
            let srgb = matches!(
                compression,
                BlockCompression::Bc7Srgb | BlockCompression::Bc3Srgb
            );
            assert_eq!(
                reader.transfer_function(),
                Some(if srgb {
                    ktx2::TransferFunction::SRGB
                } else {
                    ktx2::TransferFunction::Linear
                })
            );
            assert_eq!(read.value(SOURCE_DIGEST_KEY), Some(&b"beef"[..]));
            assert_eq!(read.value(WRITER_KEY), Some(&b"mulciber-texture"[..]));
            // Every level starts on a block boundary.
            for level in 0..4 {
                let offset = read.level(level).unwrap().as_ptr() as usize - bytes.as_ptr() as usize;
                assert_eq!(offset % 16, 0);
            }
        }
    }

    #[test]
    fn a_non_square_chain_writes_partial_blocks() {
        // 12x5: 3x2 blocks, then 6x2 (2x1), 3x1, 1x1.
        let levels: Vec<Vec<u8>> = [96, 32, 16, 16].iter().map(|&n| vec![9; n]).collect();
        let bytes = ktx2_bytes(BlockCompression::Bc7Unorm, 12, 5, &levels, &[]).unwrap();
        assert_eq!(Ktx2Texture::parse(&bytes).unwrap().level_count(), 4);
    }

    #[test]
    fn levels_that_do_not_fit_the_extent_are_refused() {
        let short = vec![vec![0; 64], vec![0; 16], vec![0; 16]];
        let error = ktx2_bytes(BlockCompression::Bc7Srgb, 8, 8, &short, &[]).unwrap_err();
        assert!(error.to_string().contains("complete chain of 4"), "{error}");
        let wrong = vec![vec![0; 64], vec![0; 16], vec![0; 16], vec![0; 4]];
        let error = ktx2_bytes(BlockCompression::Bc7Srgb, 8, 8, &wrong, &[]).unwrap_err();
        assert!(
            error.to_string().contains("level 3 holds 4 bytes"),
            "{error}"
        );
    }
}
