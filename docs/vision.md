# Why Zinc exists

Zinc exists to give native Rust games one coherent foundation for modern desktop graphics and
platform lifecycle without hiding Vulkan and Metal behind a lowest-common-denominator contract.

Its intended user is a game or engine team that wants to ship on Windows, Linux, and Apple-silicon
macOS, needs more control than a WebGPU-shaped API provides, and would otherwise have to own separate
Vulkan, Metal, Win32, AppKit, Wayland, and X11 integrations. Zinc should make that work reusable
without taking away the native capabilities that made the work necessary.

## The problem

Rust already has strong portable graphics and windowing projects. For many games, `wgpu` and `winit`
are the right answer and Zinc should not pretend otherwise.

Zinc addresses a narrower problem:

- A browser-compatible graphics model cannot necessarily expose every recent native GPU feature on
  its own terms.
- Graphics, presentation, input, lifecycle, and frame pacing interact, but separate general-purpose
  libraries cannot own the complete game-facing contract.
- Treating portability as identical behavior everywhere can obscure useful backend differences and
  encourage conservative abstractions.
- Owning every native backend inside one game gives maximum control but duplicates difficult work in
  synchronization, presentation, validation, and operating-system lifecycle handling.

Zinc aims for portability at the game contract rather than uniformity at every backend operation. A
portable path should be convenient, while backend-specific capabilities and escape hatches remain
reachable when they materially improve a game.

## What should be different

### Native capability reach

Vulkan and Metal are primary backends, not implementation details Zinc tries to erase. Bindless
resource access, mesh shading, ray tracing, sparse resources, GPU-generated work, HDR, and new Metal
features are negotiated independently. A feature does not need an equivalent on every backend to be
useful.

Games declare required and optional capabilities. Required capabilities determine whether a device
can run the game; optional capabilities select better paths with explicit fallbacks. Backend-specific
functionality may be exposed behind a clear boundary instead of being permanently excluded from the
API.

### One game-facing lifecycle

Windows, input, displays, presentation, frame pacing, suspension, and device recovery are parts of
one runtime problem. Zinc's platform and GPU layers are separate libraries with explicit ownership,
but their contracts are designed and tested together. The eventual runtime coordinates them without
requiring a global framework or taking ownership of unrelated game architecture.

### Predictable machinery

The shipped foundation should be small enough to inspect. Resource ownership, synchronization,
allocation, event delivery, and shutdown policy should be visible rather than emerging from a deep
stack of general-purpose dependencies. Offline tools may be sophisticated when that keeps shader
compilers, binding generators, and reflection machinery out of the game process.

Dependency minimalism serves this goal; dependency count is not a product feature. Zinc should accept
a focused dependency whenever it removes substantial correctness or maintenance risk without taking
over Zinc's policy layer.

### Learnable without ecosystem memory

Zinc starts with a severe familiarity disadvantage: developers and coding models already know
`wgpu` and `winit`, while Zinc has no accumulated tutorials, answers, or training corpus. A marginally
cleaner API cannot overcome that advantage.

The repository must therefore be sufficient teaching material. Zinc's public model should be small
enough to explain end to end, names and ownership should be unsurprising, compiler errors should point
to corrective action, and canonical examples should cover complete game tasks rather than isolated
methods. A developer—or an LLM given only the checked-out repository—should not need backend source
spelunking to create a window, render a scene, handle resize, and shut down correctly.

This is not a separate AI-specific API. The properties that make Zinc legible to an unfamiliar model
also make it legible to an unfamiliar human: local source-of-truth documentation, explicit state,
few hidden conventions, searchable terminology, and examples kept executable by tests.

### Evidence before abstraction

Public APIs are extracted from validated native implementations. Metal/AppKit, Vulkan/Win32,
Vulkan/Wayland, and Vulkan/X11 probes must first demonstrate real resource, rendering, presentation,
and failure paths. The abstraction should encode their shared game-facing needs while preserving
important differences, rather than beginning as an idealized API and forcing the backends underneath
it.

### First-class means tested

Platform support is not complete when code compiles or a triangle appears. A first-class backend must
be exercised on physical hardware for resize, minimize and restore, fullscreen and display changes,
suspension, device loss, memory pressure, validation cleanliness, frame pacing, and shutdown. Support
claims should be backed by reproducible capability and validation reports.

## Non-goals

Zinc is not intended to be:

- A WebGPU implementation or a drop-in replacement for the `wgpu` API.
- A general-purpose GUI or windowing toolkit.
- A compatibility layer for browsers, mobile devices, every operating system, or legacy hardware.
- An engine that dictates scenes, entities, assets, physics, networking, or gameplay architecture.
- The easiest graphics entry point for small applications.
- Dependency-free at the expense of correctness, standards compliance, or maintainability.
- Artificially identical across Vulkan and Metal when their best implementations differ.

Direct3D and Intel Mac support are outside the initial contract. They can be reconsidered from actual
demand and test capacity rather than included speculatively.

## The test for whether Zinc deserves to exist

Zinc earns its maintenance cost only if it eventually lets a serious Rust game:

1. Ship across the supported desktop platforms from one stable game-facing contract.
2. Use modern native GPU features and backend-specific escape hatches that would otherwise require
   maintaining custom backends.
3. Achieve predictable performance, frame pacing, ownership, and failure handling under native API
   validation.
4. Keep runtime policy and dependencies narrow enough for an engine team to understand and control.
5. Add a supported platform or capability once in Zinc instead of rebuilding its lifecycle in every
   game.
6. Be learned from its own documentation and examples faster than familiarity with established
   alternatives can outweigh Zinc's technical advantages.

If Zinc becomes merely a younger, less portable `wgpu`/`winit` combination, it has failed this test.
Its reason to exist is the combination of native capability reach, game-specific lifecycle
coherence, and an evidence-backed support contract. Any one of those in isolation is insufficient.

## Current reality

Zinc is presently a research foundation, not a consumable game platform. The Metal probes establish
substantial rendering and lifecycle evidence, and the initial Vulkan/Win32 presentation path has
been physically exercised on one Windows 11 and Nvidia hardware tier. Its rendering resumes at the
new size after a resize drag but does not update during the drag. Wayland, X11, the documented
Windows baseline tier, and a representative Vulkan workload still need comparable evidence. The
public `zinc-gpu` and `zinc-platform` APIs remain empty until that evidence exists. The
[implementation roadmap](roadmap.md) tracks that progression.
