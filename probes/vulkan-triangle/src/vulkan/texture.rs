use std::env;

use crate::vk;

use super::{ProbeError, RGBA8_TEXEL_SIZE};

pub(super) const TEXTURE_WIDTH: u32 = 8;
pub(super) const TEXTURE_HEIGHT: u32 = 8;
const BC1_BLOCK_WIDTH: usize = 4;
const BC1_BYTES_PER_BLOCK: usize = 8;
const BC1_REQUIRED_FORMAT_FEATURES: u32 = (vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT
    | vk::VK_FORMAT_FEATURE_TRANSFER_SRC_BIT
    | vk::VK_FORMAT_FEATURE_TRANSFER_DST_BIT) as u32;
const BC1_BLOCKS: [u8; 32] = bc1_blocks();
const CHECKERBOARD_TEXELS: [u8; TEXTURE_WIDTH as usize
    * TEXTURE_HEIGHT as usize
    * RGBA8_TEXEL_SIZE] = checkerboard_texels();

const fn bc1_blocks() -> [u8; 32] {
    const BRIGHT: u16 = (30 << 11) | (60 << 5) | 31;
    const DARK: u16 = (5 << 11) | (16 << 5) | 5;
    let mut blocks = [0; 32];
    let mut block = 0;
    while block < 4 {
        let endpoint = if block == 0 || block == 3 {
            BRIGHT
        } else {
            DARK
        };
        let offset = block * BC1_BYTES_PER_BLOCK;
        let bytes = endpoint.to_le_bytes();
        blocks[offset] = bytes[0];
        blocks[offset + 1] = bytes[1];
        block += 1;
    }
    blocks
}

