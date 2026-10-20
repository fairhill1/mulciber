# Mulciber

`mulciber` is the experimental graphics and presentation layer of the Mulciber native
game-development stack: one narrow public API implemented directly against Metal on macOS and
Vulkan on Windows and Linux, selected at compile time with no portability layer in between.
The project is validating native resource, rendering, presentation, and lifecycle
implementations before it commits to a stable public graphics API.

The current slice owns device and surface lifecycle with tracked presentation retirement, and
renders one fixed frame shape: an optional depth-only shadow pre-pass (a single map or a
cascaded layered array), a multisampled scene pass of ordered draws, and an optional fullscreen
postprocess pass whose offscreen targets accept a render scale. Applications author their own
materials — WGSL modules compiled offline by `mulciber-shader`, with declared vertex layouts
and binding slots validated against the interface recorded in the artifact — and supply
per-record uniform, read-only storage, and frame-transient geometry as plain bytes: the
application owns the layouts, the engine sees bytes. Fullscreen postprocess pipelines likewise
declare an optional group-0/binding-0 uniform of at most 256 bytes and borrow exactly that many
bytes per submission, independently from scene/material uniform storage. Policy that engines
commonly absorb
(cascade fitting and selection, depth bias, mip content, draw ordering) deliberately stays in
application code. `Device::create_texture(&TextureDescriptor { shape, mips, data })` uploads
every sampled texture: `TextureShape` selects 2D, cube or cube array; `MipChain` selects one
level, a complete application-authored chain, or a chain the GPU generates from level 0 (2D
only); `TexelData` carries sRGB or linear UNORM RGBA8 (normal maps), block-compressed BC
blocks, or `RGBA16Float` from `f32` or directly from binary16 bit patterns, one slice per image.
`Device::update_texture(&texture, TexelData::Rgba16Float(..))` queues a same-sized replacement
of a 2D `RGBA16Float` texture — its single level, its complete chain, or, for a texture created
with `MipChain::Generated`, level 0 alone with the rest regenerated — before the next scene
submission without recreating bindings or waiting for device idle. Input is copied immediately;
pending writes coalesce and submitted frames retain their prior contents.
Immutable meshes may keep one vertex region with multiple mixed-width indexed parts; material and
shadow records borrow a selected part without creating another resource lease or allocation, while
the existing mesh APIs remain the one-part/default-part path.

On Vulkan a frame is recorded while earlier frames are still executing: command buffers, fences,
semaphores, GPU timestamp blocks, and every host-visible per-frame region belong to one of three
frame slots, so building a frame does not wait on the previous frame's GPU time.
Metal also uses three slots, matched to its drawable pool. Each owns a retained command buffer
and separate CPU-written uniform, storage, transient-geometry, transform, and record-instance
buffers. Acquisition waits only before reusing a slot; unavailable and abandoned acquisitions do
not rotate the ring. Completed GPU timings remain correlated and ordered by submission. The depth
is not an application-visible latency policy.

Optional GPU diagnostics correlate completed durations with presentation frame indices. Metal
reports whole-command-buffer time and, on devices supporting stage-boundary timestamp counters,
shadow, scene, and postprocess spans with separate vertex/fragment intervals. Vulkan reports
the fixed regions when its graphics queue supports timestamps. Stage intervals can overlap and
region spans can include pipeline gaps; neither is additive GPU utilization. Lazy resource drops are reclaimed
in bounded batches at frame boundaries. Vulkan drains every frame in flight before reclamation;
Metal releases its owner references while retained command buffers preserve submitted resource
uses. Explicit destruction is fallible; shutdown waits and checks every outstanding submission.

The API is experimental and may change without compatibility guarantees. Design contracts,
decision records, runnable examples, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).

## Frame capture

