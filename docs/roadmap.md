# Implementation roadmap

Each milestone is a runnable vertical slice. Public abstraction work follows backend evidence rather
than preceding it. Once the extraction-entry evidence is established, an unstable public slice may be
built to test its design and value; stable support claims still require the applicable viability gates.

Advancing between major milestones is governed by the [viability gates](viability-gates.md). Passing
more checkboxes is not sufficient if Mulciber is converging on a less mature `wgpu`/`winit` substitute or
cannot be learned efficiently without pre-existing ecosystem knowledge.

## 0. Capability inventory

- [x] Establish the support and dependency contracts.
- [x] Query the default Metal device without a graphics abstraction dependency.
- [x] Emit a machine-readable Metal capability report.
- [x] Emit a machine-readable Vulkan capability report with real Win32 presentation data.
- [x] Port the Vulkan capability report to X11 on Linux (physically exercised through XWayland on
  KDE Plasma with Vulkan 1.4; native Xorg hardware coverage remains pending).
- [x] Port the Vulkan capability report to Wayland on Linux (physically exercised natively on KDE
  Plasma with a compositor-discovered `wl_surface` and Vulkan 1.4).
- [x] Run the report with the macOS 26 / Metal 4 runtime and compare results (physically
  exercised on an Apple M5 running macOS 26.5.2 with runtime-detected Metal 4 symbols all
  present, against an Apple M2 running macOS 15.7.7 with all absent; recorded in the macOS
  validation runbook).
- [x] Pin the Vulkan registry, headers, loader, validation, profiles, and SPIR-V toolchain revisions.

## 1. Native presentation probes

- [ ] AppKit window with a `CAMetalLayer` and Metal triangle (implemented and physically
  exercised on an Apple M2 running macOS 15.7.7 with Apple's validation layer: continuous drag
  resize including very small sizes, minimize/restore, zoom/restore, full occlusion/reveal, and
  titlebar-close shutdown all completed cleanly with validation-clean output; a separate automated
  path acquired and abandoned one drawable before submission, then recovered for 120 submitted
  frames and clean shutdown; display-change, multi-display, differing backing scale factors,
  input transitions are implemented as an AppKit-first experimental slice but physical input
  evidence, the macOS 26 / Metal 4 rendering runtime, and broader Apple-silicon hardware evidence
  remain outstanding).
- [ ] Win32 window with a Vulkan 1.3+ swapchain and triangle (physically exercised on Windows 11 and
  an Nvidia RTX 3060 Ti; the window resizes smoothly, rendering remains functional, and driving
  redraw from `WM_SIZE` improved measured callback spacing from about 27 ms to 9 ms and looked
  noticeably better; presentation-fence retirement and the forced deferred fallback are physically
  exercised, and an Intel Vulkan 1.3 tier naturally exercised the extensionless paths plus API-level
  single-generation retirement; multi-display behavior and the GTX 1060-class baseline remain
  outstanding).
- [ ] Wayland XDG-shell window with a Vulkan 1.3+ swapchain and triangle (implemented and physically
  exercised on KDE Plasma/Wayland with server-side decorations, validation-clean rendering,
  responsive paced resize, minimize/restore, maximize/restore, titlebar close, and clean Vulkan/XDG
  shutdown; display-change, explicit zero-sized suspension, input, and broader compositor/hardware
  evidence remain outstanding).
- [ ] X11 window with a Vulkan 1.3+ swapchain and triangle (implemented as a runtime-selected peer
  of the Wayland module with `WM_DELETE_WINDOW`, structure-notification, and
  `_NET_WM_SYNC_REQUEST` interactive-resize handling; physically exercised on KDE Plasma through
  XWayland with validation-clean unlocked 75 Hz pacing, live drag resize, minimize/restore,
  maximize/restore, window-manager close, and clean shutdown; display changes, input,
  multi-display, and native Xorg coverage remain outstanding).
- [x] Replace conventional device-idle swapchain retirement with tracked presentation completion
  using presentation fences when available and a deferred-retirement fallback.
