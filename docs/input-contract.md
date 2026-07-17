# Experimental input-transition contract

This document records the first native input evidence added to `mulciber-platform`. The contract is
an unstable AppKit/Win32 experiment, not a stable cross-platform support claim and not the
input-snapshot API planned for `mulciber-runtime`.

## Scope

The first slice delivers gameplay-oriented transitions through the existing fallible
`Application::pump_events` callback:

- physical keyboard key press, release, and repeat;
- aggregate Shift, Control, Alt/Option, Command/Super, Caps Lock, and Function modifiers;
- pointer motion in top-left-origin logical client coordinates;
- primary, secondary, middle, and numbered extra pointer buttons;
- precise trackpad and coarse wheel scroll deltas without collapsing their units; and
- keyboard-focus gain and loss.

Text entry, dead keys, keyboard-layout output, IME composition, gestures, pressure, gamepads,
relative-pointer motion, cursor confinement, and cursor visibility are outside this slice. They have
different lifecycle and capability requirements and must not be inferred from physical key or
ordinary pointer evidence.

## Event delivery and ownership

Input remains part of the game-owned platform pump. Native events are translated and delivered in
queue order before the pump's final `RedrawRequested`, so state changed by input is visible to that
render opportunity. If an application handler fails, native dispatch still advances while the
remaining translated events from that pump call are dropped, preserving the platform contract's
existing error semantics.

`mulciber-platform` delivers transitions; it does not retain a public pressed-key set or decide when
a game simulation tick begins. A future `mulciber-runtime` may consume these events into per-update
held/pressed/released snapshots, but that policy must remain above the lower-level game-controlled
path.

Pointer positions are `LogicalPosition` values relative to the content area's top-left origin.
Drawable sizes remain physical pixels in `WindowMetrics`; applications can use the reported scale
factor when those coordinate spaces must meet. A button pressed inside the content area keeps pointer
delivery captured until its matching release, including when the pointer leaves the content area.
Focus loss clears that internal pointer capture and tells consumers to invalidate any held-state
snapshot they maintain.

Physical `KeyCode` values describe key positions rather than text. An unrecognized backend key code
is preserved as `KeyCode::Unidentified` for diagnostics without treating its numeric value as
portable.

## AppKit implementation checkpoint

The AppKit backend inspects each `NSEvent` from the same queue it already owns, forwards it to
`NSApplication`, then emits a translated `WindowEvent::Input` for the owned window. Mouse motion is
enabled explicitly on the window. The existing delegate also records key-window transitions so focus
changes remain tied to the owned window and creating main thread.

The separate `mulciber-input-cube` example dogfoods the candidate contract: W/A/S/D and arrow key
transitions rotate the cube, primary-button dragging orbits it, scrolling zooms, Space toggles
automatic spin (initially paused), and R resets the interaction offsets. The minimal graphics-only
`mulciber-cube` remains unchanged for the existing Gate
2 comparison. `comparisons/wgpu-input-cube` implements the same input behavior and transform math
through ordinary `winit` events, while the original `wgpu-cube` also remains unchanged. The input
example constructs no snapshot layer; every visible change follows directly from a transition
delivered before redraw.

## Win32 implementation checkpoint

The Win32 backend translates physical scan codes from `WM_KEYDOWN`, `WM_KEYUP`, and their system-key
peers rather than treating keyboard-layout text as key identity. Modifier state comes from Win32's
thread-local key state. `TranslateMessage` still runs as part of ordinary native dispatch, but the
resulting `WM_CHAR` and `WM_SYSCHAR` messages are consumed because text entry, menu mnemonics, and IME
composition are outside this render-window slice; this also prevents the default OS beep. Alt+F4 is
explicitly preserved through `DefWindowProcW` so native close behavior remains intact.

Pointer messages use signed top-left client coordinates. Button presses acquire Win32 pointer
capture until every pressed button is released; capture loss synthesizes the missing releases for
the public event stream. Wheel messages convert their screen-relative coordinates to the client
space and retain coarse wheel-step units. Focus changes are retained even when they occur between
pump callbacks, and focus loss clears internal pointer capture.

## Evidence and next pressure tests

Unit tests cover the AppKit physical-key table, modifier translation, extra pointer-button identity,
focus delegate state, Win32 scan-code navigation/numpad distinctions, signed pointer coordinates,
extended-button identity, and existing lifecycle behavior. On 2026-07-18, the combined showcase was
physically exercised on Windows 11 / Intel UHD 620: W/A/S/D and arrow rotation, Space pause/resume,
R reset, two-axis primary drag, wheel zoom, resize, and title-bar close behaved correctly, key input
produced no default OS beep, and the process exited zero. That focused pass did not exercise repeats,
modifier transitions, outside-window release, focus loss/reacquisition, minimize/restore,
maximize/restore, or multi-display behavior.

Before stabilizing names or snapshot behavior:

1. complete the remaining Win32 pressure tests listed above;
2. implement Wayland keyboard/pointer support with explicit keymap ownership and compositor protocol
   behavior rather than assuming AppKit key identities or pointer capture;
3. implement and exercise the X11 peer, preserving its different focus and event-selection rules;
4. compare event loss, repeat, focus invalidation, coordinate spaces, wheel/trackpad units, and
   application ergonomics with the equivalent `wgpu-input-cube`, direct native stacks, and SDL3;
   and
5. build snapshots only as part of the Gate 5 runtime dogfood slice.
