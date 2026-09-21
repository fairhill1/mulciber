# Sampled RGBA16Float uploads (0.13.13)

`Device::create_rgba16_float_texture` uploads one linear sampled 2D image and returns the existing
owning `Texture`. `create_rgba16_float_texture_with_mips` uploads a complete application-authored
chain. Both use the existing material texture/sampler bindings and WGSL `texture_2d<f32>` in vertex
and fragment stages. RGBA8 sRGB/UNORM APIs keep their interpretation and sampling behavior.

## Input and conversion contract

```rust
let coefficients: mulciber::Texture =
    device.create_rgba16_float_texture(atlas_width, atlas_height, &decoded_rgba)?;
// decoded_rgba: Vec<[f32; 4]>, row-major RGBA coefficients and application-defined alpha.

// Alternatively, complete levels with dimensions halved independently, flooring at one:
let coefficients = device.create_rgba16_float_texture_with_mips(
    atlas_width, atlas_height, &level_slices,
)?;
// level_slices: Vec<&[[f32; 4]]>, base through the final 1x1 level.
```

Every component must be finite and in `-65504..=65504`. NaN, positive/negative infinity, and any
magnitude above 65504 are `InvalidRequest`, including values that would round back into range.
Conversion uses IEEE binary16 round-to-nearest, ties-to-even. Signed zero is retained; representable
half subnormals are uploaded; smaller magnitudes round to a subnormal or signed zero (the exact
`2^-25` midpoint rounds to zero). There is no 0–1 clamp, sRGB transfer function, or other color
transformation. GPU filtering/arithmetic can flush subnormals on some hardware; preservation in
uploaded storage does not promise identical arithmetic behavior on every GPU.

Dimensions must be nonzero. Each level has exactly `width * height` RGBA texels at its own extent.
The base-only method creates one mip level; explicit LODs clamp to level zero. The mip method
requires the complete chain, including for non-square/odd dimensions; 1x1 requires one level.
Mulciber never generates, filters, or color-transforms supplied mip content. Input byte ranges,
eight-byte native texel sizes, rows, and summed staging capacity are checked before native upload.
Allocation failure returns `OutOfMemory`; unsupported format/extent support returns `Unsupported`.

Declare `MaterialBinding::Texture { binding: 1 }` and
`MaterialBinding::Sampler { binding: 2, filter: SamplerFilter::Linear,
address: SamplerAddress::ClampToEdge }`, then supply `textures: &[&coefficients]` in the material
record. Linear material samplers filter both within and between mip levels. Sample with:

```wgsl
@group(0) @binding(1) var coefficients: texture_2d<f32>;
@group(0) @binding(2) var coefficient_sampler: sampler;
// Legal in both vertex and fragment stages:
let transfer = textureSampleLevel(coefficients, coefficient_sampler, uv, explicit_lod);
```

The legacy textured-cube interface keeps its existing level-zero sampler. Arbitrary explicit mip
selection/interpolation uses material samplers. The format is independent of the output attachment:
float inputs work with ordinary or HDR materials. Sampled 3D resources, mutable textures, implicit
mips, and lighting policy are outside this addition.

## Native ownership and synchronization

