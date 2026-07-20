# Experimental input-transition contract

This document records the first native input evidence added to `mulciber-platform`. The contract is
an unstable experiment across the AppKit, Win32, Wayland, and X11 backends, not a stable
cross-platform support claim and not the input-snapshot API planned for `mulciber-runtime`.

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

## Pointer capture and cursor modes

`Window::set_cursor_mode` accepts an application intent, `CursorMode::Normal` or
`CursorMode::Captured`, and the platform owns the native policy behind it. This exists because the
[consumer evidence](consumer-evidence.md) shows all five surveyed games hand-rolling the same
locked-versus-confined fallback, cursor hiding, and focus bookkeeping above `winit`.

While capture is applied, the cursor is hidden and pinned inside the window, and motion arrives as
`InputEvent::PointerDelta` (logical units, positive y down) instead of absolute `PointerMoved`
positions. The requested mode is an intent that survives focus changes: the platform releases the
native capture and restores the cursor on focus loss, reapplies it on focus gain, and always
restores the system cursor when the window drops, so no error path can strand a hidden or detached
cursor. Requesting `Normal` succeeds on every backend so portable release paths stay uniform;
requesting `Captured` on a backend without an implementation reports an `Unsupported` platform
error rather than pretending.

Backend status: the AppKit implementation pins the cursor by warping it to the content-view center,
detaching cursor movement with `CGAssociateMouseAndMouseCursorPosition`, and hiding it with
`NSCursor`, reporting `NSEvent` deltas during capture. The Wayland implementation locks the pointer
through `zwp_pointer_constraints_v1` with the persistent lifetime (the compositor itself suspends
and re-establishes the lock across focus changes), reads deltas from
`zwp_relative_pointer_manager_v1`, hides the cursor with a null `wl_pointer.set_cursor`, and
restores it through `wp_cursor_shape_manager_v1`; when the compositor lacks any of the three
protocols, the capture request reports `Unsupported` naming the missing global. The X11
implementation grabs the pointer confined to the window with a fully transparent pixmap cursor and
reports warp-to-center deltas, filtering the warp's own echo motion; the grab is released on focus
loss and best-effort reapplied on focus gain. The Win32 implementation registers the window for
raw mouse input and reports `WM_INPUT` motion as deltas (absolute-mode samples, as remote
desktop delivers, are differenced against the previous sample), confines the hidden cursor to
the screen-space client rectangle with `ClipCursor` re-derived on move and resize, hides it
through `WM_SETCURSOR` over the captured client area, and keeps it pinned to the client center
with warp-echo filtering, releasing on focus loss and best-effort reapplying on focus gain like
the X11 peer. It cross-compiles and lints cleanly for `x86_64-pc-windows-msvc` from Linux and is
otherwise completely unexercised: no Windows machine has executed this code.

`mulciber-input-cube` dogfoods the contract (C toggles capture into relative cube look, Escape
releases), and `comparisons/wgpu-input-cube` implements the equivalent behavior the surveyed way:
the `CursorGrabMode::Locked` then `Confined` fallback, manual visibility and focus suspension
bookkeeping, and `DeviceEvent::MouseMotion` deltas. On 2026-07-19 an agent-driven AppKit smoke run
exercised capture, Escape release, recapture, and close-while-captured with no native errors and a
restored cursor, and the operator then physically verified relative mouse-look capture on the
Apple M2 tier; both records live in the [macOS runbook](macos-validation.md). On 2026-07-20 an
agent-driven session recorded automated Wayland and X11 capture evidence — including an
XTEST-driven X11 run whose pointer stayed pinned at the measured content center through relative
motion and moved freely after Escape — in the [Linux runbook](linux-validation.md). Later that day
the operator physically verified capture on both Linux paths at committed `3075d0e`: relative look
with a hidden, escape-proof pointer, Escape restoring the cursor, Alt-Tab releasing the capture
cleanly, and Wayland window teardown from the captured state. The Win32 implementation has no
execution evidence of any kind; its first Windows session must treat it as untested code.

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

Pointer capture landed 2026-07-20 and is implemented but unexercised: `CursorMode::Captured`
registers raw mouse input targeted at the window, clips the cursor to the screen-space client
rectangle, hides it via `WM_SETCURSOR`, recenters it against absolute `WM_MOUSEMOVE` positions
with warp-echo filtering, and emits `PointerDelta` from `WM_INPUT` motion. Focus loss releases
the clip and raw-input registration while preserving the stored intent; window teardown releases
the process-global clip unconditionally, even after external native destruction. The raw-delta
differencing (relative and absolute modes) is covered by unit tests that have compiled for the
msvc target but never run on Windows. No part of this path has executed on a Windows machine.

