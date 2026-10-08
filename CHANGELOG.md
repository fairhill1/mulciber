# Changelog

Release notes moved from the README. This is a partial history of changes.

## Unreleased: mipped float texture replacement

`Device::update_rgba16_float_texture_with_mips(&texture, width, height, &levels)` replaces every
level of a texture made by `create_rgba16_float_texture_with_mips`, with the same queuing as
`update_rgba16_float_texture`: input is converted and copied now, the whole chain is uploaded before
draws of the next textured/material submission, pending writes coalesce, earlier frames keep their
contents and nothing waits for device idle. The chain must be complete and match the texture's
dimensions and mip count. Both calls share one path, a single level being a chain of one. Vulkan
sizes each frame slot's staging to the whole chain and copies a region per level, with barriers
over every level; Metal blits each level from its own 256-byte-aligned rows. The float-texture
probe now replaces its mipped texture's full chain every frame; it passed all 40 cases on
Linux/NVIDIA under Vulkan validation and synchronization validation. Metal passes Clippy but has
never run. See [mip chain replacement](docs/float-texture-uploads.md#mip-chain-replacement).

## Unreleased: Rust 1.99 and current dependencies

The pinned toolchain moves from 1.98.1 to 1.99.0 (the MSRV stays 1.97). Examples, probes and
comparisons move to glam 0.34.1, the Vulkan triangle probe to naga 30.0.1, the wgpu comparisons to
wgpu 30.0.1 and pollster 1.0.1, the Metal comparison to bytemuck 1.25.2, and the binding generator
to bindgen 0.73.2. Three tests use `assert_eq!` against an empty array for clippy 1.99's
`assert_is_empty`.

## Unreleased: modifier keys as physical keys

`KeyCode` gains `ShiftLeft`/`ShiftRight`, `ControlLeft`/`ControlRight`, `AltLeft`/`AltRight`,
`SuperLeft`/`SuperRight` and `CapsLock`, and every backend now reports those keys' own press and
release as `InputEvent::Keyboard` in addition to `ModifiersChanged`, so games can bind them (Source's
duck is Ctrl). They do not auto-repeat. AppKit decodes them from `flagsChanged` and its
device-dependent bits; Win32 maps their scan codes; Wayland and X11 share the evdev table.

## Unreleased: unaccelerated Wayland pointer deltas

While the pointer is captured, Wayland's `PointerDelta` now carries the unaccelerated motion from
`zwp_relative_pointer_v1` instead of the accelerated pair, matching the raw input Win32 already
reports, so turning speed no longer depends on how fast the mouse moves. X11 (warp deltas) and
AppKit (`NSEvent` deltas) still report accelerated motion.

## Unreleased: frame capture

`Surface::request_frame_capture()` asks for the next acquired frame to be read back when it is
presented, and `Surface::take_frame_capture()` returns it as a `FrameCapture`: the frame's
presented index, width, height and tightly packed, top-down RGBA8 pixels in the sRGB encoding the
display receives, whatever the swapchain's channel order, with alpha 255 on an opaque-composited
surface. Every `Queue` presenting verb captures, so direct, postprocessed, HDR, bloom, volumetric
and overlaid frames all read back their final color; the presenting call blocks until the GPU
finishes that frame, and frames without a request pay nothing. Abandoned or failed frames leave the
request pending. Vulkan swapchain images now carry `VK_IMAGE_USAGE_TRANSFER_SRC_BIT` wherever the
surface allows it, and a surface that does not answers the request with `Unsupported`; the copy
runs in the frame's own command buffer before the presentation transition. Metal turns
`framebufferOnly` off only while a capture is pending and blits the drawable before presenting.
The new `mulciber-frame-capture` probe matched direct (4x resolved and 1x), HDR-composited and
overlaid captures pixel by pixel on Linux/NVIDIA under Vulkan validation and synchronization
validation, on Wayland (`R8G8B8A8_SRGB`) and XWayland (`B8G8R8A8_SRGB`); Metal is implemented but
has never run. See [frame capture](docs/frame-capture.md).

## Unreleased: per-entry-point resource bindings

`mulciber-shader` now records the bindings each entry point uses, directly or through called
functions, in a `MULSHDR3` container, and material and shadow pipelines validate their declaration
against the union of their own vertex and fragment entry points' bindings instead of the whole
module. One WGSL module can hold a plain and a skinned vertex stage sharing a fragment stage, with
the plain pipeline declaring no storage slot and the skinned one declaring its bone palette. A
declared slot the module records but the pair never uses is refused by name. `MULSHDR2` artifacts
stay readable with every binding attributed to every entry point, so they validate exactly as
before; older `mulciber` releases reject `MULSHDR3` by header. Neither backend changed: Naga's MSL
already takes arguments only for an entry point's own globals, which a unit test now checks. The new
`mulciber-entry-bindings` probe drew both pipelines from one module on Linux/NVIDIA under Vulkan
validation; Metal has not run. See [per-entry-point bindings](docs/per-entry-point-bindings.md).

## Unreleased: Vulkan per-frame regions sized for every frame in flight

The Vulkan session created its record storage, transient geometry, instance transform and record
instance buffers with room for one frame's initial region, while every frame writes and binds its
region at its frame slot's base. Until a frame needed more than the initial capacity and the
buffer grew (growth already allocated a region per frame in flight), a frame in the second slot
wrote and bound past the end of the buffer: a skinned record with a palette of 256 bytes or less,
drawn in the first frames, failed the debug-build capacity assertion and in release wrote past the
mapped allocation. The initial buffers now hold a region per frame in flight, as the uniform buffer
always did. Found by the `mulciber-entry-bindings` probe's 64-byte bone palette; Metal keeps
separate per-frame buffers and was not affected.

## Unreleased: packed vertex formats

`VertexFormat` gains `Uint8x4`, `Unorm8x4`, `Uint16x2`, `Uint16x4`, `Unorm16x2` and `Unorm16x4`,
fetched narrow and read by the shader as `vec4<u32>`, `vec4<f32>`, `vec2<u32>` or `vec2<f32>`, so
skinned meshes can carry bone indices and weights in eight bytes. Pipeline creation matches the
WGSL type each format is read as against the artifact. Vertex layouts now require four-byte strides
and attribute offsets, which Metal always needed. Vulkan queries vertex-buffer support for the
16-bit formats and answers `Unsupported` without it. The new `mulciber-vertex-formats` probe read
back all six exactly on Linux/NVIDIA under Vulkan validation; Metal maps them but has not run. See
[packed vertex formats](docs/vertex-formats.md).

## Unreleased: cube textures

`Device` gains cube constructors beside every 2D sampled upload: RGBA8 sRGB and UNORM, any
`BlockCompression`, and `RGBA16Float`, each with a single-level and a complete-mip-chain form taking
six square faces in +X, -X, +Y, -Y, +Z, -Z order. They return the existing `Texture`, whose new
`dimension()` reports `TextureDimension::Cube`. Material pipelines sample one through
`MaterialBinding::CubeTexture` and WGSL `texture_cube<f32>`, which `mulciber-shader` records as its
own binding kind; pipeline creation refuses a declaration that disagrees with the artifact, and
submission refuses a 2D texture in a cube slot or a cube in a 2D slot. Vulkan creates a
cube-compatible six-layer image behind a cube view (regenerated bindings add
`VkImageCreateFlagBits`); Metal creates `MTLTextureTypeCube` and replaces each face and level by
slice. The new `mulciber-cube-texture` probe passed 72 face-order, orientation and per-face mip
readback cases in RGBA8 UNORM, RGBA8 sRGB with mips, BC1 with mips and `RGBA16Float` on
Linux/NVIDIA under Vulkan validation; Metal is implemented but has never run. See
[cube textures](docs/cube-textures.md).

## Unreleased: BC1, BC2 and BC3 uploads

`BlockCompression` gains `Bc1Srgb`/`Bc1Unorm`, `Bc2Srgb`/`Bc2Unorm` and `Bc3Srgb`/`Bc3Unorm`, the
DXT1, DXT3 and DXT5 encodings older game data ships in, so it can be uploaded as stored instead of
being re-encoded to BC7. BC1 blocks are eight bytes; block sizing and mip validation follow the
format. Vulkan BC1 and BC3 sRGB chains ran under validation on Linux/NVIDIA; BC2, the UNORM
variants and Metal are unexercised. See [block-compressed textures](docs/block-compressed-textures.md).

## Metal culls back faces as Vulkan does (0.13.31)

Vulkan's fixed, material and shadow pipelines have always culled back faces with counter-clockwise
front faces, but the Metal backend never set a cull mode, and Metal's default is to cull nothing.
On macOS every triangle was rasterized from both sides, so a model that supplied both windings of
a surface (a glTF double-sided material expanded by the application) drew a coincident reverse
face that fought the front one in the depth test, and closed meshes paid for binning and, under
alpha-tested materials, shading the half of their triangles facing away. Metal now sets the same
winding and cull mode on every encoder that records scene, material or shadow draws, including
the overlay records after postprocess; the postprocess, bloom and volumetric passes still cull
nothing. Verified by build, clippy and tests only; physical Metal visual validation remains open.

## Pacing onto a refresh the backend reports (graphics 0.13.30, runtime 0.5.6)

Window metrics on Win32, Wayland and X11 report display timing as unknown, so the runtime's frame
pacer never engaged there and every delta was the wall-clock gap between build starts: on an RTX
3060 Ti at 74.97 Hz under KDE Wayland, presents landed 13.34 ms apart while deltas ran from 6 to
24 ms, which is visible judder on a display that never missed a refresh.
`Surface::fixed_refresh_interval` reports the period when `VK_EXT_present_timing` says the
refresh is fixed (`refreshInterval` equal to `refreshDuration`; variable refresh and an
undetermined mode report `None`), and `Runtime::set_fixed_refresh_interval` hands it to the pacer,
which uses it only while platform timing is unknown. NVIDIA's Linux driver (615.71.09) reports the
duration but leaves the refresh mode undetermined, so `Runtime::set_nominal_refresh_interval`
also takes `Surface::refresh_interval` and trusts it once 30 consecutive presents have landed
within 5% of its grid, revoking it at the first that does not. `Surface::active_presentation_mode` now
resolves Adaptive on a Vulkan swapchain that switches between FIFO and immediate to the mode of
its latest present, as Metal already did, so an application can pace exactly while presents are
synchronized. See [frame-pacing controls](docs/frame-pacing-controls.md).

## Material uniforms up to 512 bytes (0.13.29)

`MATERIAL_UNIFORM_SIZE_LIMIT` rises from 256 to 512 bytes. The per-draw uniform stride in both
backends is now derived from the limit rather than written down beside it, so the two cannot
disagree; it stays a multiple of 256, the largest dynamic offset alignment Vulkan permits. A lit
material whose parameters had filled 256 bytes can now take another block of state without
moving it into a storage slot. The per-draw region costs twice as much per record. See the
[material contract](docs/material-contract.md).

## Material slots capped per native table (0.13.28)

A WGSL binding number is its native index, and Metal keeps textures, samplers and buffers in
separate tables, yet material pipelines capped every binding at slot 15, the sampler table's
size. A material with thirteen textures and two samplers failed as soon as a texture landed on
binding 16. Each kind now has its own ceiling: samplers stay at `MATERIAL_SLOT_LIMIT` (15),
textures reach `MATERIAL_TEXTURE_SLOT_LIMIT` (30) and uniform or storage buffers
`MATERIAL_BUFFER_SLOT_LIMIT` (28, clear of the two vertex buffer indices). A pipeline may declare
at most `MATERIAL_TEXTURE_COUNT_LIMIT` (16) textures, Vulkan's guaranteed sampled images per
stage. See the [material contract](docs/material-contract.md).

## Adaptive judges the workload, not the present interval (0.13.27)

Adaptive now chooses synchronized or immediate presentation from Strict's CPU and GPU workload
measurement: three consecutive overloaded frames release sync and 90 frames with 5% headroom
restore it. The 0.13.26 policy released on any single present interval over 1.15 periods, which
an application capped at the refresh rate hits with every hitch, so it alternated between the two
modes about once a second and tore in the immediate stretches. See
[frame-pacing controls](docs/frame-pacing-controls.md).

## Adaptive presentation without relaxed FIFO (0.13.26)

Where a Vulkan driver exposes no FIFO relaxed, as NVIDIA's Linux driver does on every surface,
Adaptive now switches one swapchain between FIFO and immediate per present with
`VK_KHR_swapchain_maintenance1`, driven by the throughput policy Metal already used, which moves
to a shared `backend/adaptive.rs`. Latest-ready becomes the last resort: it alternated one- and
two-refresh frames whenever the workload sat near the refresh period. See
[frame-pacing controls](docs/frame-pacing-controls.md) for measurements and limits.

## Vulkan acquisition and attachment synchronization (0.13.25)

Chain first-use swapchain layout transitions to the acquisition semaphore wait
in every draw path. Synchronize direct-render depth and MSAA attachment reuse
across overlapping frames. Synchronization validation reproduced the original
hazards on Windows / RTX 3060 Ti and no longer reports them in the patched game
menu or the subsequent map/dialogue test. This does not establish resolution of
the RTX 4070 playtest device loss.
See [evidence and limits](docs/swapchain-synchronization.md).

## Strict full-rate recovery (0.13.24)

Start Strict at full refresh and step down only after sustained overload. Recovery
requires five percent headroom instead of twenty percent, permitting 85-90 FPS
workloads to run at 75 Hz. Exclude Vulkan submission and image-availability waits
from the workload measurements used by Strict. See the pacing controls document
for regression coverage and physical-validation limits.

## Queue-ordered float texture updates (0.13.23)

Add `Device::update_rgba16_float_texture` for same-sized, single-level replacements
that retain texture handles and material bindings. Uploads are ordered before the
next scene submission without waiting for device idle, with staging reuse on
Vulkan and retained blit buffers on Metal. Earlier submitted frames retain their
old contents; pending updates coalesce to the last write. The API uses the same
checked binary16 conversion as texture creation.

Linux/Vulkan numerical validation covers repeated updates across in-flight frames.
Metal cross-compiles cleanly; physical Metal validation remains outstanding. See
[the replacement contract](docs/float-texture-uploads.md#queue-ordered-replacement).

## Adaptive presentation on drivers with no relaxed FIFO (0.13.22)

`PresentationMode::Adaptive` now has two native Vulkan spellings and takes whichever the
driver offers. FIFO relaxed is still preferred, so an adapter that already served the
policy is unchanged; where a driver exposes none, the device enables
`VK_KHR_present_mode_fifo_latest_ready` (or its original EXT name) with the
`presentModeFifoLatestReady` feature, and the policy selects that mode instead. NVIDIA's
Linux driver exposes no relaxed FIFO on Wayland, XCB/Xlib or `VK_KHR_display` surfaces, so
Adaptive was unavailable on every surface it offers.

The two are not interchangeable in what the player sees, which is why neither stands in for
the other's absence. Relaxed FIFO lets a late frame through immediately and tears;
latest-ready keeps every present on a vertical blank and discards the images that went
stale waiting for one. `Adaptive` now promises only that a queued image never costs
latency, and an application that cares which it got should ask for the active native mode.
Plain `Synchronized` never takes latest-ready: it promises every rendered frame reaches the
screen. Availability is gated on the device having enabled the feature rather than on the
surface listing the mode, which it does regardless. See
[contract](docs/frame-pacing-controls.md).

## Explicit VSync and frame caps (0.13.20 / runtime 0.5.4)

Add `Surface::set_vsync` with live Metal switching and deferred Vulkan swapchain
reconfiguration. Unsupported immediate presentation is an explicit error. Runtime
caps accept rates independent of display refresh, avoid catch-up bursts, and allow
cadence smoothing to be disabled without changing fixed-step simulation. No
automatic half-rate fallback is introduced. See [contract](docs/frame-pacing-controls.md).

## Live sample count, and Metal region timing measures what a pass adds (0.13.19)

`Device::set_sample_count` changes the samples per pixel that pipelines and targets are
built for after the session is open, with the same observable fallback to one sample as
opening. Every textured, instanced, material and postprocess pipeline and every render or
postprocess target now remembers the count it was built for, and submitting one built for
another count is refused with an error naming it, on both backends, instead of a native
sample-count mismatch. Nothing is rebuilt for the caller: shaders are not retained, so the
application destroys and recreates its own resources. `DeviceSelection::sample_count` keeps
the count chosen at open.


Fix the Metal shadow, scene and postprocess regions overstating light passes. A region was
the span from its earliest stage start to its latest stage end, and on Apple's tile-based
GPUs a pass's vertex stage starts under the previous pass's fragment stage and waits
there, so an almost empty shadow cascade reported the fragment time of the cascade before
it, and the regions of a frame overlapped instead of adding up. A region is now measured
from the previous pass finishing to this pass finishing, carrying the previous frame's
last tick into the first pass, which is what removing the pass would save. Replayed
against an Instruments capture of a 31 ms frame, the old spans summed to 119 ms and the
new regions to 31.7 ms. The vertex and fragment stage intervals are unchanged and documented: the
fragment interval is a pass's own work, the vertex interval includes the wait.

## Two-sample rendering (0.13.18)

`SampleCount::Two` joins `One` and `Four`. Each backend asks the device for the requested
count (`supportsTextureSampleCount:` on Metal, the framebuffer colour and depth sample-count
limits on Vulkan) and falls back observably to one sample per pixel, as the four-sample
request already did. `SampleCount::samples`, `from_samples` and `is_multisampled` replace
the assumption that multisampled means four. Shaders that read scene depth through a
multisampled texture are unchanged: the sample count is a property of the texture, not
the shader.

## Optional Vulkan device labels (0.13.17)

Fix startup without validation: 0.13.16 stopped enabling `VK_EXT_debug_utils` on the
instance but still required its device label functions. Load and call those functions
only with validation enabled. GPU region timestamps remain available in ordinary builds.
Windowless regression tests now create a real device, load its complete function table,
and submit/read timestamp queries through the renderer's region-recording methods in
both modes. The device test reproduced the 0.13.16 failure before this correction.

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
