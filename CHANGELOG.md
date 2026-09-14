# Changelog

Release notes moved from the README. This is a partial history of changes.

## Optional Vulkan validation (0.13.16)

Published consumers no longer unconditionally require `VK_LAYER_KHRONOS_validation` or
`VK_EXT_debug_utils`. Default builds request no validation layer, install no debug messenger,
and do not resolve its extension functions. This fixes release startup on machines without
the Vulkan SDK. The opt-in `vulkan-validation` feature preserves strict layer requirements
and failure on warning/error callbacks. Repository examples and probes opt in explicitly;
`native-validation` enables it too. Metal behavior is unchanged.

Windowless native instance tests cover SDK-free creation/destruction, explicit validation
with a messenger, and rejection when requested validation is unavailable.

## Block-compressed sampled uploads (0.13.15)

`Device::create_block_compressed_texture` and its `_with_mips` peer upload already encoded BC7
(sRGB or UNORM) and BC5 blocks through the existing `Texture` and material bindings, sampled
directly by the GPU at a quarter of the RGBA8 footprint. Mulciber never encodes or decodes; the
application encodes each level of its own filtered chain. Vulkan gates on `textureCompressionBC`
and Metal on `supportsBCTextureCompression`, returning `Unsupported` rather than decoding on the
CPU. Isle of Rán uploads its BC7 material textures through this path. See the
[contract](docs/block-compressed-textures.md).

The same release moves `mulciber-shader` to 0.5.2 on naga 30.0.1, whose source is identical to
30.0.0 (only its manifest changed); the cube Vulkan artifact regenerated under 30.0.1 is
byte-identical to the checked-in one, so every artifact hash in `vulkan-toolchain.lock.toml`
stands and only its recorded compiler string moves.

## Growable Vulkan descriptor pools (0.13.14)

A Vulkan pipeline's cached descriptor sets are allocated from as many pools as the scene turns
out to need. Previously each pipeline owned one 64-set pool, so a scene whose distinct sampled
textures through one pipeline outgrew it failed with `VK_ERROR_OUT_OF_POOL_MEMORY` while Metal,
which has no such pool, ran on. Growth is transparent; reset and destruction release every pool.
See the [backend contract note](docs/backend-contracts.md#growable-vulkan-descriptor-pools-01314).

## Sampled floating-point textures (0.13.13)

Native RGBA16Float uploads accept linear f32 RGBA texels, with optional complete authored mip chains.
Existing material bindings support linear filtering and explicit LOD sampling in vertex and fragment
stages. The 40-case numerical readback probe passed on Metal/Apple M2; native Vulkan execution remains
pending. See the [input contract, validation, and consumer migration](docs/float-texture-uploads.md).

## Vulkan recording and opt-in frame-start pacing (0.13.12 / runtime 0.5.3)

Vulkan material/shadow recording avoids redundant pipeline binds and single-draw indirect
commands on Windows and Linux. Optional native refresh feedback supports the runtime's new
opt-in FrameStartLimiter, which waits before input polling; it leaves fixed-step timing
and interpolation unchanged. Isle of Ran enables limiting only on Windows, retaining Linux
FIFO behavior. Windows measurements and playtesting support the combined improvement;
native Linux/macOS performance and AMD coverage remain unmeasured. See
[validation and measurements](docs/windows-validation.md#vulkan-recording-and-opt-in-frame-start-pacing-01312--runtime-053).

## Windows mailbox presentation (0.13.11)

Windows Vulkan prefers supported mailbox presentation with FIFO fallback;
Linux FIFO and Metal behavior are unchanged. Optional presentation timing is
bounded to native queue capacity, and unavailable timestamps remain untimed.
The user confirmed the Windows input delay is gone. See
[Windows validation](docs/windows-validation.md#windows-mailbox-presentation-01311)
for measured results and hardware coverage.

## Metal HDR fixes (0.13.10)

First run of the HDR, scene-depth and volumetric passes on Apple silicon found two Metal-only
faults. The MSAA scene color carried a resolve texture on intermediate encoders whose store
action was a plain store, which Metal rejects; only the last writer resolves now. Render target
creation released an autoreleased texture descriptor, so any target created inside a frame left
a freed object in the frame pool and every error exit segfaulted instead of returning its
message. Isle of Rán renders on Metal with both fixes.

## GPU-local Vulkan meshes (0.13.9)

Immutable meshes now prefer device-local storage with frame-owned staged uploads, bounded retained
staging capacity and an observable host-memory fallback. GPU timing feedback remains ordered across
frame-slot abandonment. See the [mesh-memory policy and evidence](docs/vulkan-mesh-memory.md).
