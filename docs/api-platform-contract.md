# Experimental platform and window contract

This document records the first API extraction from the native probes. The types and names are
unstable and exist to test Gate 2; they are not a supported platform claim. The implementation began
from revision `449c01cb1997fedd674a4a58bd0105f141a3317b` and was initially exercised through the
AppKit/Metal probe. Peer Win32, Wayland, and X11 implementations now drive the Vulkan probes through
the same contract. The Win32 extraction still requires physical validation before this candidate
contract can be judged coherent across all four native window systems.

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

The Metal probe no longer creates or polls `NSApplication`, `NSWindow`, or `NSView` directly. The
Vulkan probes no longer create or pump Win32, Wayland/XDG-shell, or Xlib windows directly. Each probe
retains its graphics API ownership and consumes a borrowed platform surface target. This is an
intentional intermediate boundary: platform lifecycle is extracted before GPU resource and command
APIs, while the full validated workloads continue to exercise it.

## Decisions established by this slice

### Main-thread ownership

On macOS, `Application::new` verifies the process main thread before connecting to AppKit. On Linux
and Windows, application and window objects are confined to their creating thread; Linux's native
display connection is currently established with the window because both proven implementations own
one connection per window. `Application`, `Window`, and the borrowed surface target are intentionally
neither `Send` nor `Sync`. This makes native-thread ownership structural rather than a comment that
ordinary application code can accidentally violate.

### Game-owned loop with native event pumping

The game calls `Application::pump_events` and receives translated events through a callback. This
keeps the game in control of its architecture while leaving room for a native backend to invoke redraw
during nested or modal event processing. AppKit, Win32, Wayland, and X11 emit `RedrawRequested` after
queued events are dispatched whenever the surface is drawable. Win32 temporarily registers the
handler while dispatching messages, allowing `WM_SIZE` and a window timer to deliver redraw requests
synchronously inside its nested interactive-sizing loop. Its Vulkan adapter renders only those
nested requests; normal frame work remains in the probe's outer loop. Linux currently uses the pump's
continue/exit result and then reads current metrics, intentionally leaving fully event-driven render
coordination for the graphics extraction. The Win32 callback shape is structurally preserved but
still requires physical regression evidence through the extracted boundary.

The metrics carried by `RedrawRequested` are the authoritative input for that render opportunity. The
Metal probe consumes them directly rather than querying the window a second time after event delivery.

This is not yet a commitment that polling is the final runtime API. Gate 5 may add an owning runtime
loop above this layer, but it must not invalidate the lower-level game-controlled path without a
written comparison.

The current AppKit, Win32, and Linux slices permit exactly one live `Window` per `Application`.
AppKit's event queue is process-wide, Win32 messages are thread-wide, and the initial Linux
extraction preserves each probe's one-window connection topology; accepting multiple windows here
would silently assert an event-routing and connection-ownership model that has not been designed.
Dropping the window releases the slot so another can be created. Multi-window support remains a
deliberate later design step that must introduce application-level window identity and event routing
rather than pretending the present callback is already sufficient.

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

Minimized, hidden/ordered-out, fully occluded, and zero-sized AppKit windows currently produce
`RenderingSuspended` and no redraw request. Returning to a drawable state produces
`RenderingResumed` with current window metrics. AppKit delegate callbacks track an actual close
request separately, so temporary invisibility is not interpreted as termination.
This encodes the policy already exercised by the Metal probe; the Wayland explicit-zero-size case and
other compositors may refine the vocabulary before support.

Win32, Wayland, and X11 translate their proven native client extent into the shared extent with scale
factor `1.0`. They advance the same revision type on extent changes, but per-monitor DPI, scale,
display-change, fractional-scaling, and explicit Wayland zero-sized-suspension behavior remain
pending evidence and must not be inferred from the current value.

### Borrowed native integration

`Window::surface_target` returns an opaque value borrowed for the window lifetime. It transfers no
retain, release, or destruction authority. Raw AppKit, Win32, Wayland, and Xlib handles are reachable
only through hidden unsafe integration bridges because `mulciber-platform` and the graphics consumers
are separate crates. Backend code must not retain handles beyond the source window, use them from
another thread, destroy them, or replace platform ownership.

This is backend plumbing, not the intended native escape hatch for games. The safe public graphics
API will accept the opaque target directly.

### Failure and destruction

Creation and event pumping return contextual `PlatformError` values. The AppKit `Window` owns the
retain returned by `NSWindow` initialization and an owned delegate whose non-owning close-state
association remains bounded by the window. Destruction detaches and releases the delegate, closes the
window, and releases the window retain on its creating thread. Graphics shutdown still occurs
explicitly before the probe and window are dropped. Stable recovery categories remain open until the
error model is extracted across both graphics backends. The Wayland implementation destroys protocol
roles child-first before disconnecting its display. The X11 implementation destroys its sync counter
and window before closing the display. Win32 destroys its window before unregistering its unique
class. Existing GPU/presentation shutdown still occurs before each platform window drops.

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

A later development tree based on the extracted revision replaced visibility-based closure detection
with delegate-backed close tracking and made event-delivered metrics authoritative in the Metal
consumer. A three-frame validation-enabled smoke run loaded four strict binary-archive hits, reported
0.879 ms average GPU frame time, and exited zero. A targeted run then abandoned one acquired drawable,
recovered, submitted 120 later frames at 0.951 ms average GPU frame time, and exited zero. Both runs
loaded four strict hits and emitted no Metal validation output beyond the enabled banner. They validate
construction, rendering, and the exceptional non-submission path after the change; they are not
physical hide/restore or titlebar-close evidence for the new delegate path.

On 2026-07-17, an uncommitted development tree based on `e573d68` moved the existing Wayland and X11
window implementations into `mulciber-platform`. On a native KDE Wayland session with an Nvidia RTX
3060 Ti, explicit Wayland and X11-through-XWayland runs each presented 120 frames, emitted no Vulkan
validation warning/error callbacks, and exited zero. These finite runs validate construction, event
pumping, borrowed-handle surface creation, presentation, and shutdown through the new boundary. They
do not repeat or broaden the previously recorded physical lifecycle, resize, display, or visual
evidence.

The development tree after `mulciber-platform` 0.1.0 then moved Win32 window and event ownership into
the platform crate. Both Vulkan consumers and the platform tests compile and lint cleanly for
`x86_64-pc-windows-msvc` from Linux. This proves target structure and Rust/Win32 ABI compilation only;
it does not establish construction, nested resize callbacks, Vulkan presentation, lifecycle, or
shutdown until the physical Windows validation below is completed.

## Required next evidence

1. Run the extracted Win32 path through the automated validation matrix and physically repeat nested
   live resize, minimize/restore, maximize/restore, titlebar close, and Alt+F4 shutdown.
2. Resolve whether full occlusion is a rendering-suspension state or a separate render-policy event
   once the runtime contract is tested.
3. Prove scale/display changes advance window revisions correctly on hardware with the necessary
   displays.
4. Physically repeat hide/restore and titlebar close after the delegate-backed close-tracking change.
5. Define the graphics-owned presentation generation and replace the hidden AppKit bridge's probe use
   with safe `mulciber` surface creation when the graphics extraction begins.
6. Compare the resulting event and lifecycle flow with direct native stacks, `winit`, SDL3, and the
   other Gate 2 targets in the extraction plan.
