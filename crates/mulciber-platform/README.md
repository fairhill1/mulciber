# Mulciber desktop OS layer

`mulciber-platform` is the experimental desktop OS layer of the Mulciber native game-development
stack. It owns native application connections, windows, event pumping, drawable metrics, and
borrowed graphics surface targets without imposing a cross-platform windowing framework.

Version 0.4.1 contains peer native AppKit, Win32, Wayland, and X11 implementations exercised by
Mulciber's Metal and Vulkan probes. New in this version, the Wayland and X11 backends implement
keyboard, pointer, scroll, and focus input through one shared evdev key table, and pointer
capture behind the 0.4.0 `Window::set_cursor_mode` intent — Wayland through pointer constraints
with relative-pointer deltas, X11 through a confined grab with warp-to-center deltas — with no
public API change. The AppKit implementation is physically verified for relative mouse-look;
Win32 still reports an explicit unsupported error for capture until its tier is exercised.
Display enumeration, multi-window support, and a stable application-facing API remain in
progress. The current API is research-stage and may change without compatibility guarantees. On
docs.rs, use the target selector to view the implementation available for each operating
system.

Development, design contracts, runnable probes, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
