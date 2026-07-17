# Mulciber desktop OS layer

`mulciber-platform` is the experimental desktop OS layer of the Mulciber native game-development
stack. It owns native application connections, windows, event pumping, drawable metrics, and
borrowed graphics surface targets without imposing a cross-platform windowing framework.

Version 0.2.0 contains peer native AppKit, Win32, Wayland, and X11 implementations exercised by
Mulciber's Metal and Vulkan probes. New in this version, the event pump takes a fallible handler
and returns the first application error once native dispatch completes (a breaking change from
0.1), and the platform owns the startup wait for first drawable metrics. Input, display
enumeration, multi-window support, and a stable application-facing API remain in progress. The current API is research-stage and may change without
compatibility guarantees. On docs.rs, use the target selector to view the implementation available
for each operating system.

Development, design contracts, runnable probes, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
