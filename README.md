# Mulciber

[![crates.io](https://img.shields.io/crates/v/mulciber.svg)](https://crates.io/crates/mulciber)
[![docs.rs](https://docs.rs/mulciber/badge.svg)](https://docs.rs/mulciber)

Mulciber is a native game-development stack for Rust, built directly on Vulkan and Metal.
It combines graphics, native windows and input, and game-loop coordination for Windows,
Linux, and Apple-silicon macOS.

Mulciber is pre-1.0; its API still changes between minor releases. Isle of Rán, an open-world
RPG, runs on the published crates on all three platforms. Hardware coverage and remaining
validation gaps are tracked in the [roadmap](docs/roadmap.md) and platform runbooks.

## What it provides

- **Graphics** (`mulciber`): device, queue, and surface ownership; textures, meshes, instancing,
  and materials; shadow, offscreen render-texture, HDR, bloom, volumetric, and postprocessing
  passes; native presentation,
  presented-frame capture, and GPU timing feedback.
- **Windows and input** (`mulciber-platform`): native Win32, AppKit, Wayland, and X11
  implementations with window lifecycle, keyboard and pointer events, typed text from the native
  layout, cursor capture, and fullscreen.
- **Game loop** (`mulciber-runtime`): input snapshots, fixed-step simulation, bounded catch-up,
  render interpolation, suspension coordination, and optional frame-start pacing.
  [Presentation controls](docs/frame-pacing-controls.md) also support explicit caps independent of refresh.
- **Audio** (`mulciber-audio`): a mixer with buses, music fades, moving emitters, HRTF
  spatialization, voices muffled by what stands between them and the listener, and a room reverb
  sized by probing the game's world.
- **Entities** (`mulciber-ecs`): a small sparse-set entity component store with generational
  handles, so a stale handle misses instead of aliasing a reused slot. There are no systems or
  schedules; the game decides what runs when.
- **Networking** (`mulciber-net`): a dedicated server and its clients over UDP, with
  address-proving connects, reliable ordered and unreliable messages of any size, acks, round-trip
  time and loss, and a link conditioner for latency, jitter, loss and duplication.
- **Texture baking** (`mulciber-texture`): mip chains filtered for colour, cutouts, normals and
  data, packed into physically based materials, encoded as BC7 (light as BC6H) and written as KTX 2.0
  beside their sources, with the same chains built from the sources at run time when a bake is stale. `mulciber`
  uploads KTX 2.0 files directly.
- **Models** (`mulciber-model`): glTF 2.0 meshes and their metallic-roughness materials,
  node transforms baked in, one part per material, flat normals and MikkTSpace tangents where the
  file has none, and images left for the texture baker. Skins and animations: a skeleton, four
  joints and weights per vertex (rigid meshes ride the node above them), keyframes sampled into
  poses, poses cross-faded or blended by subtree, and bone palettes for skinned pipelines. Morph targets
  by name, shared by the meshes that name them, each kept for the vertices it moves.
- **Text and overlays** (`mulciber-text`): the game's fonts rasterised into one atlas at the sizes
  it asks for, text measured, wrapped and fitted, and overlay geometry for text, images and flat
  shapes, with a software rasteriser for testing screens without a GPU. The look stays the game's.
- **Offline shaders** (`mulciber-shader`): WGSL compiled into validated, cached native artifacts.
  Shaders `#import` shared WGSL modules, with `#ifdef` shader defs, including Mulciber's own
  `mulciber::` lighting library: photometric units, a Filament BRDF with a CPU-baked DFG table,
  and tone mapping. Designated WGSL functions can also generate callable Rust evaluators for
  simulation code. No shader compiler ships in the game process.

The graphics baseline is Vulkan 1.3 on Windows and Linux and Metal 3 on Apple silicon.
Vulkan 1.4 is requested when exposed by the loader; Metal 4 paths are SDK- and capability-gated.
Advanced GPU features are tracked as independent capabilities. See the
[support contract](docs/support-contract.md) for platform requirements.

Published `mulciber` builds do not require Vulkan SDK validation layers. Enable the
`vulkan-validation` Cargo feature during development to require Khronos validation and its
debug messenger. Repository examples and probes explicitly enable it; leave it off when
shipping a game. This feature is independent of debug/release optimization. Device debug
labels are optional, and GPU timing works without them.

## Examples

Run an example from this repository. Each uses one safe application source and selects native
Metal or Vulkan at compile time.

| Command | Workload | Details |
| --- | --- | --- |
| `cargo run -p mulciber-clear` | Clear-only surface lifecycle | [graphics contract](docs/api-graphics-contract.md) |
| `cargo run -p mulciber-capability-report` | Device-capability selection report, no rendering setup | supports `--force-one-sample` |
| `cargo run -p mulciber-cube` | A spinning textured cube: one WGSL shader compiled at build time, one material pipeline | [material contract](docs/material-contract.md) |
| `cargo run -p mulciber-input-cube` | Ordered native keyboard/pointer/scroll/focus input driving the cube | [input contract](docs/input-contract.md) |
| `cargo run -p mulciber-game-slice` | Playable Forge Run game on `mulciber-runtime`: instanced material records, a shared WGSL module, and a postprocess grade | [game contract](docs/game-slice.md) |

The examples are ordinary interactive programs; Mulciber prefers 4x MSAA and reports a fallback to
1x. The cube examples use `glam` locally for transform math; no Mulciber crate depends on it.

Every frame is a `Scene` of material records submitted through `Queue::render_and_present`; the
cube is the smallest whole program on that path. The probes under `probes/` exercise the rest of
the API, `mulciber-material-scene` among them (several vertex layouts, two-texture materials,
cascaded shadows, skinning, instancing and a frame-transient HUD overlay).

### Writing your own program

New programs follow the `examples/` pattern: copy an example package (path dependencies on the
Mulciber crates, `mulciber-shader` as a build dependency, `publish = false`, workspace lints), add
it to the root workspace `members`, and start from the example nearest your workload. A few
conventions to know:

- A field the simulation also needs — terrain displacement, say — is authored once in WGSL and
  generated for the host with `mulciber_shader::compile_host_field` from a `build.rs`, then pulled
  in with `include!` inside a module of its own. The host answer is an ordinary synchronous Rust
  call, available in the tick that asks for it.
- Shared WGSL goes in modules that name themselves with `#define_import_path` and are
  `#import`ed, rather than being copied between shader files. A `build.rs` registers them with
  `mulciber_shader::ShaderModules` and compiles each shader through it; see
  [WGSL modules and imports](crates/mulciber-shader/README.md#wgsl-modules-and-imports).
- Shaders are offline artifacts. No shader compiler ships in the game process and there is no
  runtime-WGSL path; each example's `build.rs` compiles its WGSL with
  [`mulciber-shader`](crates/mulciber-shader/README.md) into `OUT_DIR` for the target (SPIR-V
  checked by `spirv-val` for Vulkan, a Metal library built with Xcode's tools on macOS), and the
  program embeds the result. `MULCIBER_METAL_TYPECHECK_ONLY=1` writes empty artifacts so a macOS
  build can be type-checked on another host.
- Rendering suspends while a window is minimized or fully occluded: redraw delivery pauses and
  resumes with visibility, so a program counting presented frames stalls while hidden. See the
  [platform contract](docs/api-platform-contract.md).

## Frame pacing

`Surface::set_presentation_mode` chooses how frames reach the display:

| Policy | Behaviour |
| --- | --- |
| `Synchronized` (default) | Always wait for the vertical blank, even when a frame is late. |
| `Immediate` | Present as soon as possible, permitting tearing. |
| `Adaptive` | Synchronized while the game keeps up, never queueing stale frames; whether a late frame tears depends on the backend. |
| `HalfRefresh` | Tear-free at half the native refresh rate. |
| `Strict` | Synchronized, switching between full and half refresh from measured CPU and GPU work. |

Query `supports_presentation_mode` first: an unsupported policy returns an error rather than
silently becoming another. Metal implements every policy natively. Vulkan Adaptive uses relaxed
FIFO where the driver offers it, otherwise one swapchain switching between FIFO and immediate per
present (`VK_KHR_swapchain_maintenance1`), and latest-ready only as a last resort; Strict and
HalfRefresh need `VK_EXT_present_timing`.

`mulciber-runtime` smooths frame deltas onto the display's cadence only when the refresh period is
known to be fixed, from the window system (AppKit), from the presentation engine
(`VK_EXT_present_timing`), or from a nominal period that recent presents confirm. Variable or
unknown timing uses elapsed time. Explicit frame caps (`FrameStartLimiter`) are independent of
refresh and are never rounded to a divisor of it.

Strict pacing has been reported as still irregular on Windows (RTX 3060 Ti, 75 Hz), not every Vulkan
path has been exercised on hardware, and variable refresh and multiple
displays are unvalidated. See [frame-pacing controls](docs/frame-pacing-controls.md) for the
behaviour in detail, the measurements and their limits.

## Validation

Native probes exercise backend capabilities, rendering, and presentation lifecycle. API probes
cover finite runs, acquired-frame abandonment and recovery, and forced single-sample rendering.
For commands, prerequisites, measured results, and coverage limits, see the
[macOS](docs/macos-validation.md), [Windows](docs/windows-validation.md), and
[Linux](docs/linux-validation.md) runbooks.

## Documentation

- [Project vision](docs/vision.md) and [support contract](docs/support-contract.md)
- [Graphics](docs/api-graphics-contract.md), [platform](docs/api-platform-contract.md), and
  [runtime](docs/runtime-contract.md) contracts
- [Shader toolchain](crates/mulciber-shader/README.md) and [texture baker](crates/mulciber-texture/README.md)
- [Networking contract](docs/net-contract.md)
- [Materials](docs/material-contract.md), [HDR and bloom](docs/hdr-bloom-contract.md),
  [volumetrics](docs/volumetric-contract.md), and [scene depth](docs/scene-depth-contract.md)
- [Floating-point textures and queue-ordered updates](docs/float-texture-uploads.md),
  [block-compressed textures and KTX 2.0](docs/block-compressed-textures.md), and
  [cube textures and cube texture arrays](docs/cube-textures.md)
- [Frame pacing](docs/frame-pacing-controls.md) and
  [swapchain synchronization](docs/swapchain-synchronization.md)
- [Packed vertex formats](docs/vertex-formats.md),
  [per-entry-point resource bindings](docs/per-entry-point-bindings.md),
  [frame capture](docs/frame-capture.md), and [render textures](docs/render-textures.md)
- [Architecture](docs/architecture.md) and [backend contracts](docs/backend-contracts.md)
- [Roadmap](docs/roadmap.md), [viability gates](docs/viability-gates.md), and
  [API extraction plan](docs/api-extraction-plan.md)
- [Changelog](CHANGELOG.md)
