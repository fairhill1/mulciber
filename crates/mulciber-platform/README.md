# Mulciber desktop OS layer

`mulciber-platform` is the experimental desktop OS layer of the Mulciber native game-development
stack. It owns native application connections, windows, event pumping, drawable metrics, and
borrowed graphics surface targets without imposing a cross-platform windowing framework.

Version 0.5.0 contains peer native AppKit, Win32, Wayland, and X11 implementations exercised by
Mulciber's Metal and Vulkan probes. New in this version, `Window::set_window_mode` and
`Window::window_mode` add a Windowed/Fullscreen intent on all four backends — borderless or
native fullscreen on the window's current display, never an exclusive mode — with the reported
mode following window-system-confirmed transitions so an application toggle stays correct when
the window system enters or leaves fullscreen on its own. The Wayland and X11 fullscreen paths
have a tool-automated Linux checkpoint (the native Wayland request path is unit-tested only);
the AppKit path is unvalidated, and the Win32 backend — pointer capture and fullscreen alike —
is cross-compiled and linted from Linux only and has not yet executed on a Windows machine, so
treat it as untested until the recorded validation lands.
Display enumeration, multi-window support, and a stable application-facing API remain in
progress. The current API is research-stage and may change without compatibility guarantees. On
docs.rs, use the target selector to view the implementation available for each operating
system.

Development, design contracts, runnable probes, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
