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
  and materials; shadow, HDR, bloom, volumetric, and postprocessing passes; native presentation
  and GPU timing feedback.
- **Windows and input** (`mulciber-platform`): native Win32, AppKit, Wayland, and X11
  implementations with window lifecycle, keyboard and pointer events, cursor capture, and fullscreen.
- **Game loop** (`mulciber-runtime`): input snapshots, fixed-step simulation, bounded catch-up,
  render interpolation, suspension coordination, and optional frame-start pacing.
  [Presentation controls](docs/frame-pacing-controls.md) also support explicit caps independent of refresh.
- **Offline shaders** (`mulciber-shader`): WGSL compiled into validated, cached native artifacts.
  Designated WGSL functions can also generate callable Rust evaluators for simulation code.
  No shader compiler ships in the game process.

The graphics baseline is Vulkan 1.3 on Windows and Linux and Metal 3 on Apple silicon.
Vulkan 1.4 is requested when exposed by the loader; Metal 4 paths are SDK- and capability-gated.
Advanced GPU features are tracked as independent capabilities. See the
[support contract](docs/support-contract.md) for platform requirements.

Published `mulciber` builds do not require Vulkan SDK validation layers. Enable the
`vulkan-validation` Cargo feature during development to require Khronos validation and its
debug messenger. Repository examples and probes explicitly enable it; leave it off when
shipping a game. This feature is independent of debug/release optimization.
Use 0.13.17 or newer: it also makes device debug labels optional while retaining GPU timing.

## Examples

Run an example from this repository. Each uses one safe application source and selects native
Metal or Vulkan at compile time.

| Command | Workload | Details |
| --- | --- | --- |
| `cargo run -p mulciber-clear` | Clear-only surface lifecycle | [graphics contract](docs/api-graphics-contract.md) |
| `cargo run -p mulciber-capability-report` | Device-capability selection report, no rendering setup | supports `--force-one-sample` |
| `cargo run -p mulciber-cube` | Spinning indexed, textured, depth-tested cube | [cube contract](docs/api-cube-contract.md) |
| `cargo run -p mulciber-input-cube` | Ordered native keyboard/pointer/scroll/focus input | [input contract](docs/input-contract.md) |
| `cargo run -p mulciber-postprocess-cube` | Half-render-scale resolve plus a uniform-animated fullscreen underwater grade on Vulkan | [postprocess contract](docs/postprocess-contract.md) |
| `cargo run -p mulciber-showcase-cube` | Input and two-pass composition | composes the two above |
| `cargo run -p mulciber-scene` | 100-object heterogeneous multi-draw scene | [scene contract](docs/scene-contract.md) |
| `cargo run -p mulciber-instanced-scene` | Same field grouped into four native instance batches | [instancing contract](docs/instancing-contract.md) |
| `cargo run -p mulciber-material-scene` | Application-authored materials, layouts, uniform bytes, cascaded shadow maps, and a frame-transient HUD overlay | [material contract](docs/material-contract.md) |
| `cargo run -p mulciber-game-slice` | Playable Forge Run game on `mulciber-runtime` | [game contract](docs/game-slice.md) |

The examples are ordinary interactive programs; Mulciber prefers 4x MSAA and reports a fallback to
1x. The cube examples use `glam` locally for transform math; no Mulciber crate depends on it.

### Writing your own program

New programs follow the `examples/` pattern: copy an example package (path dependencies on the
Mulciber crates, `publish = false`, workspace lints), add it to the root workspace `members`, and
start from the example nearest your workload. A few conventions to know:

- A field the simulation also needs — terrain displacement, say — is authored once in WGSL and
  generated for the host with `mulciber_shader::compile_host_field` from a `build.rs`, then pulled
  in with `include!` inside a module of its own. The host answer is an ordinary synchronous Rust
  call, available in the tick that asks for it.
- Shaders are offline artifacts. No shader compiler ships in the game process and there is no
  runtime-WGSL path; each example embeds a checked-in `.shaderbin` selected by its `build.rs`.
  Reuse a checked-in artifact when your pipeline shape matches (several examples and probes share
  the cube's artifact for the standard textured pipeline), or generate a new one with
  [`mulciber-shader`](crates/mulciber-shader/README.md).
- Rendering suspends while a window is minimized or fully occluded: redraw delivery pauses and
  resumes with visibility, so a program counting presented frames stalls while hidden. See the
  [platform contract](docs/api-platform-contract.md).

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
- [Shader toolchain](crates/mulciber-shader/README.md)
- [Materials](docs/material-contract.md), [HDR and bloom](docs/hdr-bloom-contract.md),
  [volumetrics](docs/volumetric-contract.md), and [scene depth](docs/scene-depth-contract.md)
- [Floating-point textures and queue-ordered updates](docs/float-texture-uploads.md) and
  [block-compressed textures](docs/block-compressed-textures.md)
- [Architecture](docs/architecture.md) and [backend contracts](docs/backend-contracts.md)
- [Roadmap](docs/roadmap.md), [viability gates](docs/viability-gates.md), and
  [API extraction plan](docs/api-extraction-plan.md)
- [Changelog](CHANGELOG.md)

### Display-aware pacing (platform/runtime 0.5.5)

Native AppKit timing now distinguishes fixed/variable/unknown refresh; the runtime smooths only
a known fixed period and uses elapsed time otherwise. Explicit caps remain independent. Other
platforms report unknown until native capability evidence is added. Fixed-panel native evidence
and synthetic VRR regressions do not establish physical VRR support or advance a viability gate.
See [frame-pacing controls](docs/frame-pacing-controls.md).

### Adaptive/Strict presentation (graphics 0.13.21)

The graphics API now owns adaptive synchronization and automatic full/half refresh with
workload-based recovery. Applications query support and select policy; fixed-divisor scheduling
is native, not a rounded integer CPU cap. The Apple M2 fixed-60-Hz probe demonstrates stable
full/half/full transitions and API conformance passes 101 cases with Metal validation. Vulkan
Adaptive uses FIFO relaxed where a driver exposes it and otherwise
`VK_KHR_present_mode_fifo_latest_ready`; Strict/HalfRefresh use capability-gated relative
presentation timing. The new paths are compile checked, not physically validated here. VRR and multi-display evidence remain outstanding.
See [behavior, reproduction and limits](docs/frame-pacing-controls.md#adaptive-and-strict-presentation--graphics-01321).


### Strict pacing correction (graphics 0.13.24)

Strict starts at full refresh, steps down after sustained workload overload, and
recovers with five percent headroom. Vulkan workload timing excludes native
submission and image-availability waits. Regression tests cover 85-90 FPS capacity
on a 75 Hz display. The user reports the consuming game works on Windows / RTX
3060 Ti / 75 Hz but Strict still behaves irregularly; this is not a claim of
fully resolved Strict pacing or broader hardware coverage. See
[the correction and validation limits](docs/frame-pacing-controls.md#strict-recovery-correction-01324-2026-09-21).