const fn checkerboard_texels()
-> [u8; TEXTURE_WIDTH as usize * TEXTURE_HEIGHT as usize * RGBA8_TEXEL_SIZE] {
    let mut pixels = [0; TEXTURE_WIDTH as usize * TEXTURE_HEIGHT as usize * RGBA8_TEXEL_SIZE];
    let mut y = 0;
    while y < TEXTURE_HEIGHT as usize {
        let mut x = 0;
        while x < TEXTURE_WIDTH as usize {
            let offset = (y * TEXTURE_WIDTH as usize + x) * RGBA8_TEXEL_SIZE;
            let bright = (x / BC1_BLOCK_WIDTH + y / BC1_BLOCK_WIDTH).is_multiple_of(2);
            pixels[offset] = if bright { 247 } else { 41 };
            pixels[offset + 1] = if bright { 243 } else { 65 };
            pixels[offset + 2] = if bright { 255 } else { 41 };
            pixels[offset + 3] = 255;
            x += 1;
        }
        y += 1;
    }
    pixels
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TextureMode {
    Auto,
    Bc1,
    Rgba8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TexturePath {
    Bc1,
    Rgba8,
}

impl TexturePath {
    pub(super) const fn format(self) -> vk::VkFormat {
        match self {
            Self::Bc1 => vk::VK_FORMAT_BC1_RGBA_UNORM_BLOCK,
            Self::Rgba8 => vk::VK_FORMAT_R8G8B8A8_UNORM,
        }
    }

    pub(super) const fn upload_bytes(self) -> &'static [u8] {
        match self {
            Self::Bc1 => &BC1_BLOCKS,
            Self::Rgba8 => &CHECKERBOARD_TEXELS,
        }
    }

    pub(super) const fn diagnostic_name(self) -> &'static str {
        match self {
            Self::Bc1 => "BC1_RGBA_UNORM direct sampling",
            Self::Rgba8 => "RGBA8 fallback",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Bc1Support {
    pub(super) core_feature: bool,
    pub(super) optimal_tiling_features: u32,
}

impl Bc1Support {
    const fn complete(self) -> bool {
        self.core_feature
            && self.optimal_tiling_features & BC1_REQUIRED_FORMAT_FEATURES
                == BC1_REQUIRED_FORMAT_FEATURES
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TextureSelection {
    pub(super) mode: TextureMode,
    pub(super) path: TexturePath,
    pub(super) bc1: Bc1Support,
}

pub(super) fn texture_mode_from_environment() -> Result<TextureMode, ProbeError> {
    let Some(value) = env::var_os("MULCIBER_VULKAN_TEXTURE_MODE") else {
        return Ok(TextureMode::Auto);
    };
    let value = value.to_str().ok_or_else(|| {
        ProbeError("MULCIBER_VULKAN_TEXTURE_MODE contains non-Unicode data".into())
    })?;
    match value {
        "auto" => Ok(TextureMode::Auto),
        "bc1" => Ok(TextureMode::Bc1),
        "rgba8" => Ok(TextureMode::Rgba8),
        _ => Err(ProbeError(format!(
            "invalid MULCIBER_VULKAN_TEXTURE_MODE={value:?}; expected auto, bc1, or rgba8"
        ))),
    }
}

pub(super) fn missing_bc1_requirements(support: Bc1Support) -> String {
    let mut missing = Vec::new();
    if !support.core_feature {
        missing.push("textureCompressionBC");
    }
    for (feature, name) in [
        (
            vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT as u32,
            "SAMPLED_IMAGE",
        ),
        (
            vk::VK_FORMAT_FEATURE_TRANSFER_DST_BIT as u32,
            "TRANSFER_DST",
        ),
        (
            vk::VK_FORMAT_FEATURE_TRANSFER_SRC_BIT as u32,
            "TRANSFER_SRC",
        ),
    ] {
        if support.optimal_tiling_features & feature == 0 {
            missing.push(name);
        }
    }
    missing.join(", ")
}

pub(super) fn select_texture(
    mode: TextureMode,
    bc1: Bc1Support,
) -> Result<TextureSelection, ProbeError> {
    let path = match mode {
        TextureMode::Rgba8 => TexturePath::Rgba8,
        TextureMode::Auto => {
            if bc1.complete() {
                TexturePath::Bc1
            } else {
                TexturePath::Rgba8
            }
        }
        TextureMode::Bc1 => {
            if bc1.complete() {
                TexturePath::Bc1
            } else {
                return Err(ProbeError(format!(
                    "required BC1 texture path is unavailable: {}",
                    missing_bc1_requirements(bc1)
                )));
            }
        }
    };
    Ok(TextureSelection { mode, path, bc1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_matches_the_metal_checkerboard() {
        assert_eq!(BC1_BLOCKS.len(), 32);
        assert_eq!(CHECKERBOARD_TEXELS.len(), 256);
        assert_eq!(&CHECKERBOARD_TEXELS[0..4], &[247, 243, 255, 255]);
        assert_eq!(&CHECKERBOARD_TEXELS[4 * 4..4 * 5], &[41, 65, 41, 255]);
        assert_eq!(
            &CHECKERBOARD_TEXELS[(4 * 8 + 4) * 4..(4 * 8 + 5) * 4],
            &[247, 243, 255, 255]
        );
    }

    #[test]
    fn selection_requires_core_and_every_used_format_role() {
        let complete = Bc1Support {
            core_feature: true,
            optimal_tiling_features: BC1_REQUIRED_FORMAT_FEATURES,
        };
        assert_eq!(
            select_texture(TextureMode::Auto, complete)
                .expect("complete BC1 support")
                .path,
            TexturePath::Bc1
        );
        assert_eq!(
            select_texture(TextureMode::Rgba8, complete)
                .expect("forced fallback")
                .path,
            TexturePath::Rgba8
        );

        for missing in [
            Bc1Support {
                core_feature: false,
                ..complete
            },
            Bc1Support {
                optimal_tiling_features: complete.optimal_tiling_features
                    & !(vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT as u32),
                ..complete
            },
            Bc1Support {
                optimal_tiling_features: complete.optimal_tiling_features
                    & !(vk::VK_FORMAT_FEATURE_TRANSFER_DST_BIT as u32),
                ..complete
            },
            Bc1Support {
                optimal_tiling_features: complete.optimal_tiling_features
                    & !(vk::VK_FORMAT_FEATURE_TRANSFER_SRC_BIT as u32),
                ..complete
            },
        ] {
            assert_eq!(
                select_texture(TextureMode::Auto, missing)
                    .expect("auto fallback")
                    .path,
                TexturePath::Rgba8
            );
            assert!(select_texture(TextureMode::Bc1, missing).is_err());
        }
    }
}