- [x] Prove one-shot Vulkan acquired-frame non-presentation through
  `vkReleaseSwapchainImagesKHR` when swapchain maintenance is enabled and whole-generation
  replacement on the base-swapchain fallback, followed by 120 presented recovery frames and clean
  shutdown (physically exercised on native Wayland on the current Nvidia tier, with the compatibility
  path forced).
- [ ] Record per-platform presentation-feedback availability for the
  [Gate 4 pacing plan](gate4-pacing-plan.md) (Metal presented handlers with `presentedTime` and
  drawable-ID correlation are physically exercised on the Apple M2 60 Hz tier, including
  vsync-quantized load-spike degradation and a per-frame CSV path; the Windows Intel UHD 620 tier
  is surveyed and exposes none of `VK_KHR_present_id`/`present_wait`, `VK_GOOGLE_display_timing`,
  or `VK_KHR_incremental_present`, making the estimation fallback the only path on that recorded
  tier; the Nvidia tier and the Wayland presentation-time survey remain outstanding).

Every probe must handle resize, zero-sized/minimized surfaces, VSync, acquire failure, and clean
shutdown with API validation enabled.

## 2. Representative rendering workload

Implement independently in Vulkan and Metal:

- Vertex, index, uniform, storage, and indirect buffers.
- Sampled, storage, depth, multisampled, and compressed textures.
- Uploads, readback, mip generation, and transient render targets.
- Graphics and compute pipelines with offline shader compilation.
- GPU timestamps, labels, and pipeline caching.
- One GPU-driven textured scene with depth, shadows, and post-processing.

Vulkan evidence completed so far:

- [x] Device-local vertex and index buffers populated through host-visible staging uploads, with
  indexed drawing (physically exercised on Windows 11 / RTX 3060 Ti with validation enabled).
- [x] Device-local RGBA8 sampled texture populated through a host-visible staging upload, explicit
  layout transitions, a combined image sampler descriptor, and fragment sampling (same machine).
- [x] Optional device-local BC1 checkerboard selected from the core feature plus exact sampled and
  transfer format roles, uploaded as four compressed blocks, copied back byte-for-byte, and sampled
  directly; required BC1 and forced RGBA8 paths are physically validated at native 4x, forced 1x,
  and resize while retaining strict pipeline-cache hits (same machine).
- [x] Capability-selected, resize-dependent device-local depth attachment with explicit layout
  transitions, depth testing/writes, and swapchain-retirement ownership (same machine).
- [x] Three persistently mapped, host-coherent uniform-buffer frame slots with matching descriptor
  sets, aspect-correct transforms, and shader-visible elapsed time (same machine).
- [x] Compute-written device-local storage buffer with explicit compute-to-copy and copy-to-host
  barriers plus exact GPU-to-CPU readback verification (same machine).
- [x] Compute-written device-local indexed-indirect command synchronized for transfer and
  indirect-command reads, verified through exact readback, and consumed by
  `vkCmdDrawIndexedIndirect` (same machine).
- [x] Compute-written device-local RGBA8 storage image with capability checks, exact texel
  readback, explicit compute-to-copy-to-fragment transitions, and fragment sampling (same machine).
- [x] Complete 8x8-to-1x1 storage-image mip chain generated through synchronized nearest blits,
  with exact 1x1 tail readback and explicit generated-mip fragment sampling (same machine).
- [x] Capability-selected 4x multisampled transient color/depth attachments resolved into the
  swapchain, with a physically validated 1x fallback and resize-retirement ownership (same machine).
- [x] Resize-dependent offscreen scene color target consumed by a second dynamic-rendering fullscreen
  vignette pass, with explicit color-write-to-sampled-read synchronization (same machine).
- [x] Capability-checked timestamp query pool with valid-bit wrap handling, fence-safe startup
  compute and per-frame shadow/scene/post timing, plus colored debug-utils labels for the same
  command regions (same machine).
