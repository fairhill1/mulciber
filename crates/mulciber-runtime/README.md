# mulciber-runtime

Experimental game-loop timing and input coordination for Mulciber.

`mulciber-runtime` combines a fixed-rate simulation clock, render interpolation, bounded hitch
catch-up, input snapshots, rendering suspension, and presentation-cadence pacing. It consumes
platform events without owning the native event pump or game state.

## Frame flow

Forward platform events to `Runtime::handle_window_event`, drain presentation feedback with
`Runtime::record_presented`, then call `Runtime::begin_frame` once per rendered frame. The returned
`FramePlan` specifies the exact number and duration of fixed updates plus the interpolation fraction
for rendering between the previous and current simulation states.

Frame deltas follow observed presentation cadence when fresh feedback is available and fall back to
wall-clock timing otherwise. Cadence smoothing keeps cumulative scheduled time within 16 ms of
elapsed time; an outdated FPS estimate cannot accelerate gameplay for seconds after recovery.
Fallback preserves this bounded offset, and resume resets it. Catch-up work and accepted frame time
are bounded so a hitch cannot create an unbounded simulation spiral; discarded time remains visible through diagnostics.

## Input semantics

`InputSnapshot` exposes held controls and pressed/released transitions with focus-loss clearing.
Transient presses, releases, and scroll samples remain latched through render-only frames and are
consumed after a frame schedules fixed simulation work. This prevents one-shot input from being
lost when the display rate exceeds the fixed simulation rate.

Transitions belong to the whole simulation-bearing frame rather than individual catch-up ticks.
Handle an edge-triggered action once before the fixed-update loop, guarded by
`FramePlan::fixed_steps() != 0`; every fixed update may read the latest held state.

## Scope

The crate owns timing, input accumulation, presentation-cadence diagnostics, and rendering
suspend/resume coordination. It deliberately does not own the platform event pump, graphics
submission, collision, scene state, camera policy, process suspension, jobs, or device recovery.

The complete experimental contract and validation record live in the
[Mulciber repository](https://github.com/fairhill1/mulciber/blob/main/docs/runtime-contract.md).


## Optional frame-start limiting

`FrameStartLimiter::new(true)` enables an independent CPU frame-start limiter. Supply
the native display period through `set_refresh_interval`, then call `wait` before pumping
fresh input. Use `new(false)` on paths already paced by presentation. Without a usable
native period it performs no wait; never substitute measured FPS for the display period.
Call `reset` on rendering suspension/resume and supply refreshed display information
after a surface or monitor change. Short overruns preserve the deadline grid; long stalls
restart without a burst of catch-up frames. The final 300 microseconds may busy-wait.
This utility does not change `Runtime` simulation, interpolation, or presentation policy.
