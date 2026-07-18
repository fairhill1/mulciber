# Gate 4 presentation pacing and timing plan

Gate 4 requires one feature that materially motivates native backends, compared fairly against
direct native implementations and the best practical `wgpu` path, with a pass only if the difference
matters to a real game. This document records the candidate decision and pre-registers the
implementation scope, measurements, comparison targets, and pass/redesign/stop conditions before
implementation begins, so an unfavorable result cannot be reframed after the fact.

## Candidate decision (recorded 2026-07-18)

The primary Gate 4 candidate is **native presentation pacing and timing feedback**, with SDK- and
capability-gated MetalFX-class upscaling as its Metal-side companion. A bindless, GPU-driven
rendering path remains the **secondary** candidate, to be revisited after the primary verdict with a
voxel-style streaming workload as its reference.

Rationale, from the [consumer evidence](consumer-evidence.md):

- Presentation pacing pain is five-for-five across the surveyed portfolio, including one game that
  reimplements vsync-grid reconstruction in software because the `wgpu`/`winit` seam exposes no true
  presentation timing, and one that stalls the CPU mid-frame to reach MetalFX through interop.
- The capability is structural to the seam rather than a missing feature: present feedback belongs
  to the surface the graphics library owns while pacing belongs to the loop the windowing library
  owns, so neither upstream project can deliver the coordinated control alone. Unlike GPU-feature
  differentiation, this does not erode as `wgpu` absorbs native extensions.
- The GPU-feature candidate has exactly one observed consumer, with its demand visible mainly as
  workarounds. That evidence is censored, not negative, so the candidate is deferred rather than
  dropped.

Gate 4's stop clause applies symmetrically: if `wgpu`/`winit` expose comparable presentation timing
and pacing control before this evaluation concludes, that is recorded and the stop condition is
honored rather than argued around.

## What the native path must expose

- **Per-frame presentation feedback**: the actual presentation time (or presentation completion) of
  identified frames. Metal: drawable presented handlers and presented-time queries. Vulkan:
  `VK_KHR_present_id`/`VK_KHR_present_wait` and display-timing extensions where the adapter exposes
  them, plus platform feedback such as the Wayland presentation-time protocol and XPresent where
  available. Per-adapter and per-platform availability is recorded, and an explicit estimation
  fallback exists and is observable when native feedback is absent.
- **Refresh cadence observation**: the display refresh interval and changes to it.
- **Pacing policy inputs**: negotiated present-mode selection visible to the application, a frame
  latency bound, and runtime scheduling hooks so `mulciber-runtime` can align simulation and
  presentation cadence instead of guessing.
- **Pacing diagnostics**: present-interval distributions, missed-interval counts, and
  suspension/resume transition instrumentation as runtime-owned reports.
- **Metal companion**: MetalFX-class upscaling integrated on the native Metal path without a
  mid-frame CPU wait, SDK- and capability-gated with a native-resolution fallback. No false Vulkan
  equivalent is fabricated; per Gate 4, inability to express a feature on a target is recorded as a
  reach result.

## Pre-registered measurements

All measurements run on the Forge Run workload (extended where the scenario requires it) on recorded
hardware, OS, and display tiers, with native validation enabled. Single-display evidence is claimed
as single-display evidence.

1. **Present-interval distributions** (mean, p95, p99, worst, missed-interval count) for three fixed
   interactive scenarios: steady camera motion, an induced load spike, and resume from
   minimize/occlusion.
2. **Timestamp fidelity**: implement the same pacing-consumer logic on each stack and record whether
   it receives reported presentation times or must estimate them, how much application code the
   estimation costs, and how the two diverge under the load-spike scenario. This is the direct
   reproduction of the surveyed PLL workaround.
3. **Upscaling integration cost** (Metal tier only): frame time with upscaling on and off, and CPU
   wait time inside the frame, for the native integration versus a `wgpu` HAL-interop baseline
   reproducing the surveyed mid-frame stall.
4. **Cost controls**: dependency count, binary size, and clean-build time deltas against the
   existing game-slice comparison figures, so the feature does not silently regress Gate 2 results.

## Comparison targets

| Target | Role | Notes |
| --- | --- | --- |
| Direct native probes | Ground-truth ceiling for what each OS actually exposes. | Extend the existing Metal and Vulkan probes with presentation-feedback instrumentation first. |
| `wgpu`/`winit`, pinned | The practical portable baseline. | Best-effort timing using whatever the pinned versions expose, estimation included; native-interop escape hatches are permitted with their cost recorded, mirroring surveyed practice. |
| SDL3 | Integrated platform/GPU baseline. | Record what its integrated stack exposes for present timing and pacing; scoped, not a full port. |

Exact pinned versions, sources, environments, validation output, and raw measurements are preserved
with the runs, following the Gate 2 comparison conventions.

## Pass, redesign, and stop conditions

- **Pass** requires all of: a measured cadence improvement (or equal cadence with materially less
  application code and estimation) in at least one pre-registered scenario on physical hardware; an
  operator-reported felt difference in the dogfood game rather than only a synthetic probe; the
  Metal upscaling path running without the mid-frame CPU wait; and no disqualifying regression in
  the cost controls.
- **Redesign** if native feedback is exposed correctly but produces no measurable or felt advantage
  over the estimation approach, or if the pacing vocabulary cannot be expressed without leaking
  backend-specific presentation machinery into ordinary application code.
- **Stop** per Gate 4 if established libraries expose the needed control comparably by evaluation
  time, or if the feature requires an escape hatch so invasive that Mulciber retains no useful
  portable contract.

## Sequence

1. **Probe-first evidence**: extend `mulciber-metal-triangle` and `mulciber-vulkan-triangle` with
   presentation-feedback and cadence instrumentation; record per-platform and per-adapter
   availability on the existing hardware tiers before any API extraction.
2. **Extract the minimal runtime pacing vocabulary**: diagnostics first, policy second, following
   the evidence-before-abstraction rule.
3. **Consume it in Forge Run**, and give the pinned `wgpu`/`winit` peer the equivalent best-effort
   implementation.
4. **Run the pre-registered measurements**, record the raw results, and write the Gate 4 decision:
   pass, redesign, or stop. The secondary bindless/GPU-driven candidate is re-evaluated after that
   decision with the consumer-evidence caveats attached.
