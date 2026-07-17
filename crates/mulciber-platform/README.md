# Mulciber Platform

`mulciber-platform` is the experimental native desktop platform layer for the Mulciber game
platform. It owns native application connections, windows, event pumping, drawable metrics, and
borrowed graphics surface targets without imposing a cross-platform windowing framework.

Version 0.1.0 contains peer native AppKit, Wayland, and X11 implementations exercised by Mulciber's
Metal and Vulkan probes. The current development tree adds the peer Win32 implementation, pending
physical validation and a later release. Input, display enumeration, multi-window support, and a
stable application-facing API remain in progress. The current API is research-stage and may change
without compatibility guarantees. On docs.rs, use the platform selector to view the implementation
available for each target operating system.

Development, design contracts, runnable probes, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
