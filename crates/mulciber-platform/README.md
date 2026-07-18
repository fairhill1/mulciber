# Mulciber desktop OS layer

`mulciber-platform` is the experimental desktop OS layer of the Mulciber native game-development
stack. It owns native application connections, windows, event pumping, drawable metrics, and
borrowed graphics surface targets without imposing a cross-platform windowing framework.

Version 0.3.0 contains peer native AppKit, Win32, Wayland, and X11 implementations exercised by
Mulciber's Metal and Vulkan probes. New in this version, AppKit and Win32 deliver ordered
physical-key, modifier, pointer, button, scroll, and focus input transitions through the fallible
event pump (a breaking `WindowEvent` addition since 0.2), and platform errors expose
recovery-oriented kinds alongside their contextual messages. Wayland and X11 input implementations
remain in progress, as do display enumeration, multi-window support, and a stable
application-facing API. The current API is research-stage and may change without compatibility
guarantees. On docs.rs, use the target selector to view the implementation available for each
operating system.

Development, design contracts, runnable probes, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
