# Experimental runtime timing and input contract

`mulciber-runtime` is the first runtime boundary extracted from a playable Mulciber application. It
is intentionally a small timing/input crate, not an engine framework or an owner of every platform
and graphics lifecycle decision.

## Application shape

The runtime begins from an explicit clock origin and fixed update rate:

```rust
let mut runtime = Runtime::new(RuntimeConfig::fixed_hz(60)?, Instant::now());
```

The application forwards ordered `mulciber-platform::InputEvent` values as they arrive. On each
redraw it asks for a `FramePlan`, handles one-frame input transitions, runs the requested number of
fixed updates, runs variable presentation work with the clamped frame delta, and renders its own
previous/current state with `FramePlan::interpolation()`.

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
`Runtime::end_frame` clears transitions and scroll samples while preserving held controls.

Focus loss synthesizes release membership for every held key and pointer button, clears held state,
and clears modifiers. This prevents movement or dragging from sticking if a native backend cannot
deliver the corresponding physical release while the window is unfocused.

## Ownership boundary

The first runtime slice owns:

- input snapshot accumulation and focus-loss clearing;
- fixed-step accumulation and render interpolation;
- variable-delta clamping, catch-up limits, and dropped-time reporting.

It does not yet own:

- the AppKit, Win32, Wayland, or X11 event pump;
- native display synchronization or presentation pacing;
- game state, collision, transforms, camera, scene, or renderer architecture;
- suspension/resume, fullscreen/display transitions, jobs, or device recovery.

Those missing capabilities remain Gate 5 work. They should be extracted from physical workloads and
native lifecycle evidence rather than inferred from this clock utility.

## Current evidence

Unit tests cover invalid timing limits, partial-step accumulation, interpolation, frame clamping,
catch-up discard reporting, held/pressed/released key semantics, key repeats, focus-loss releases,
pointer buttons, and precise scroll preservation. Forge Run is the first integrated consumer and
keeps simulation updates ahead of graphics acquisition so drawable unavailability does not directly
gate game time.

The earlier application-owned Forge Run checkpoint was physically exercised on Apple M2 / macOS
15.7.7 and Intel Vulkan 1.3 / Windows 11. After migration, the operator replayed the runtime-backed
path on the same Apple M2 machine and reported that the game and interpolated movement felt correct.
No measured frame-cadence, runtime-backed Windows, suspension, display-transition, or recovery claim
is inferred from that focused check.