- [x] Persistent 1024x1024 sampled depth map rendered by a depth-only light-space pass, synchronized
  for filtered fragment sampling by the main scene, and measured as its own diagnostic region (same
  machine).
- [x] Device-specific raw Vulkan pipeline cache shared by compute, shadow, native/forced-1x scene,
  and post pipelines, with header preflight, per-pipeline application-hit feedback, optional
  compile prohibition, atomic learning-mode persistence, strict read-only cross-process proof,
  corruption recovery, and a physically validated no-cache correctness control (same machine).

Metal evidence completed so far:

- [x] Shared vertex and index buffers with indexed drawing.
- [x] Private sampled texture populated from a staging buffer through a blit command.
- [x] Linear filtering, repeating texture coordinates, and fragment texture sampling.
- [x] Resize-dependent private depth target with depth testing and writes.
- [x] Compute pipeline writing a private storage texture consumed by the render pipeline.
- [x] GPU-to-CPU texture readback with byte-for-byte startup validation.
- [x] Offline MSL-to-metallib build with an embedded runtime library.
- [x] Triple-buffered, CPU-updated uniforms with explicit in-flight slot reuse.
- [x] Compute-written private storage buffer with verified GPU-to-CPU readback.
- [x] Indexed-indirect drawing from a native Metal argument buffer.
- [x] Capability-checked BC1 source texture decompressed through compute.
- [x] Generated mip chain with exact 1x1 mip-tail readback validation.
- [x] Memoryless 4x MSAA color and depth attachments resolved into the drawable.
- [x] Reusable shadow depth, main MSAA, and fullscreen post-processing passes.
- [x] Debug labels and command-buffer GPU start/end timing.
- [x] Strict cold-generation and cross-process loading of a device-specific Metal binary archive.

## 3. Extract and test the first Mulciber API slice

Follow the [API extraction and comparison plan](api-extraction-plan.md). Gate 1's remaining physical
coverage continues in parallel; this milestone creates an explicitly unstable experiment, not a
first-class support claim.

The current implementation checkpoint is the same-source spinning textured cube described in the
[textured-cube API contract](api-cube-contract.md). It is the resource-backed successor to the native
clear-surface checkpoint, not a replacement for the advanced backend probes. Ordinary clear and cube
examples remain interactive and free of validation switches; explicit public-API probes own finite,
fallback, and acquired-frame abandonment/recovery controls.

- [x] Define the experimental-extraction threshold, unresolved design decisions, comparison targets,
  tasks, measurements, single-backend scoring, and stop conditions.
- [x] Write the complete application-facing flow before committing to public type names, covering
  window creation, capability requests, device/surface creation, rendering, resize/suspension, frame
  completion, and fallible shutdown.
- [x] Decide and record main-thread/event-loop ownership, object topology, surface generations, frame
  presentation/non-presentation, resource-use vocabulary, error recovery, and safe native reach in
  the [API slice decision ledger](api-slice-decisions.md); native reach is recorded as a deliberate
  non-exposure with a binding constraint. A provisional recovery-oriented error taxonomy now
  distinguishes caller, lifecycle, resource, capability, surface, device, memory, validation,
  native, and internal failures while retaining contextual messages.
- [x] Extract the minimal event, window, display, and lifecycle spine into `mulciber-platform`, backed
  by peer AppKit, Win32, Wayland, and X11 modules.
- [x] Extract owned device, queue, mesh, texture, pipeline, surface-generation, frame, and
  presentation types into `mulciber` with Metal and Vulkan implementations.
- [ ] Generalize the deliberately narrow scene-submission vocabulary into owned buffer, binding,
  command/pass-composition, and synchronization facilities only as representative game slices force
  those concepts.
- [x] Build an intermediate same-source clear-only checkpoint through target-selected Metal and
  Vulkan, with scoped acquisition, reconfiguration, explicit abandonment, and fallible shutdown;
  keep device/queue/command topology private until the representative slice forces it.
- [x] Build the same textured depth-tested resize-aware example through both backends without
  ordinary backend branches or application `unsafe`.
