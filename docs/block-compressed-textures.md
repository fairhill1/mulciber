# Block-compressed sampled uploads (0.13.15)

`Device::create_texture` with `TexelData::BlockCompressed(compression, &blocks)` uploads an already
encoded BC1, BC2, BC3, BC5, BC6H or BC7 image and returns the existing owning `Texture`: one level with
`MipChain::Single`, or a complete application-encoded chain with `MipChain::Supplied`. Both bind
through the existing material texture/sampler bindings and WGSL `texture_2d<f32>` in either stage,
exactly as an RGBA8 upload does; nothing in a shader changes when a texture moves from RGBA8 to
BC7. The same data uploads a cube with `TextureShape::Cube` (see [cube textures](cube-textures.md));
a cube array takes `RGBA16Float` or BC6H.

## What Mulciber does and does not do

Mulciber uploads blocks. It never encodes, decodes, transcodes, or filters them: the application
runs an encoder over each level of a mip chain it has already filtered in RGBA8 and hands the
resulting blocks over, and the GPU samples the blocks directly. That is what makes the upload a
quarter of its RGBA8 equivalent in video memory as well as on disk (BC6H an eighth of its
`RGBA16Float` one), and it is why
`MipChain::Generated` is refused with `InvalidRequest`: a block cannot be downsampled without
decoding it first.

```rust
use mulciber::{BlockCompression, MipChain, TexelData, TextureDescriptor, TextureShape};

let shape = TextureShape::D2 { width, height };
let albedo = device.create_texture(&TextureDescriptor {
    shape,
    mips: MipChain::Supplied,
    data: TexelData::BlockCompressed(BlockCompression::Bc7Srgb, &level_slices),
})?;
let normal = device.create_texture(&TextureDescriptor {
    shape,
    mips: MipChain::Supplied,
    data: TexelData::BlockCompressed(BlockCompression::Bc7Unorm, &normal_level_slices),
})?;
```

BC6H (graphics 0.18.0) holds light rather than colour: three channels of unsigned half float, sampled
linear and unclamped, for a sky, a lightmap or a reflection probe that would otherwise be
`RGBA16Float`. Only the unsigned variant exists, the one Intel's ISPC encoder writes; light is not
negative. [`mulciber-texture`](../crates/mulciber-texture/README.md) 0.6.0 builds its chain and encodes
it (`hdr_chain` averages light 2×2 down to 1×1, `encode_bc6h` encodes one level, clamping to 0..65504
and refusing non-finite values):

```rust
let levels = mulciber_texture::hdr_chain(width, height, sky);
let blocks = levels
    .iter()
    .enumerate()
    .map(|(level, texels)| {
        let (w, h) = ((width >> level).max(1), (height >> level).max(1));
        mulciber_texture::encode_bc6h(w, h, texels)
    })
    .collect::<Result<Vec<_>, _>>()?;
let slices: Vec<&[u8]> = blocks.iter().map(Vec::as_slice).collect();
let sky = device.create_texture(&TextureDescriptor {
    shape: TextureShape::D2 { width, height },
    mips: MipChain::Supplied,
    data: TexelData::BlockCompressed(BlockCompression::Bc6hUfloat, &slices),
})?;
```

## KTX 2.0 files

