# Experimental input-transition contract

This document records the first native input evidence added to `mulciber-platform`. The contract is
an unstable AppKit-first experiment, not a cross-platform support claim and not the input-snapshot
API planned for `mulciber-runtime`.

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

Input remains part of the game-owned platform pump. AppKit events are translated and delivered in
native queue order before the pump's final `RedrawRequested`, so state changed by input is visible to
that render opportunity. If an application handler fails, AppKit dispatch still advances while the
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

Physical `KeyCode` values describe key positions rather than text. An unrecognized AppKit key code is
preserved as `KeyCode::Unidentified` for diagnostics without treating its numeric value as portable.

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

## Evidence and next pressure tests

Unit tests cover the AppKit physical-key table, modifier translation, extra pointer-button identity,
focus delegate state, and existing lifecycle behavior. Physical evidence must exercise key presses
and repeats, modifiers, primary drag including an outside-window release, coarse or precise scroll,
focus loss/reacquisition, resize, minimize/restore, and titlebar close in one captured session.

No shared input claim follows from AppKit alone. Before stabilizing names or snapshot behavior:

1. implement and physically exercise Win32 transitions;
2. implement Wayland keyboard/pointer support with explicit keymap ownership and compositor protocol
   behavior rather than assuming AppKit key identities or pointer capture;
3. implement and exercise the X11 peer, preserving its different focus and event-selection rules;
4. compare event loss, repeat, focus invalidation, coordinate spaces, wheel/trackpad units, and
   application ergonomics with the equivalent `wgpu-input-cube`, direct native stacks, and SDL3;
   and
5. build snapshots only as part of the Gate 5 runtime dogfood slice.
