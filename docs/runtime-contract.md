# Experimental runtime timing and input contract

`mulciber-runtime` is the first runtime boundary extracted from a playable Mulciber application. It
is intentionally a small timing/input crate, not an engine framework or an owner of every platform
and graphics lifecycle decision.

## Application shape

The runtime begins from an explicit clock origin and fixed update rate:

```rust
let mut runtime = Runtime::new(RuntimeConfig::fixed_hz(60)?, Instant::now());
```

The application forwards each `mulciber-platform::WindowEvent` through
`Runtime::handle_window_event`; the runtime consumes its input and rendering-lifecycle portions while
redraw, metrics, close, and graphics policy stay with the application. On each redraw it begins a
scoped `RuntimeFrame`, handles one-frame input transitions, runs the requested number of fixed
updates, runs variable presentation work with the clamped frame delta, and renders its own
previous/current state with `FramePlan::interpolation()`. Dropping the frame automatically clears
transient input, including on an early return.

This makes the simulation rate independent from a 60, 120, 144, 240, or other-Hz display. The
renderer deliberately runs one fixed step behind the newest simulation state and interpolates
`previous.lerp(current, alpha)`, where alpha is always below one. That small latency is the standard
accumulator tradeoff for smooth motion without predicting future game state.

## Timing semantics

- `RuntimeConfig::fixed_hz` selects a representable, nonzero fixed step. Forge Run uses 60 Hz.
- Wall-clock time accumulates until one or more complete fixed steps are available.
- `FramePlan::fixed_steps` and `fixed_step` define deterministic simulation work; the application
  must run exactly that many equal-duration updates before rendering.
- `frame_delta` is separately clamped variable time for presentation-only work. It must not be used
  to make fixed simulation outcomes depend on monitor cadence.
- The default clamp accepts at most 250 ms from one frame and schedules at most eight fixed updates.
  Whole steps beyond that catch-up budget are discarded to prevent a spiral of death.
- `dropped_time` reports time lost to either clamp so diagnostics can expose rather than hide a
  sustained overload.
- The fractional accumulator becomes `interpolation`; the application owns previous/current state
  and the policy for interpolating or snapping each property.

The runtime uses `std::time::Instant` and has no third-party dependency beyond
`mulciber-platform`.

## Input semantics

`InputSnapshot` preserves held state across frames and exposes pressed/released membership for the
current frame. Key-repeat events do not manufacture new presses. Pointer position, held and
transitional pointer buttons, modifiers, and ordered precise/coarse scroll samples are also retained.
Dropping `RuntimeFrame` clears transitions and scroll samples while preserving held controls.

Focus loss synthesizes release membership for every held key and pointer button, clears held state,
and clears modifiers. This prevents movement or dragging from sticking if a native backend cannot
deliver the corresponding physical release while the window is unfocused.

Pressed and released membership belongs to the whole rendered frame, not to each fixed update. An
edge-triggered action such as reset should therefore run once before the fixed-step loop, as Forge
Run demonstrates. Every catch-up update currently consumes the latest held snapshot; platform input
events do not yet carry timestamps that would permit historical per-tick staging. Deterministic
replay/rollback evidence must decide whether that narrower input timeline belongs here later.

## Rendering suspension

`Runtime::handle_window_event` maps `RenderingSuspended` and `RenderingResumed` into runtime timing.
Suspension releases held controls and schedules zero fixed or variable time. Resume restarts
wall-clock sampling without counting the paused interval, while preserving the fractional
accumulator so the first restored render does not jump backward. Direct `suspend` and `resume`
methods remain available for a different application coordination shape.

This is rendering lifecycle only. It does not yet establish process suspension, system sleep, or
background execution policy where the platform may not emit the same drawable-state transitions.

## Pacing diagnostics

`PacingDiagnostics` is the diagnostics-first half of the pacing vocabulary from the
[Gate 4 pacing plan](gate4-pacing-plan.md): it consumes presented-frame timestamps as plain
`Instant`s, so it works with native presentation feedback, estimated timestamps, or any other
source, and stays independent of the graphics crate. It maintains a bounded window of presented
intervals and reports frame counts, untimed presentations, a median-of-window cadence estimate
(withheld until enough intervals exist), a min/median/p95/max interval summary, and a count of
intervals exceeding 1.5 times the running estimate. It owns no scheduling policy: nothing sleeps,
throttles, or reorders work. Scheduling hooks are deliberately deferred until the probe-first
evidence and the Vulkan availability survey say what policy inputs are real.

## Ownership boundary

The first runtime slice owns:

- input snapshot accumulation and focus-loss clearing;
- fixed-step accumulation and render interpolation;
- variable-delta clamping, catch-up limits, and dropped-time reporting;
- input/rendering-lifecycle event mapping and rendering suspend/resume timing;
- presented-cadence diagnostics over application-supplied timestamps; and
- scoped frame cleanup on normal completion or early return.

It does not yet own:

- the AppKit, Win32, Wayland, or X11 event pump;
- native display synchronization or presentation pacing policy;
- game state, collision, transforms, camera, scene, or renderer architecture;
- process/OS suspension, fullscreen/display transitions, jobs, or device recovery.

Those missing capabilities remain Gate 5 work. They should be extracted from physical workloads and
native lifecycle evidence rather than inferred from this clock utility.

## Focused wgpu/winit comparison

`comparisons/wgpu-game-slice` recreates Forge Run through pinned wgpu 30.0.0 and winit 0.30.13. It
locally implements only the held/pressed keyboard state, focus clearing, fixed accumulator, hitch
clamp, eight-step catch-up limit, variable delta, interpolation, and occluded/zero-size suspension
required for equivalent game behavior. It does not inflate its count by recreating unused
`mulciber-runtime` pointer, scroll, release-membership, configuration-validation, or dropped-time
diagnostics.

The comparison therefore measures the current application integration experience, not total library
implementation size. Mulciber backend/runtime internals and wgpu/winit internals are excluded equally.
See the [game-slice comparison](game-slice-comparison.md) for source-count methodology and evidence.

## Current evidence

Unit tests cover invalid timing limits, partial-step accumulation, interpolation, frame clamping,
catch-up discard reporting, held/pressed/released key semantics, key repeats, focus-loss releases,
pointer buttons, precise scroll preservation, suspended time freezing, interpolation preservation,
and held-input release. Forge Run is the first integrated consumer and keeps simulation updates ahead
of graphics acquisition so drawable unavailability does not directly gate game time.

The application-owned and runtime-backed Forge Run checkpoints were physically exercised on an Apple
M2 running macOS 15.7.7, where the operator reported that the game and interpolated movement felt
correct. A later Metal-validation run physically confirmed that hold/minimize/release/wait/restore
caused no catch-up jump or stuck movement. On 2026-07-20, physically played Forge Run sessions on
native Wayland and on X11 through XWayland (committed `3075d0e`, KDE tier) repeated the
hold/minimize/release/wait/restore sequence on both paths with no catch-up jump or stuck movement
and exercised focus-loss clearing of held keys; see the
[Linux runbook](linux-validation.md). The Windows validation ledger separately establishes
Vulkan graphics and Win32 input slices, while the runtime-backed game currently has only Windows
cross-build evidence; it does not record a physical Windows Forge Run. No measured frame-cadence,
runtime-backed Windows, process/OS suspension, display-transition, or recovery claim is inferred
from the focused macOS and Linux checks. The wgpu/winit peer passed the general visual and
interaction review through Metal with API Validation enabled; its updated local suspension path
launched and closed cleanly, but no explicit physical hold/minimize/restore observation was
recorded.
