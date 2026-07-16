# Experimental platform and window contract

This document records the first API extraction from the native probes. The types and names are
unstable and exist to test Gate 2; they are not a supported platform claim. The implementation began
from revision `449c01cb1997fedd674a4a58bd0105f141a3317b` and is initially exercised through the
AppKit/Metal probe. Peer Win32, Wayland, and X11 implementations remain required before this candidate
contract can be judged coherent.

## Extracted boundary

`mulciber-platform` now owns:

- connection to the native application environment;
- creation and destruction of an owned native window;
- native event dispatch;
- translation of drawable extent and backing scale into `WindowMetrics`;
- monotonically increasing `WindowRevision` values for changed drawable metrics;
- rendering suspension, resumption, redraw, metric-change, and close events; and
- a borrowed opaque `SurfaceTarget` used to connect the graphics layer without transferring native
  window ownership.

The Metal probe no longer creates or polls `NSApplication`, `NSWindow`, or `NSView` directly. It
retains Metal ownership, creates the `CAMetalLayer`, and consumes platform redraw and window metrics.
This is an intentional intermediate boundary: platform lifecycle is extracted before
GPU resource and command APIs, while the full validated workload continues to exercise it.

## Decisions established by this slice

### Main-thread ownership

`Application::new` verifies the process main thread before connecting to AppKit. `Application`,
`Window`, and the borrowed surface target are intentionally neither `Send` nor `Sync`. This makes
main-thread platform ownership structural rather than a comment that ordinary application code can
accidentally violate.

### Game-owned loop with native event pumping

The game calls `Application::pump_events` and receives translated events through a callback. This
keeps the game in control of its architecture while leaving room for a native backend to invoke redraw
during nested or modal event processing. The current AppKit path emits `RedrawRequested` after queued
events are dispatched whenever the surface is drawable. A later Win32 implementation must prove that
the same callback shape can preserve the already validated live-resize redraw behavior.

This is not yet a commitment that polling is the final runtime API. Gate 5 may add an owning runtime
loop above this layer, but it must not invalidate the lower-level game-controlled path without a
written comparison.

The first AppKit slice permits exactly one live `Window` per `Application`. Its event queue is
process-wide, while this candidate API pumps events against one explicit window; accepting multiple
windows here would silently route lifecycle state through the wrong boundary. Dropping the window
releases the slot so another can be created. Multi-window support remains a deliberate later design
step that must introduce application-level window identity and event routing rather than pretending
the present callback is already sufficient.

### Window metrics and presentation ownership

`WindowMetrics` carries physical pixel extent, backing scale, and a revision. A changed physical
extent or scale advances the revision so the graphics layer can observe platform changes without
receiving native resize messages.

Initial window requests use the separate `LogicalSize` type while drawable state uses
`PhysicalExtent`. Keeping those coordinate spaces distinct prevents AppKit points, Win32 logical
coordinates, and compositor-provided physical extents from becoming interchangeable integers.

`mulciber-platform` deliberately does not issue a `SurfaceGeneration`. Presentation remains owned by
`mulciber`: a Vulkan swapchain can become outdated or change format without new platform metrics, and
only the graphics backend knows when presentation-dependent resources have actually entered a new
generation. The future graphics surface will consume window revisions alongside native acquisition
results and report its own generation to the game.

Minimized, fully occluded, and zero-sized AppKit windows currently produce `RenderingSuspended` and
no redraw request. Returning to a drawable state produces `RenderingResumed` with current window
metrics.
This encodes the policy already exercised by the Metal probe; the Wayland explicit-zero-size case and
other compositors may refine the vocabulary before support.

### Borrowed native integration

`Window::surface_target` returns an opaque value borrowed for the window lifetime. It transfers no
retain, release, or destruction authority. The raw AppKit view is reachable only through a hidden
unsafe integration bridge because `mulciber-platform` and `mulciber` are separate crates. Backend code
must not retain the pointer beyond the target, message it from another thread, release it, or replace
platform ownership.

This is backend plumbing, not the intended native escape hatch for games. The safe public graphics
API will accept the opaque target directly.

### Failure and destruction

Creation and event pumping return contextual `PlatformError` values. `Window` owns the retain returned
by `NSWindow` initialization, closes the window, and releases that retain on its creating thread.
Graphics shutdown still occurs explicitly before the probe and window are dropped. Stable recovery
categories remain open until the error model is extracted across both graphics backends.

## Initial validation

On the Apple M2/macOS 15.7.7 development machine, the extracted AppKit path completed:

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle -- --frames 3
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle -- \
  --abandon-acquired-frame-once --frames 120
```

Both processes loaded the existing binary archive with four strict pipeline hits and exited zero with
no Metal validation output beyond the enabled banner. The first submitted three frames. The second
abandoned one acquired drawable, recovered, submitted 120 later frames, and drained retained command
buffers at shutdown. These runs establish finite and targeted abandonment regression coverage through
the extracted platform boundary.

The extracted path then ran interactively without a frame limit. After approximately four minutes
idle, the user physically exercised continuous resize including very small sizes, minimize/restore,
zoom/restore, full occlusion/reveal, and titlebar close. The process submitted 6,504 frames at a
reported 0.917 ms average GPU frame time and exited zero with no Metal validation output beyond the
enabled banner; no visual artifacts or lag were reported. This is single-display development-tree
evidence, not display-change or multi-display coverage, and its console output was not archived.

## Required next evidence

1. Implement the same public types with peer Win32, Wayland, and X11 modules and migrate the Vulkan
   probe without regressing their native event and pacing behavior.
2. Resolve whether full occlusion is a rendering-suspension state or a separate render-policy event
   once the runtime contract is tested.
3. Prove scale/display changes advance window revisions correctly on hardware with the necessary
   displays.
4. Define the graphics-owned presentation generation and replace the hidden AppKit bridge's probe use
   with safe `mulciber` surface creation when the graphics extraction begins.
5. Compare the resulting event and lifecycle flow with direct native stacks, `winit`, SDL3, and the
   other Gate 2 targets in the extraction plan.