`Surface::request_frame_capture()` before acquiring a frame makes `Queue::render_and_present`
read back its final color, blocking until the GPU finishes that frame;
`Surface::take_frame_capture()` then returns a `FrameCapture` with tightly packed, top-down RGBA8
pixels in the sRGB encoding the display receives. It is a screenshot path: frames without a
request pay nothing. Metal's path has not run yet. See the
[contract](https://github.com/fairhill1/mulciber/blob/main/docs/frame-capture.md).

## Vulkan validation

Validation is off by default, including in debug builds. Ordinary applications need a Vulkan
driver, not the Vulkan SDK. Enable `features = ["vulkan-validation"]` on the `mulciber`
dependency for development: this requires `VK_LAYER_KHRONOS_validation` and `VK_EXT_debug_utils`
and preserves failure on every validation warning/error. A missing requested layer is an error,
not a silent fallback. The repository-only `native-validation` feature also enables it.

Cargo features are additive: do not enable this feature anywhere in a shipped application's
dependency graph. Repository examples and probes deliberately enable it for their validation runs.

## Depth-isolated first-person geometry

`SceneOutput::Postprocessed { foreground_start: Some(split), .. }` draws the world
records before the split, preserves color, clears depth, and draws the foreground
records before postprocessing and the HUD overlay. Both groups must be non-empty
and use the same depth comparison direction. Only postprocessed output carries a
split; it retains material lighting, shadow sampling, scene resolution and MSAA.

Material and shadow records expose their per-record instance upload cap through
`mulciber::INSTANCE_SUPPLY_SIZE_LIMIT`. Split larger supplies on whole-instance
boundaries and submit multiple records using the same mesh and pipeline.

## HDR scenes and bloom

HDR is a setting, not a separate constructor. A material pipeline whose descriptor has
`scene_color: SceneColor::Hdr` renders linear radiance into the RGBA16Float bundle created by
`Device::create_postprocess_targets(info, scale, SceneColor::Hdr)`.
`Device::create_postprocess_pipeline` with `hdr: Some(HdrComposite { bloom, volume })` accepts
application-authored composite shaders plus optional `BloomShaders` (prefilter, downsample and
optional upsample) and `VolumetricShaders`. Six bloom levels feed the final sRGB composite;
foreground geometry participates in HDR and bloom, while HUD overlays remain sharp.
`SceneColor::Surface` and `hdr: None` keep the surface-format path. HDR scene storage
is independent of HDR monitor output.

Vulkan validates HDR format roles, sample counts and extent limits. Both native
backends own bloom resources and their dependencies. Isle of Rán renders its HDR
scene with bloom on Vulkan and Metal. See the [HDR contract](https://github.com/fairhill1/mulciber/blob/main/docs/hdr-bloom-contract.md).

## Material scene depth

`MaterialBinding::SceneDepth` gives HDR materials an immutable snapshot of preceding world
depth at the selected 1x/4x sample count. The engine captures it before the first consuming
record and preserves normal depth testing, volumetric lighting and foreground ordering.
Scene-depth consumers must use the matching WGSL depth texture kind and cannot write depth.
See the [scene-depth contract](https://github.com/fairhill1/mulciber/blob/main/docs/scene-depth-contract.md).

Vulkan material/shadow command recording avoids redundant pipeline binds and uses direct
indexed draws for immutable single-instance parts. Native presentation feedback additionally
reports an optional display refresh duration for application-controlled frame-start pacing.
The runtime limiter is opt-in; graphics presentation mode selection is unchanged.

## Sampled textures

`Device::create_texture` takes three kinds of `TexelData`, all sampled through the same material
bindings:

- **RGBA8**, sRGB or linear (normal maps), with any `MipChain`.
- **`RGBA16Float`**, from `f32` or directly from binary16 bit patterns, for WGSL
  `texture_2d<f32>` in vertex or fragment shaders. Conversion from `f32` rounds to binary16, ties
  to even, without colour transforms or a 0–1 clamp; non-finite values and magnitudes above 65504
  are rejected. Signed zero and half subnormals are preserved in storage, while GPU subnormal
  arithmetic can differ. Linear samplers interpolate texels and mips. Metal numerical readback
  passed on Apple M2; native Vulkan numerical validation remains pending. See the
  [contract](https://github.com/fairhill1/mulciber/blob/main/docs/float-texture-uploads.md).
- **Block-compressed** BC1, BC2, BC3, BC5, BC6H or BC7, as
  `TexelData::BlockCompressed(BlockCompression::Bc7Srgb, &level_slices)` or
  `create_texture(&ktx.descriptor())` for a KTX 2.0 file. These are sampled directly at a quarter
  of the RGBA8 footprint (BC6H, unsigned half-float light, at an eighth of `RGBA16Float`'s). The
  application encodes every level of its own filtered chain; Mulciber never encodes or decodes,
  so block-compressed data cannot ask for `MipChain::Generated` (`InvalidRequest`), and a chain
  at an extent Apple GPUs misread a level of (`misread_block_mip`) is refused on every backend
  (`InvalidRequest`). Adapters
  without BC sampling return `Unsupported` for these uploads and are otherwise unaffected. See the
  [contract](https://github.com/fairhill1/mulciber/blob/main/docs/block-compressed-textures.md).

On Vulkan, the descriptor sets cached per pipeline are allocated from as many pools as needed, so
the number of distinct textures a scene binds through one pipeline has no fixed limit.