- [x] Establish baseline, optional-capability, invalid-usage, surface-generation, frame-abandonment,
  resource-reclamation, multi-draw, instancing, and shutdown conformance tests:
  `probes/api-conformance` currently asserts eighteen Metal cases across those categories (plus the
  Vulkan-only superseded-generation branch when applicable) and exits nonzero on divergence;
  per-platform runs are recorded in the validation ledgers as they are exercised.
- [x] Prove that a Metal-only and Vulkan-only build neither links nor initializes the unused backend
  and does not add portability-only dispatch to the ordinary frame path; symbol, linkage,
  dependency-tree, size, and clean-build measurements are recorded in the
  [Linux](linux-validation.md) and [macOS](macos-validation.md) single-backend build evidence.
- [ ] Implement and preserve the pinned direct-native, practical single-backend Rust, `wgpu`/`winit`,
  SDL3 GPU, Vulkano, and scoped raylib comparisons.
- [ ] Record the Gate 2 pass, redesign, narrow, or stop decision from correctness, ergonomics,
  learnability, control, cost, performance, and single-backend results.
- [ ] Run the Gate 3 cold-start suite across subjects and tiers. The first pre-registered
  model-subject run on the Apple M2 tier completed five-for-five with first-compile success on
  every task and no reliability deficit against the familiarity-assisted `wgpu`/`winit` control,
  but claims no pass (the pre-registered task-by-task time condition failed on two
  interference-confounded tasks); see the [plan](gate3-cold-start-plan.md) and
  [2026-07-19 results](gate3-cold-start-results.md). The human arm, repetition, and the
  Vulkan-side tiers remain open.
- [ ] Keep backend-specific capabilities reachable without leaking native object ownership or
  bypassing presentation-retirement tracking.
- [ ] Add a pointer-capture/cursor-mode API to `mulciber-platform` that owns the native
  locked-versus-confined policy and cursor visibility; all five surveyed games hand-roll the same
  fallback (see the [consumer evidence](consumer-evidence.md)). The `CursorMode` intent,
  `PointerDelta` events, and focus/drop restoration policy are extracted and consumed by
  `mulciber-input-cube`, with the AppKit implementation physically verified for relative
  mouse-look on the Apple M2 tier. The Wayland implementation (pointer constraints, relative
  pointer, cursor-shape restore, `Unsupported` naming any missing global) and the X11
  implementation (confined grab, invisible cursor, warp-to-center deltas) passed agent-driven
  automated runs on the KDE tier on 2026-07-20 — including an XTEST-driven X11 run with the
  pointer measurably pinned to the content center while captured — with physical human
  verification pending; Win32 still reports explicit `Unsupported` (see the
  [input contract](input-contract.md)).

Platform spine: peer AppKit, Win32, Wayland, and X11 application/window/event paths live in
`mulciber-platform` and drive both full native probes. The extracted path passed the automated
Windows matrix and physical live-resize/lifecycle validation on Windows 11 / RTX 3060 Ti at
`044ae86`. Decisions, backing-scale policy, and remaining scale/display gaps:
[experimental platform contract](api-platform-contract.md).

Input: AppKit, Win32, Wayland, and X11 translate ordered physical-key, modifier, pointer, button,
scroll, and focus transitions through the fallible platform pump; `mulciber-input-cube` consumes
them beside a preserved `wgpu-input-cube` peer. The showcase passed focused physical checks on
Windows 11 / Intel UHD 620. The Wayland and X11 slices share one evdev key table; Wayland owns its
xkb keymap through libxkbcommon for modifier masks and synthesizes pump-paced key repeat, while
X11 uses detectable auto-repeat, core modifier masks, and live queries on modifier transitions.
The X11 path passed an automated XTEST-driven pipeline run (keys, drag, wheel, capture, both close
paths) through XWayland on 2026-07-20; physical human evidence on both Linux paths is pending, so
no stable cross-platform input support is claimed. See the
[experimental input contract](input-contract.md).