## Wayland implementation checkpoint

The Wayland backend binds `wl_seat` capped at protocol version five and owns peer `wl_keyboard`
and `wl_pointer` proxies that follow the seat's capability announcements. Key identity comes from
the evdev codes the keyboard delivers, through a translation table shared with the X11 backend.
The keymap the compositor sends is mapped privately and parsed with libxkbcommon — the platform's
canonical xkb parser and the contract's "explicit keymap ownership" — solely to resolve which mask
bits carry Shift, Lock, Control, Mod1, and Mod4; aggregate modifier state then follows the
compositor's `modifiers` events rather than being inferred from key transitions.

Wayland leaves key repeat to the client. The backend synthesizes at most one repeat transition per
pump for the most recently held key, self-paced against the seat's `repeat_info` rate and delay;
game pumps run at display rate, above every sane repeat rate, so no burst catch-up path exists.
Pointer frames batch axis events: finger and continuous sources report precise logical deltas,
wheel sources report coarse steps from discrete counts (falling back to the conventional
fifteen-units-per-detent division), and the vertical axis is flipped so wheel-forward stays
positive across backends. Keyboard enter/leave is the focus signal; leave clears repeat state and
modifier state. Translated events queue during native dispatch and drain through the shared Linux
pump in queue order before the final redraw opportunity.

## X11 implementation checkpoint

The X11 backend extends the existing event selection with key, button, motion, and focus-change
masks, translating keycodes through the same evdev table after removing the fixed offset of
eight. Modifier state uses the core Shift/Lock/Control masks plus the universal Mod1-as-Alt and
Mod4-as-Super convention from event state masks, with a live `XQueryPointer` query on modifier-key
transitions because an X event's state field predates its own transition. Detectable auto-repeat
is requested through Xkb so held-key repeats arrive as consecutive presses and are flagged from an
internal pressed-key set; servers without it degrade to visible release/press pairs rather than
misreported repeats. Core scroll buttons four through seven become coarse scroll steps on press,
extended buttons eight and nine map to the same `Other(3)`/`Other(4)` identities as Win32, and
grab-mode focus excursions (window-manager keyboard grabs) are filtered from focus transitions.
Titles are additionally published as UTF-8 `_NET_WM_NAME`, and a window destroyed by the server is
never destroyed again during drop, which Xlib would treat as fatal.

## Evidence and next pressure tests

Unit tests cover the AppKit physical-key table, modifier translation, extra pointer-button identity,
focus delegate state, Win32 scan-code navigation/numpad distinctions, signed pointer coordinates,
extended-button identity, and existing lifecycle behavior. On 2026-07-18, the combined showcase was
physically exercised on Windows 11 / Intel UHD 620: W/A/S/D and arrow rotation, Space pause/resume,
R reset, two-axis primary drag, wheel zoom, resize, and title-bar close behaved correctly, key input
produced no default OS beep, and the process exited zero. That focused pass did not exercise repeats,
modifier transitions, outside-window release, focus loss/reacquisition, minimize/restore,
maximize/restore, or multi-display behavior.

Unit tests additionally cover the shared evdev translation table, xkb modifier-index mask
computation, Wayland axis-frame precise/coarse folding, X11 core-mask translation, and both
backends' button identities. The 2026-07-20 agent-driven Linux runs recorded in the
[Linux runbook](linux-validation.md) exercised the full X11 pipeline through XTEST (keys, drag,
wheel, capture, warp-pinned deltas, release, both close paths) and the Wayland capture protocol
against live KWin. The same day's physical human session at committed `3075d0e` (recorded in the
same runbook) then exercised typed transitions, held-key repeat through both repeat paths, drag,
wheel, capture feel, focus-loss clearing, lifecycle, and both close paths on Wayland and X11;
modifier transitions, trackpad precise-scroll units, and repeat cadence measured against the
configured rate remain unexercised on either path.

Before stabilizing names or snapshot behavior:

1. complete the remaining Win32 pressure tests listed above;
2. complete the remaining Wayland and X11 physical coverage (modifier transitions, trackpad
   versus wheel units, repeats against the configured cadence) on the KDE tier, then repeat the
   slices on a non-KDE compositor and native Xorg;
3. physically exercise the implemented Win32 pointer capture (engage, relative look, Escape
   release, focus-loss release with refocus reapply, teardown while captured, and the
   absolute-mode delta path that remote desktop exercises) so the capture contract has evidence
   on all four backends;
4. compare event loss, repeat, focus invalidation, coordinate spaces, wheel/trackpad units, and
   application ergonomics with the equivalent `wgpu-input-cube`, direct native stacks, and SDL3;
   and
5. build snapshots only as part of the Gate 5 runtime dogfood slice.
