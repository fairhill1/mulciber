# Mulciber desktop OS layer

`mulciber-platform` is the experimental desktop OS layer of the Mulciber native game-development
stack. It owns native application connections, windows, event pumping, drawable metrics, and
borrowed graphics surface targets without imposing a cross-platform windowing framework.

Version 0.4.2 contains peer native AppKit, Win32, Wayland, and X11 implementations exercised by
Mulciber's Metal and Vulkan probes. New in this version, the Win32 backend implements pointer
capture behind the unchanged 0.4 `Window::set_cursor_mode` intent — raw-input `WM_INPUT` deltas,
`ClipCursor` confinement, `WM_SETCURSOR` hiding, client-center pinning, and focus-loss release
with refocus reapply — completing the intent on all four backends with no public API change.
The AppKit, Wayland, and X11 implementations are physically verified for relative mouse-look;
the Win32 implementation is cross-compiled and linted from Linux only and has not yet executed
on a Windows machine, so treat it as untested until the recorded validation lands.
Display enumeration, multi-window support, and a stable application-facing API remain in
progress. The current API is research-stage and may change without compatibility guarantees. On
docs.rs, use the target selector to view the implementation available for each operating
system.

Development, design contracts, runnable probes, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
