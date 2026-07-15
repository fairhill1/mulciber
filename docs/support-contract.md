# Initial support contract

This contract defines the intended minimum. It will be revised only from hardware evidence collected
by the native probes.

## Windows

- Windows 10 or 11 on x86-64.
- Vulkan 1.4 from the installed GPU vendor driver.
- Nvidia Pascal / GeForce GTX 1060-class hardware remains in the baseline when using a current
  conformant driver.
- Win32 owns windows, input, display enumeration, and the event loop.
- Direct3D is not an initial backend.

## Linux

- Linux on x86-64.
- Vulkan 1.4 from a conformant proprietary or Mesa driver.
- Wayland with XDG shell is first class.
- X11 is a separate first-class platform backend, not a Wayland compatibility assumption.

## macOS

- macOS 13 or newer on Apple silicon.
- Metal 3 is the initial compatibility baseline.
- Metal 4 is enabled only when both the build SDK and runtime device support it.
- AppKit owns windows, input, display enumeration, and the event loop.
- MoltenVK is not a backend.

Intel Mac support is not part of the initial contract. It may be reconsidered from demand and test
hardware availability.

## Capability model

The baseline must support conventional rasterization, compute, texture compression appropriate to
each platform, explicit synchronization, GPU timestamps, and predictable presentation.

Advanced features are negotiated independently because hardware support is not a strict hierarchy:

- Descriptor indexing and bindless resource tables.
- Mesh and task/object shaders.
- Hardware ray tracing.
- Sparse resources.
- Variable-rate shading.
- HDR and advanced presentation timing.
- GPU-driven indirect command generation.

A game declares required and optional capabilities. Startup rejects a device only when a required
capability is absent; optional systems select a fallback path.

## Quality bar

First-class support means more than compilation. Every backend must be tested for resize, minimize,
fullscreen transitions, display changes, suspend/resume, device loss, out-of-memory behavior,
validation cleanliness, frame pacing, and clean shutdown.