Two-pass postprocess: dedicated target/pipeline handles and one fixed two-pass queue operation
render the scene into generation-bound resolved color and sample it in a fullscreen grade/vignette
pass, deliberately not generalized into a command encoder. Passed validation-layer visual smokes on
the Apple M2 tier, plus 100 automated rapid resizes and a manual drag-resize/close pass on Intel UHD
620 / Vulkan 1.3.215. See the [experimental postprocess contract](postprocess-contract.md).

A fourth showcase pair composes the input controller with the two-pass operation for side-by-side
Mulciber/wgpu review without adding API; its line counts stay separate from the pre-registered Gate 2
figures.

Multi-object scene: `TexturedSceneDraw` plus direct and postprocessed scene operations submit an
ordered non-empty slice with per-record mesh, texture, pipeline, and transform; both backends keep
one scene pass open and issue one indexed draw per record. Passed Metal visual and conformance
checks on the Apple M2 tier and Vulkan conformance plus interactive lifecycle checks on Intel UHD
620. Line counts and the API boundary:
[experimental multi-object scene contract](scene-contract.md).

GPU instancing: an instance-rate pipeline and non-empty homogeneous batches drive four native
indexed draws for the same 100-object field through `Queue::render_and_present` and
`SceneSubmission`. Passed Metal visual and conformance checks on Apple M2 and nineteen-case Vulkan
conformance plus visual confirmation on Intel UHD 620. Line counts and native behavior:
[experimental GPU instancing contract](instancing-contract.md).

Runtime dogfood: Forge Run's playable loop earned a narrow `mulciber-runtime` extraction covering
held/pressed/released input snapshots with focus-loss clearing, a configurable fixed-step
accumulator with bounded hitch catch-up, clamped variable deltas, dropped-time diagnostics, and
render interpolation; the runtime does not own collision, camera, scene, or the platform/GPU event
loop. Rendering suspension freezes runtime time, preserves interpolation, releases held input, and
physically passed minimize/restore on macOS. Native pacing, process/OS suspension,
fullscreen/display transitions, device recovery, Linux input, and the full lifecycle comparison
remain pending, so Gate 5 is not complete. See the [runtime contract](runtime-contract.md) and
[game dogfood contract](game-slice.md).

The same Forge Run workload has pinned `wgpu`/`winit` and direct AppKit/Metal peers. The direct peer
passed its physical macOS checkpoint on the Apple M2, and the measured application-size, dependency,
build-time, and unsafe-site comparison (including the selected stack's `metal-rs`/`objc2`
binding-generation seam and future-incompatible `block` 0.1.6 dependency) is a favorable **continue**
checkpoint for Gate 2's single-backend score, not a pass. Matched cadence/performance/resource-cost,
failure-diagnosis, forced-fallback, and Gate 4 differentiation work remain open. Measurements are
single-sourced in the [game-slice comparison](game-slice-comparison.md).

Graphics lifecycle: `mulciber` exposes experimental physical surface extents, graphics-owned surface
generations, nonfatal acquisition outcomes, and presented/abandoned frame dispositions, consumed by
both native probes. The Windows Vulkan matrix passed after integration, followed by native Metal
archive, abandonment, and physical resize/lifecycle validation on an Apple M2 at `931b0dc`.
Acquisition was reshaped so reconfiguration happens inside it after the separate reconfigured
outcome measurably made trailing live resize the default application shape. Decision rows:
[API slice decision ledger](api-slice-decisions.md); see the
[experimental graphics contract](api-graphics-contract.md).

Clear checkpoint: `examples/clear` drives target-selected native Metal/AppKit or Vulkan from one
safe application source. The Vulkan path passed the Windows automated matrix on the RTX 3060 Ti tier
on 2026-07-17, including abandonment/recovery and resize reconfigurations; the Apple M2 tier passed
the same finite abandonment/recovery run under Metal API Validation plus an interactive smoke. This
checkpoint did not settle device, queue, resource, or command ownership.

