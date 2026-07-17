# Mulciber Platform

`mulciber-platform` is the experimental native desktop platform layer for the Mulciber game
platform. It owns native application connections, windows, event pumping, drawable metrics, and
borrowed graphics surface targets without imposing a cross-platform windowing framework.

Version 0.1.0 contains peer native AppKit, Wayland, and X11 implementations exercised by Mulciber's
Metal and Vulkan probes. Win32 extraction, input, display enumeration, multi-window support, and a
stable application-facing API remain in progress. The current API is research-stage and may change
without compatibility guarantees.

Development, design contracts, runnable probes, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