`Ktx2Texture::parse(&bytes)` reads a KTX 2.0 file in place and `descriptor()` describes its upload,
so `device.create_texture(&ktx.descriptor())` uploads it as above: its base level alone
(`MipChain::Single`), or the complete chain it stores (`MipChain::Supplied`). The
[`ktx2`](https://crates.io/crates/ktx2) crate (0.5.0) parses the container and checks its section
bounds and data format descriptor. Mulciber then accepts only what the GPU samples directly and
refuses the rest by name:

- `InvalidRequest`: not KTX 2.0, truncated, a level whose byte count is not its extent in blocks, a
  level count that is neither one nor the complete chain to 1×1 (0, "generate mips", included), or
  a level whose uncompressed length differs from its stored one.
- `Unsupported`: a `VkFormat` outside `BlockCompression`'s ten, 1D, 3D, array and cube textures,
  and any supercompression (Basis, Zstandard, zlib).

`width()`, `height()`, `compression()`, `level_count()`, `level(n)` and `levels()` describe what was
parsed; `value(key)` and `key_values()` read the key/value data, values without their trailing NUL.
`ktx2_vk_format(compression)` gives the `VkFormat` a writer records for each encoding.
[`mulciber-texture`](../crates/mulciber-texture/README.md) writes such files: it bakes textures to BC7
KTX 2.0 and reads them back through this parser.

## Input contract

Every encoding stores a 4×4 texel block, in eight bytes for BC1 and sixteen for the rest. BC1,
BC2 and BC3 are the DXT1, DXT3 and DXT5 encodings that older game data ships in, so it can be
uploaded as it is stored (BC1, BC2 and BC3 added in 0.13.32):

| `BlockCompression` | Channels | Sampled as | Vulkan | Metal |
|---|---|---|---|---|
| `Bc7Srgb` | RGBA | sRGB transfer function decoded | `VK_FORMAT_BC7_SRGB_BLOCK` | `MTLPixelFormatBC7_RGBAUnorm_sRGB` |
| `Bc7Unorm` | RGBA | as stored | `VK_FORMAT_BC7_UNORM_BLOCK` | `MTLPixelFormatBC7_RGBAUnorm` |
| `Bc5Unorm` | RG | as stored; `.ba` undefined | `VK_FORMAT_BC5_UNORM_BLOCK` | `MTLPixelFormatBC5_RGUnorm` |
| `Bc1Srgb` | RGBA, one-bit alpha | sRGB transfer function decoded | `VK_FORMAT_BC1_RGBA_SRGB_BLOCK` | `MTLPixelFormatBC1_RGBA_sRGB` |
| `Bc1Unorm` | RGBA, one-bit alpha | as stored | `VK_FORMAT_BC1_RGBA_UNORM_BLOCK` | `MTLPixelFormatBC1_RGBA` |
| `Bc2Srgb` | RGBA, explicit alpha | sRGB transfer function decoded | `VK_FORMAT_BC2_SRGB_BLOCK` | `MTLPixelFormatBC2_RGBA_sRGB` |
| `Bc2Unorm` | RGBA, explicit alpha | as stored | `VK_FORMAT_BC2_UNORM_BLOCK` | `MTLPixelFormatBC2_RGBA` |
| `Bc3Srgb` | RGBA, interpolated alpha | sRGB transfer function decoded | `VK_FORMAT_BC3_SRGB_BLOCK` | `MTLPixelFormatBC3_RGBA_sRGB` |
| `Bc3Unorm` | RGBA, interpolated alpha | as stored | `VK_FORMAT_BC3_UNORM_BLOCK` | `MTLPixelFormatBC3_RGBA` |
| `Bc6hUfloat` | RGB unsigned half float | linear, unclamped (0 to 65504); alpha 1 | `VK_FORMAT_BC6H_UFLOAT_BLOCK` | `MTLPixelFormatBC6H_RGBUfloat` |

A level `w`×`h` texels in extent carries `ceil(w / 4) × ceil(h / 4)` blocks, row-major and tightly
packed, so a level narrower or shorter than a block (the 2×2 and 1×1 tail of every chain) is one
block. Dimensions must be nonzero and need not be multiples of four. `MipChain::Supplied`
requires the complete chain from the base level to 1×1, halving each axis and flooring at one,
the same rule supplied RGBA8 chains follow. A level whose byte count is not its block count times
the block size is `InvalidRequest`, naming the level and both numbers. Input byte ranges and summed
staging capacity are checked before native upload.

## Capability

BC sampling is optional hardware. On Vulkan the adapter's `textureCompressionBC` feature is read
during selection and enabled on the logical device only where the adapter reported it; an adapter
without it is still selected, and only the compressed uploads themselves return `Unsupported`.
The existing format and image-format-properties checks then confirm optimal-tiling sampled,
linear-filter and transfer-destination support and the extent and level-count limits. On Metal
the device's `supportsBCTextureCompression` answers, which is true for every Mac Mulciber targets
and false on the iOS-class devices it does not. The application decides what to do without BC;
Mulciber does not fall back to decoding on the CPU, because a fallback that silently uploads four
times the memory is the failure the compressed path exists to avoid.

## Native ownership and synchronization

Vulkan packs the levels into one staging buffer and copies them with one
`vkCmdCopyBufferToImage2` carrying a region per level, as for RGBA8; a compressed region's
`imageExtent` is the level's texel extent and its buffer layout is tightly packed blocks. Metal
creates the texture with the native BC pixel format and replaces every level's region with
`bytesPerRow` equal to the level's block-row size. Layout transitions, the upload fence, and
destruction ordering are the RGBA8 paths, unchanged.

## Validation boundary

Workspace checks and unit tests pass, covering level sizing in blocks for every extent including
partial and sub-block levels, and the mip validation that measures a compressed chain in blocks
rather than texels. Native execution has not been run on either backend for this release: the
consuming game is the first user, and its startup is where a BC7 upload first meets a driver. No
viability gate is advanced.

BC1, BC2 and BC3 (added in 0.13.32): workspace checks and unit tests cover BC1's eight-byte
block sizing at full, partial and tail extents and its mip validation. On 2026-10-08, Linux / KDE
Wayland / NVIDIA RTX 3060 Ti, Vulkan with `vulkan-validation` enabled and no validation messages,
a The Ship map viewer uploaded 151 textures from the game's own DXT1 and DXT5 data as `Bc1Srgb` and
`Bc3Srgb` mip chains (full chains through `create_block_compressed_texture_with_mips`) and sampled
them in a material pipeline; the rendered frame was inspected and matched the same data decoded
through another renderer. BC2 was not exercised (that game ships no DXT3), the UNORM variants were
not exercised, and Metal was not run.

KTX 2.0 (unreleased): unit tests parse files written byte by byte from the specification, every
`BlockCompression` encoding's `VkFormat`, a base-level-only file and a complete non-square chain,
and refuse truncated, short-level, partial-chain, foreign-format, cube, array, 3D and
supercompressed files with the expected kind. `mulciber-texture`'s tests round-trip its writer
through the parser. On 2026-10-09, Linux / KDE Wayland / NVIDIA RTX 3060 Ti, Vulkan with
`vulkan-validation` enabled and no validation messages, Shiplike loaded 17 `mulciber-texture` bakes
(`Bc7Srgb` albedo and `Bc7Unorm` normal + roughness and metallic + occlusion, full chains, 128×128,
384×512 and 768×768) through `create_ktx2_texture` and sampled them in its world material pipeline;
the frame matched the same textures uploaded from their PNG sources as RGBA8 with GPU mips (mean
absolute difference 0.12 of 255 per channel). Partial-chain files, the other block formats through
KTX 2.0, and Metal were not exercised natively.

BC6H (graphics 0.18.0, texture 0.6.0): unit tests size BC6H levels (an eighth of `RGBA16Float`'s
bytes), accept a BC6H cube array with a supplied chain and refuse it with one level, and round-trip
VkFormat 143 through KTX 2.0. `mulciber-texture`'s tests encode a sky gradient with a sun in it and
decode it with an independent decoder (`bcdec_rs`): the sky away from the sun's blocks within 5%
relative error, the sun still above 15000; odd extents padded to whole blocks; a chain baked to KTX
2.0 and read back with its 1×1 level within 2% of the base's mean. On 2026-10-15
`mulciber-cube-array-texture` sampled 37 BC6H cases (two cubes, three levels, every face and level,
and a `textureQueryLevels`/`textureNumLayers` case) through `texture_cube_array<f32>` on Vulkan with
no validation messages, each within two half ULPs of `bcdec_rs`'s decode of the same blocks; that run
was Mesa's software rasteriser lavapipe (see [Linux validation](linux-validation.md#cube-texture-arrays-graphics-0160-0180)),
not a GPU. No hardware GPU, 2D BC6H upload, or Metal run yet.