Metal uses native `MTLPixelFormatRGBA16Float`, shared CPU-writable storage, shader-read usage, and
`replaceRegion` for every level with `width * 8` bytes per row. Metal 3's
[format table](https://developer.apple.com/metal/capabilities/) guarantees filtering; the upload
checks that family and conservatively accepts extents through 16384. Writes finish before the handle
becomes bindable. Ordinary retained command buffers preserve submitted references when an owner is
dropped. Partial sampler/arena construction releases the native texture and sampler.

Vulkan uses optimal `VK_FORMAT_R16G16B16A16_SFLOAT` with sampled and transfer-destination usage,
one sample, and a view covering every supplied mip. Format properties must support sampled reads,
linear filtering, and transfer destination; image-format properties must admit the extent, mip
count, and one-sample usage. Tightly packed eight-byte texels keep every mip offset aligned to its
texel block. The upload command transitions all levels from undefined to transfer destination,
copies every region, then transitions to shader-read-only with transfer writes visible to **both
vertex and fragment** sampled reads. This also corrects the previous fragment-only upload barrier
for existing RGBA8 vertex consumers. The synchronous upload fence completes before staging is
released; submitted frame use follows existing waited resource retirement. Failed arena insertion
releases the image/view/memory and sampler. All needed format/copy symbols were already generated;
no hand-written Vulkan declarations or binding regeneration were needed.

## Reproducible numerical validation

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-float-texture  # macOS
cargo run -p mulciber-float-texture                  # Windows/Linux; Vulkan validation enabled
```

The standalone probe uploads a 2x2 base and a distinct 1x1 authored mip through the public API. Its
40 cases cover both texture constructors, both shader stages, all four texel centers, horizontal
and bilinear interpolation, exact mip one, fractional LODs 0.25/0.5, and out-of-range LOD clamping.
Known inputs include zero, negatives, values up to 16, and signed coefficients around `1e-5` to
`1e-3`. It renders the sampled result through `MaterialBinding::Texture` to an HDR target and reads
back the native half bits, rather than asserting shader compilation or a presentation alone.
Small channels are multiplied by an exact power of two (1024) before output to avoid subnormal
render-target storage concealing a texture-sampling error.

The CPU oracle enumerates representable halves independently of the upload converter, quantizes
input texels, interpolates, and quantizes the expected output. The limit is two output half ULPs
with sign checking (either signed zero is accepted). This admits native filtering/attachment
rounding, but rejects zeroed small coefficients, UNORM clamping, wrong mip contents and byte sizing.
All shader sources and both native artifacts are checked in; their hashes and Vulkan 1.3/Naga 30 /
SPIRV-Tools 2026.2 pins are in `vulkan-toolchain.lock.toml`. Regenerate each with:

```sh
cargo run -p mulciber-shader -- build probes/float-texture/src/sample.wgsl --target metal --output probes/float-texture/artifacts/sample.metal.shaderbin
cargo run -p mulciber-shader -- build probes/float-texture/src/sample.wgsl --target vulkan --output probes/float-texture/artifacts/sample.vulkan.shaderbin
# Repeat for composite.wgsl / composite.<backend>.shaderbin, then update the lock hashes.
```

The `native-validation` Cargo feature enables a hidden repository-only one-pixel HDR readback hook
and transfer-source usage on Vulkan scene targets. It is not a supported application readback API;
default consumer builds omit it. Readback waits for prior work and copy completion, uses tracked
Metal blits or explicit Vulkan image/host barriers, then restores Vulkan shader-read layout.

Six CPU tests cover invalid dimensions/counts, row/image/staging overflow, odd/non-square complete
chains, missing/extra/mismatched levels, invalid components, known quantization and packing,
every finite half round-trip, and every positive/negative adjacent midpoint.

Metal numerical execution passed on Apple M2 / macOS 15.7.7 with API validation; observed error was
at most one half ULP. See the [macOS runbook](macos-validation.md#sampled-rgba16float-uploads-01313).
Windows cross-target checks are structural evidence. Native Vulkan numerical execution remains
pending on Windows and Linux; this Mac cannot establish that behavior. No physical lifecycle,
visual, performance, multi-display, other Apple GPU, AMD, Intel, or Nvidia coverage is claimed.

## Isle of Rán migration handoff

Use the exact base-level call above once the game has produced decoded RGBA coefficient texels.
Its current atlas header stores **f32 bit patterns as RGBA8 bytes**, decoded by `textureLoad` and
`bitcast`. Converting those bytes to numeric half components destroys the header. Decode coefficient
planes separately; relocate/re-encode origin, spacing, dimensions, portal normals, and per-plane
scale metadata into an appropriate application-owned uniform/storage/header representation. Keep
alpha/air validity and its filtering/normalization semantics intact. If header rows are removed,
adjust atlas offsets and UVs as part of that migration.

The current RGB integer is `(high_byte * 256 + low_byte) / 65535`, multiplied by its plane scale.
Decode that value on the CPU (or from original bake floats) before uploading; do not upload either
byte plane as float coefficients. A single native sample can then replace the two coefficient
samples and their reconstruction. No game files were changed for this work.

Compare the existing bake's errors before dropping its plane scales. Scaled UNORM16 has a fixed
absolute step `plane_scale / 65535`; normal float16 has exponent-dependent spacing, approximately
constant relative precision, and a subnormal absolute step of `2^-24`. Retaining scales with float16
may avoid underflow but does not reproduce UNORM16's precision. Measure absolute/relative error,
near-zero coefficients, filtered results and final lighting against the original bake before
choosing scaled or unscaled float storage. Mulciber contains none of the game's packing policy.

Release version: **0.13.13**. The native Vulkan validation boundary above also applies to this release.

## Queue-ordered replacement

`Device::update_rgba16_float_texture(&texture, width, height, &texels)` replaces a
single-level float texture without changing its handle or material bindings. It
uses the same checked binary16 conversion as creation. Foreign/stale handles,
changed dimensions, other formats, and mip chains are rejected before replacing
any pending write. The caller may release its input immediately.

Writes are consumed before draws in the next textured/material submission;
multiple pending writes coalesce to the last one. Earlier submitted frames see
the previous contents. Dropping the texture cancels unsubmitted writes. No
queue/device-idle operation is introduced. Vulkan uses one reusable host staging
buffer per acquired frame slot, guarded by that slot's fence, and barriers from
vertex/fragment sampling to transfer and back. Staging storage follows the
texture's existing GPU retirement. Metal encodes an ordered buffer-to-texture
blit with 256-byte row alignment; the command buffer retains staging through
completion. Resizing and mip replacement are deliberately outside this API.

The float-texture probe now alternates replacements on an existing texture,
submits nine frames between readbacks to exercise staging reuse,
checks last-write-wins, and verifies that rejected dimension/mip writes preserve
the valid contents. On 2026-09-21, Linux/Vulkan (RTX 3060 Ti) passed all 40
vertex/fragment sampling cases with validation enabled, within two half ULPs.
Metal compiled and passed Clippy for `aarch64-apple-darwin`; physical Metal
replacement/lifetime validation remains outstanding.
