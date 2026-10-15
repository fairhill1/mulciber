//! BC6H light, encoded and decoded again, and its chains.
#![cfg(feature = "encode")]

use mulciber::{BlockCompression, Ktx2Texture};
use mulciber_texture::{encode_bc6h, hdr_chain, ktx2_bytes, level_bytes};

/// Decodes BC6H unsigned blocks back to RGB floats, `width` × `height`.
fn decode(blocks: &[u8], width: u32, height: u32) -> Vec<[f32; 3]> {
    let (w, h) = (width as usize, height as usize);
    let across = w.div_ceil(4);
    let mut out = vec![[0.0; 3]; w * h];
    for (index, block) in blocks.chunks_exact(16).enumerate() {
        let (bx, by) = (index % across, index / across);
        let mut texels = [0.0_f32; 4 * 4 * 3];
        bcdec_rs::bc6h_float(block, &mut texels, 4 * 3, false);
        for y in 0..4 {
            for x in 0..4 {
                let (px, py) = (bx * 4 + x, by * 4 + y);
                if px < w && py < h {
                    let at = (y * 4 + x) * 3;
                    out[py * w + px] = [texels[at], texels[at + 1], texels[at + 2]];
                }
            }
        }
    }
    out
}

/// Sky-like light: a smooth gradient over three decades, and a sun far brighter than it.
#[allow(clippy::cast_precision_loss)]
fn light(width: u32, height: u32) -> Vec<[f32; 4]> {
    (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            let sky = 0.05 * 1000.0_f32.powf(1.0 - v);
            let sun = if (u - 0.7).hypot(v - 0.2) < 0.03 {
                20_000.0
            } else {
                0.0
            };
            [sky * 0.6 + sun, sky * 0.8 + sun, sky + sun, 1.0]
        })
        .collect()
}

/// The worst error of `decoded` against `original`, relative to each texel's brightest channel:
/// BC6H's endpoints are shared by a block's channels.
fn worst_error(original: &[[f32; 4]], decoded: &[[f32; 3]]) -> f32 {
    original
        .iter()
        .zip(decoded)
        .flat_map(|(original, decoded)| {
            let scale = original[..3].iter().copied().fold(1.0e-3, f32::max);
            (0..3).map(move |channel| (original[channel] - decoded[channel]).abs() / scale)
        })
        .fold(0.0, f32::max)
}

#[test]
fn light_survives_encoding_within_bc6h_precision() {
    let (width, height) = (64, 32);
    let texels = light(width, height);
    let blocks = encode_bc6h(width, height, &texels).unwrap();
    assert_eq!(
        blocks.len(),
        level_bytes(BlockCompression::Bc6hUfloat, width, height)
    );
    let decoded = decode(&blocks, width, height);
    // The sky, three decades of smooth gradient, keeps a few percent. Blocks the sun's edge
    // crosses share their endpoints with it, so the sky beside the disc is left out here.
    let away: Vec<usize> = (0..texels.len())
        .filter(|&index| {
            let (x, y) = (index % 64, index / 64);
            (x / 4 * 4..x / 4 * 4 + 4)
                .flat_map(|bx| (y / 4 * 4..y / 4 * 4 + 4).map(move |by| by * 64 + bx))
                .all(|neighbour| texels[neighbour][2] < 10_000.0)
        })
        .collect();
    let (sky, sky_decoded): (Vec<[f32; 4]>, Vec<[f32; 3]>) =
        away.iter().map(|&i| (texels[i], decoded[i])).unzip();
    let worst = worst_error(&sky, &sky_decoded);
    // The gradient more than doubles across a block's four rows; BC6H's basic settings land
    // within about four percent of it at worst.
    assert!(worst < 0.05, "worst relative error in the sky {worst}");
    // The sun stays tens of thousands bright: light far past 1.0 is what BC6H is for.
    let sun = (0..texels.len()).filter(|&i| texels[i][2] > 10_000.0);
    for index in sun {
        assert!(decoded[index][2] > 15_000.0, "{:?}", decoded[index]);
    }
}

#[test]
fn odd_extents_and_flat_light_encode_whole_blocks() {
    // 5×3 is two blocks across and one down, padded by repeating the edge.
    let texels = vec![[2.5, 0.25, 40.0, 1.0]; 15];
    let blocks = encode_bc6h(5, 3, &texels).unwrap();
    assert_eq!(blocks.len(), 32);
    for decoded in decode(&blocks, 5, 3) {
        for (a, b) in decoded.iter().zip([2.5, 0.25, 40.0]) {
            assert!((a - b).abs() <= b * 0.01, "{decoded:?}");
        }
    }
    // Light out of BC6H's unsigned range is clamped; light that is not a number is refused.
    let blocks = encode_bc6h(4, 4, &[[-1.0, 1.0e9, 1.0, 1.0]; 16]).unwrap();
    let decoded = decode(&blocks, 4, 4)[0];
    assert!(
        decoded[0].abs() < 1.0e-3 && (decoded[1] - 65504.0).abs() < 700.0,
        "{decoded:?}"
    );
    assert!(encode_bc6h(4, 4, &[[f32::INFINITY; 4]; 16]).is_err());
    assert!(encode_bc6h(4, 4, &[[1.0; 4]; 15]).is_err());
}

#[test]
fn a_chain_of_light_bakes_to_ktx2_and_reads_back() {
    let (width, height) = (16, 8);
    let chain = hdr_chain(width, height, light(width, height));
    assert_eq!(chain.len(), 5);
    let levels: Vec<Vec<u8>> = (0_u32..)
        .zip(&chain)
        .map(|(level, texels)| {
            encode_bc6h((width >> level).max(1), (height >> level).max(1), texels).unwrap()
        })
        .collect();
    let bytes = ktx2_bytes(BlockCompression::Bc6hUfloat, width, height, &levels, &[]).unwrap();
    let read = Ktx2Texture::parse(&bytes).unwrap();
    assert_eq!(read.compression(), BlockCompression::Bc6hUfloat);
    assert_eq!(read.level_count(), 5);
    let reader = ktx2::Reader::new(bytes.as_slice()).unwrap();
    assert_eq!(
        reader.transfer_function(),
        Some(ktx2::TransferFunction::Linear)
    );
    // The 1×1 level is the mean light of the whole image, within BC6H's precision.
    #[allow(clippy::cast_precision_loss)]
    let mean: [f32; 3] = std::array::from_fn(|channel| {
        chain[0].iter().map(|texel| texel[channel]).sum::<f32>() / chain[0].len() as f32
    });
    let top = decode(&levels[4], 1, 1)[0];
    for (a, b) in top.iter().zip(mean) {
        assert!((a - b).abs() <= b * 0.02, "{top:?} vs {mean:?}");
    }
}
