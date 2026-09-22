# Changelog

Release notes moved from the README. This is a partial history of changes.

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
