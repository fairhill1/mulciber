# Mulciber

[![crates.io](https://img.shields.io/crates/v/mulciber.svg)](https://crates.io/crates/mulciber)
[![docs.rs](https://docs.rs/mulciber/badge.svg)](https://docs.rs/mulciber)

Mulciber is a native game-development stack for Rust, built directly on Vulkan and Metal.

## Why Mulciber?

Mulciber is for Rust games that want native-engine control without maintaining separate graphics and
window-system stacks for every desktop platform. It combines direct Vulkan and Metal access with
game-focused platform lifecycle, exposes recent GPU capabilities without forcing them into a WebGPU
feature model, and keeps the shipped runtime small and auditable.

This is a deliberately narrower goal than `wgpu` plus `winit`, not a claim that those projects are
bad foundations. Mulciber trades their broad portability and maturity for native API reach,
coordinated GPU/platform/runtime design, and a first-class support contract limited to modern
Windows, Linux, and Apple-silicon macOS machines. Minimal dependencies are a means to predictable
ownership, policy, and maintenance, not the reason for the project by themselves.

Read [the project vision](docs/vision.md) for the intended user, non-goals, and the criteria
Mulciber must meet to justify its existence; the [viability gates](docs/viability-gates.md) govern
whether the project continues, narrows, or stops.

## Direction

- Vulkan 1.3 on Windows and Linux, requesting Vulkan 1.4 when the loader exposes it.
- Metal 3 on Apple silicon as the compatibility baseline.
- Metal 4 as an SDK- and capability-gated path.
- Native Win32, AppKit, Wayland, and X11 platform implementations.
- No `wgpu`, `winit`, or Direct3D dependency.
- Native presentation timing, pacing feedback, and platform-native upscaling as first-class
  capabilities.
- Modern features such as mesh shading, bindless resources, ray tracing, and sparse resources are
  independent capabilities rather than a single linear hardware tier.

## Status

Mulciber is an unstable experimental extraction, not a supported API. Native Metal and Vulkan probes
established the real platform contracts first; the public slice is derived from that evidence and
evaluated against the pre-registered comparisons in the
[API extraction and comparison plan](docs/api-extraction-plan.md). Current state:

- `mulciber-platform` owns peer AppKit, Win32, Wayland, and X11 application/window, event, and
  lifecycle modules ([platform contract](docs/api-platform-contract.md)).
- `mulciber` exposes experimental device/queue/surface owners, owning resource handles, surface
  generations, nonfatal acquisition outcomes, frame dispositions, and recovery-oriented errors
  ([graphics contract](docs/api-graphics-contract.md),
  [decision ledger](docs/api-slice-decisions.md)).
- `mulciber-runtime` provides input snapshots with focus-loss clearing, a configurable fixed-step
  accumulator with bounded catch-up, clamped variable updates, render interpolation, and rendering
  suspension coordination ([runtime contract](docs/runtime-contract.md)).
- `mulciber-shader` is a separate offline tool that turns WGSL into validated, cached native
  artifacts; no shader compiler ships in the game process.

Capability evidence, per-platform validation records, and the exact remaining gaps live in the
[roadmap](docs/roadmap.md) and the [macOS](docs/macos-validation.md),
[Windows](docs/windows-validation.md), and [Linux](docs/linux-validation.md) runbooks.

## Examples

Each example is one safe application source that selects native Metal or Vulkan at compile time.
Pinned `wgpu`/`winit` and direct-native peers under `comparisons/` implement the same workloads;
line counts and measurements are single-sourced in the linked contract documents.

| Command | Workload | Details |
| --- | --- | --- |
| `cargo run -p mulciber-clear` | Clear-only surface lifecycle | [graphics contract](docs/api-graphics-contract.md) |
| `cargo run -p mulciber-cube` | Spinning indexed, textured, depth-tested cube | [cube contract](docs/api-cube-contract.md) |
| `cargo run -p mulciber-input-cube` | Ordered native keyboard/pointer/scroll/focus input | [input contract](docs/input-contract.md) |
| `cargo run -p mulciber-postprocess-cube` | Offscreen resolve plus fullscreen grade/vignette pass | [postprocess contract](docs/postprocess-contract.md) |
| `cargo run -p mulciber-showcase-cube` | Input and two-pass composition for side-by-side review | composes the two above |
| `cargo run -p mulciber-scene` | 100-object heterogeneous multi-draw scene | [scene contract](docs/scene-contract.md) |
| `cargo run -p mulciber-instanced-scene` | Same field grouped into four native instance batches | [instancing contract](docs/instancing-contract.md) |
| `cargo run -p mulciber-game-slice` | Playable Forge Run dogfood on `mulciber-runtime` | [game contract](docs/game-slice.md), [comparison](docs/game-slice-comparison.md) |

The examples are ordinary interactive programs; Mulciber prefers 4x MSAA and reports a fallback to
1x. The cube examples use `glam` locally for transform math; no Mulciber crate depends on it.

Finite execution, acquired-frame abandonment/recovery, and forced 1x coverage live in explicit API
probes instead of the examples:

```sh
cargo run -p mulciber-api-clear -- --frames 120 --abandon-acquired-frame-once
cargo run -p mulciber-api-cube -- --frames 120 --abandon-acquired-frame-once
cargo run -p mulciber-api-cube -- --frames 120 --force-one-sample
```

`mulciber-api-conformance` additionally asserts invalid use, resource destruction/drop churn,
replacement rendering, direct and postprocessed multi-draw and instancing, mixed-session rejection,
and fallible shutdown.

## Native probes

The capability reports query each backend directly and emit versioned machine-readable output for
cross-machine comparison:

```sh
cargo run -q -p mulciber-metal-info -- --json                      # macOS
cargo run -q -p mulciber-vulkan-info -- --json                     # Windows
cargo run -q -p mulciber-vulkan-info -- --platform wayland --json  # Linux; or --platform x11
```

The presentation probes exercise the full representative native workloads: staging uploads, BC1
with verified readback, compute-written buffers/images/indirect commands, mip generation with
mip-tail verification, indexed-indirect drawing, capability-selected 4x MSAA, shadow/scene/post
passes, timestamps and debug labels, and device-specific pipeline artifacts. The exercised
capabilities are itemized in [roadmap sections 1 and 2](docs/roadmap.md).

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle    # macOS
cargo run -p mulciber-vulkan-triangle -- --frames 600     # Windows/Linux; validation required
```

Both probes accept `--frames N` and `--abandon-acquired-frame-once`. The Metal probe generates and
strictly reloads a device-specific binary archive; pass `--binary-archive PATH` to select a
different artifact or `--rebuild-binary-archive` after changing shaders, pipeline descriptors, the
OS, or the GPU. The Vulkan probe requires the Khronos validation layer and loads the platform
loader dynamically; its pipeline-cache flags, texture-mode controls, and fallback switches are
documented in the [pipeline cache policy](docs/vulkan-pipeline-cache.md),
[BC1 decision record](docs/vulkan-bc1.md), and the platform runbooks.

## Documentation

- [Vision](docs/vision.md), [viability gates](docs/viability-gates.md), and
  [support contract](docs/support-contract.md)
- [Architecture decisions](docs/architecture.md) and
  [backend contract ledger](docs/backend-contracts.md)
- [Implementation roadmap](docs/roadmap.md) and
  [API extraction and comparison plan](docs/api-extraction-plan.md)
- [macOS](docs/macos-validation.md), [Windows](docs/windows-validation.md), and
  [Linux](docs/linux-validation.md) validation runbooks
- [Pinned references](docs/references.md)
