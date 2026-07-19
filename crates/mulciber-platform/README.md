# Mulciber desktop OS layer

`mulciber-platform` is the experimental desktop OS layer of the Mulciber native game-development
stack. It owns native application connections, windows, event pumping, drawable metrics, and
borrowed graphics surface targets without imposing a cross-platform windowing framework.

Version 0.4.0 contains peer native AppKit, Win32, Wayland, and X11 implementations exercised by
Mulciber's Metal and Vulkan probes. New in this version, `Window::set_cursor_mode` expresses a
pointer-capture intent the platform owns: while capture is applied the cursor is hidden and pinned,
motion arrives as relative `PointerDelta` input events, the intent survives focus loss and
reapplies on focus gain, and dropping the window always restores the system cursor. The AppKit
implementation is physically verified for relative mouse-look; Win32, Wayland, and X11 report an
explicit unsupported error for capture until their tiers are exercised. Wayland and X11 input
implementations remain in progress, as do display enumeration, multi-window support, and a stable
application-facing API. The current API is research-stage and may change without compatibility
guarantees. On docs.rs, use the target selector to view the implementation available for each
operating system.

Development, design contracts, runnable probes, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