Textured checkpoint: `examples/cube` uses one safe Rust source and one WGSL module through public
`Device`, `Queue`, and `Surface` owners plus owning, non-`Copy` mesh, texture, pipeline,
generation-bound target, and frame handles, with explicit fallible destruction and drop-queued
reclamation over reusable generational arenas. Naga is confined to the offline `mulciber-shader`
tool. Native KDE Wayland resize evidence forced two corrections, superseded-generation reclamation
and paced extent-driven reconfiguration (see the [Linux validation runbook](linux-validation.md)).
Destruction, drop churn, replacement rendering, and the Vulkan-only superseded-generation branch
passed on Apple M2 and Intel UHD 620 / Vulkan 1.3. The provisional command vocabulary remains open
for review and expansion through representative game slices.

Do not stabilize names merely because both backends compile. Stable claims wait for Gate 1 completion
and a successful Gate 2 decision.

## 4. Recent GPU capabilities

The differentiation this milestone tests narrows over time as `wgpu` continues absorbing native-only
features, and Gate 4 already treats "established libraries expose the needed path with comparable
control" as a stop condition. The first Gate 4 candidate therefore takes schedule priority over
completing the remaining Gate 2 comparison families, which re-measure coordination value that already
has three data points. The recorded candidate decision, based on the
[consumer evidence](consumer-evidence.md), selects native presentation pacing/timing (with SDK-gated
MetalFX-class upscaling on Metal) as primary and defers the bindless/GPU-driven path as secondary;
the [Gate 4 pacing plan](gate4-pacing-plan.md) pre-registers the scope, measurements, and
pass/redesign/stop conditions.

Add each as an independent backend feature with a tested fallback:

- Bindless resource tables and descriptor indexing.
- Mesh shading.
- Hardware ray tracing.
- Sparse resources.
- GPU-generated command paths.
- HDR and presentation timing.
- Metal 4 command allocation, argument tables, barriers, and pipeline datasets.

## 5. Runtime

Create `mulciber-runtime` only after platform and GPU lifecycle contracts stabilize. It will coordinate
the game loop, fixed and variable updates, input snapshots, frame pacing, jobs, suspension, and
device recovery.

- [x] Extract frame-scoped input snapshots from physically exercised AppKit and Win32 transitions.
- [x] Extract fixed/variable timing with bounded catch-up, dropped-time reporting, and render
  interpolation; migrate Forge Run as the first consumer without moving game policy into the crate.
- [x] Build and physically compare the same Forge Run workload through `wgpu`/`winit`, including
  equivalent local fixed-step, input, and interpolation glue.
- [x] Coordinate platform rendering suspend/resume with frozen timing, preserved interpolation, and
  held-input clearing; physically exercise minimize/restore on macOS.
- [ ] Establish native presentation pacing and cadence diagnostics independently from simulation
  rate (the diagnostics half is extracted: `Surface::take_present_feedback` drains identified
  presented times on Metal with explicit `Unsupported` on Vulkan, and
  `mulciber-runtime::PacingDiagnostics` reports cadence estimates, interval distributions, and
  missed intervals, consumed by Forge Run and `mulciber-api-cube`, and the pinned wgpu/winit Forge
  Run peer carries the equivalent best-effort present-return estimator; pacing policy and the
  Vulkan feedback path remain outstanding per the [Gate 4 pacing plan](gate4-pacing-plan.md)).
- [ ] Establish process/OS suspension, peer Windows/Linux runtime evidence, fullscreen/display
  transitions, and device recovery.
- [ ] Evaluate timestamped or per-tick input staging against a deterministic replay/rollback
  workload; current catch-up steps intentionally consume the latest frame snapshot. The
  [consumer evidence](consumer-evidence.md) records a high-refresh one-shot-input workaround and
  render-rate-decoupled pointer look as concrete inputs to this evaluation.
- [ ] Add supported Linux input and runtime evidence, then perform the full Gate 5 lifecycle
  comparison.
