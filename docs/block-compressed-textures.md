# Block-compressed sampled uploads (0.13.15)

`Device::create_block_compressed_texture(compression, width, height, &blocks)` uploads one
level of an already encoded BC7 or BC5 image and returns the existing owning `Texture`.
`create_block_compressed_texture_with_mips` uploads a complete application-encoded chain. Both
bind through the existing material texture/sampler bindings and WGSL `texture_2d<f32>` in either
stage, exactly as an RGBA8 upload does; nothing in a shader changes when a texture moves from RGBA8
to BC7.

## What Mulciber does and does not do

Mulciber uploads blocks. It never encodes, decodes, transcodes, or filters them: the application
runs an encoder over each level of a mip chain it has already filtered in RGBA8 and hands the
resulting blocks over, and the GPU samples the blocks directly. That is what makes the upload a
quarter of its RGBA8 equivalent in video memory as well as on disk, and it is why there is no
"generate mips" option: a block cannot be downsampled without decoding it first.

```rust
use mulciber::BlockCompression;

let albedo = device.create_block_compressed_texture_with_mips(
    BlockCompression::Bc7Srgb, width, height, &level_slices,
)?;
let normal = device.create_block_compressed_texture_with_mips(
    BlockCompression::Bc7Unorm, width, height, &normal_level_slices,
)?;
```

## Input contract

Every encoding stores a 4×4 texel block in sixteen bytes:

| `BlockCompression` | Channels | Sampled as | Vulkan | Metal |
|---|---|---|---|---|
| `Bc7Srgb` | RGBA | sRGB transfer function decoded | `VK_FORMAT_BC7_SRGB_BLOCK` | `MTLPixelFormatBC7_RGBAUnorm_sRGB` |
| `Bc7Unorm` | RGBA | as stored | `VK_FORMAT_BC7_UNORM_BLOCK` | `MTLPixelFormatBC7_RGBAUnorm` |
| `Bc5Unorm` | RG | as stored; `.ba` undefined | `VK_FORMAT_BC5_UNORM_BLOCK` | `MTLPixelFormatBC5_RGUnorm` |

A level `w`×`h` texels in extent carries `ceil(w / 4) × ceil(h / 4)` blocks, row-major and tightly
packed, so a level narrower or shorter than a block (the 2×2 and 1×1 tail of every chain) is one
sixteen-byte block. Dimensions must be nonzero and need not be multiples of four. The mip method
requires the complete chain from the base level to 1×1, halving each axis and flooring at one,
the same rule the RGBA8 mip methods apply. A level whose byte count is not its block count times
sixteen is `InvalidRequest`, naming the level and both numbers. Input byte ranges and summed
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
