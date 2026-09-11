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
  lifecycle modules, plus a pointer-capture/cursor-mode intent implemented on all four backends —
  physically verified on the Apple M2 and KDE Linux tiers — with the Win32 implementation
  cross-checked from Linux only and never executed on Windows
  ([platform contract](docs/api-platform-contract.md)).
- `mulciber` exposes experimental device/queue/surface owners, owning resource handles, surface
  generations, nonfatal acquisition outcomes, frame dispositions, drained native presentation
  feedback, opt-in correlated GPU duration feedback (including capability-checked Metal render
  stage counters), RGBA8 sRGB and linear-UNORM sampled-texture
  uploads with optional application-authored mip chains, three frames in flight on Metal and Vulkan,
  immutable shared-vertex indexed mesh
  parts, bounded completed-frame lazy resource reclamation, and recovery-oriented errors
  ([graphics contract](docs/api-graphics-contract.md),
  [decision ledger](docs/api-slice-decisions.md)).
- `mulciber-runtime` provides input snapshots with focus-loss clearing, a configurable fixed-step
  accumulator with bounded catch-up, clamped variable updates, render interpolation, rendering
  suspension coordination, and presented-cadence smoothing with bounded cumulative time drift
  and diagnostics
  ([runtime contract](docs/runtime-contract.md)).
- `mulciber-shader` is a separate offline tool that turns WGSL into validated, cached native
  artifacts; no shader compiler ships in the game process. It also generates host-callable Rust
  evaluators for designated functions of that same WGSL, so a simulation can ask what the shader
  draws — a displacement height field, for instance — without a second hand-written copy of it
  ([crate README](crates/mulciber-shader/README.md)).

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
| `cargo run -p mulciber-capability-report` | Device-capability selection report, no rendering setup | supports `--force-one-sample` |
| `cargo run -p mulciber-cube` | Spinning indexed, textured, depth-tested cube | [cube contract](docs/api-cube-contract.md) |
| `cargo run -p mulciber-input-cube` | Ordered native keyboard/pointer/scroll/focus input | [input contract](docs/input-contract.md) |
| `cargo run -p mulciber-postprocess-cube` | Half-render-scale resolve plus a uniform-animated fullscreen underwater grade on Vulkan | [postprocess contract](docs/postprocess-contract.md) |
| `cargo run -p mulciber-showcase-cube` | Input and two-pass composition for side-by-side review | composes the two above |
| `cargo run -p mulciber-scene` | 100-object heterogeneous multi-draw scene | [scene contract](docs/scene-contract.md) |
| `cargo run -p mulciber-instanced-scene` | Same field grouped into four native instance batches | [instancing contract](docs/instancing-contract.md) |
| `cargo run -p mulciber-material-scene` | Application-authored materials, layouts, uniform bytes, cascaded shadow maps, and a frame-transient HUD overlay | [material contract](docs/material-contract.md) |
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
shared-vertex mixed-width mesh parts in material and shadow passes, and fallible shutdown.

### Writing your own program

New programs follow the `examples/` pattern: copy an example package (path dependencies on the
Mulciber crates, `publish = false`, workspace lints), add it to the root workspace `members`, and
start from the example nearest your workload. Two conventions are easy to miss:

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
passes, timestamps and debug labels, and device-specific pipeline artifacts. The Vulkan probe
additionally dispatches a WGSL height field over 256 directions and compares the readback with the
host evaluator generated from that same source. The exercised
capabilities are itemized in [roadmap sections 1 and 2](docs/roadmap.md).

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle    # macOS
cargo run -p mulciber-vulkan-triangle -- --frames 600     # Windows/Linux; validation required
```

Both probes accept `--frames N` and `--abandon-acquired-frame-once`, print a presentation pacing
report, and support `--pacing-csv PATH` for per-frame samples plus `--load-spike START:COUNT:MILLIS`
for the load-spike scenario in the [Gate 4 pacing plan](docs/gate4-pacing-plan.md). The Metal
report uses native presented-time feedback from drawable presented handlers against the queried
display cadence; the Vulkan report drains native `VK_EXT_present_timing` presented-stage times
beside the CPU present-return estimation where the surveyed tier exposes the extension chain
(currently the Linux Nvidia tier), and remains labeled estimation-only with the observable reason
elsewhere. The Metal probe generates and
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
- [Shared-vertex mesh-parts contract](docs/mesh-parts-contract.md)
- [Implementation roadmap](docs/roadmap.md) and
  [API extraction and comparison plan](docs/api-extraction-plan.md)
- [macOS](docs/macos-validation.md), [Windows](docs/windows-validation.md), and
  [Linux](docs/linux-validation.md) validation runbooks
- [Pinned references](docs/references.md)

### Foreground material pass (headless checkpoint)

The first-person weapon workload adds a depth-isolated foreground group before
postprocessing and the HUD overlay, implemented on Vulkan and Metal. See the
[material contract](docs/material-contract.md)
for ordering, MSAA storage, and validation limits. Physical rendering and visual
validation of this addition remain outstanding.

## HDR scene and bloom

The opt-in RGBA16Float material/postprocess path supports independently optional bloom and
volumetric passes, with cached pipeline selection for live effect toggles. Bloom adds six levels before the final sRGB
composite and native-resolution HUD. See the [HDR contract](docs/hdr-bloom-contract.md) for APIs,
format checks, synchronization, ownership and the validation boundary. Native rendering and visual
validation of this addition remain outstanding.

## Volumetric HDR scattering

The opt-in HDR path can now sample native-MSAA world depth and the submitted shadow cascades
for a half-resolution scattering pass, followed by additive depth-aware upscaling before
foreground and bloom. See the [volumetric contract](docs/volumetric-contract.md) for binding, lifetime and
validation boundaries. This is a headless checkpoint; native visual and performance evidence
remains outstanding.

## Material scene-depth snapshot (headless checkpoint)

HDR materials can sample a world-depth snapshot before volumetric and foreground passes,
using the existing 1x/4x depth shader support. The first consumer splits the world pass;
normal depth testing remains active. See the [scene-depth contract](docs/scene-depth-contract.md)
for ordering, native copy ownership, validation and remaining hardware evidence.

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

## Windows mailbox presentation (0.13.11)

Windows Vulkan prefers supported mailbox presentation with FIFO fallback;
Linux FIFO and Metal behavior are unchanged. Optional presentation timing is
bounded to native queue capacity, and unavailable timestamps remain untimed.
The user confirmed the Windows input delay is gone. See
[Windows validation](docs/windows-validation.md#windows-mailbox-presentation-01311)
for measured results and hardware coverage.
