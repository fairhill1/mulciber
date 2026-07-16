# Why Mulciber exists

Mulciber exists to give native Rust games one coherent foundation for modern desktop graphics and
platform lifecycle without hiding Vulkan and Metal behind a lowest-common-denominator contract.

Its intended user is a game or engine team that wants to ship on Windows, Linux, and Apple-silicon
macOS, needs more control than a WebGPU-shaped API provides, and would otherwise have to own separate
Vulkan, Metal, Win32, AppKit, Wayland, and X11 integrations. Mulciber should make that work reusable
without taking away the native capabilities that made the work necessary.

Cross-platform reuse is part of the value proposition, but it cannot be the only value. A team using
Mulciber for only Metal or only Vulkan should still prefer its safe ownership, coordinated lifecycle,
capability model, diagnostics, or predictable machinery for the serious-game slice it supports. The
unused backend must not become a dependency, binary, dispatch, or feature-access tax.

## The problem

Rust already has strong portable graphics and windowing projects. For many games, `wgpu` and `winit`
are the right answer and Mulciber should not pretend otherwise.

Mulciber addresses a narrower problem:

- A browser-compatible graphics model cannot necessarily expose every recent native GPU feature on
  its own terms.
- Graphics, presentation, input, lifecycle, and frame pacing interact, but separate general-purpose
  libraries cannot own the complete game-facing contract.
- Treating portability as identical behavior everywhere can obscure useful backend differences and
  encourage conservative abstractions.
- Owning every native backend inside one game gives maximum control but duplicates difficult work in
  synchronization, presentation, validation, and operating-system lifecycle handling.

Mulciber aims for portability at the game contract rather than uniformity at every backend operation. A
portable path should be convenient, while backend-specific capabilities and escape hatches remain
reachable when they materially improve a game.

## What should be different

### Native capability reach

Vulkan and Metal are primary backends, not implementation details Mulciber tries to erase. Bindless
resource access, mesh shading, ray tracing, sparse resources, GPU-generated work, HDR, and new Metal
features are negotiated independently. A feature does not need an equivalent on every backend to be
useful.

Games declare required and optional capabilities. Required capabilities determine whether a device
can run the game; optional capabilities select better paths with explicit fallbacks. Backend-specific
functionality may be exposed behind a clear boundary instead of being permanently excluded from the
API.

### One game-facing lifecycle

Windows, input, displays, presentation, frame pacing, suspension, and device recovery are parts of
one runtime problem. Mulciber's platform and GPU layers are separate libraries with explicit ownership,
but their contracts are designed and tested together. The eventual runtime coordinates them without
requiring a global framework or taking ownership of unrelated game architecture.

### Intrinsic single-backend value

Mulciber must earn its place separately on Metal and Vulkan. Portability receives no credit in that
evaluation: the comparison asks whether the same game is better served by Mulciber than by a reasonable
direct native Rust stack on one backend. Mulciber should remove unsafe ownership, synchronization,
presentation, resize, and shutdown burden while preserving the backend's useful capabilities and
native validation. It need not replace direct APIs for arbitrary graphics experiments, but it must be
materially preferable for the workload and lifecycle contract it claims.

This criterion also constrains implementation cost. A one-backend build does not initialize or link
the other backend, and ordinary frame work does not pay for runtime dispatch that the supported target
does not need. Dependency, compile-time, binary-size, memory, and performance costs are measured rather
than assumed negligible.

### Predictable machinery

The shipped foundation should be small enough to inspect. Resource ownership, synchronization,
allocation, event delivery, and shutdown policy should be visible rather than emerging from a deep
stack of general-purpose dependencies. Offline tools may be sophisticated when that keeps shader
compilers, binding generators, and reflection machinery out of the game process.

Dependency minimalism serves this goal; dependency count is not a product feature. Mulciber should accept
a focused dependency whenever it removes substantial correctness or maintenance risk without taking
over Mulciber's policy layer.

### Learnable without ecosystem memory

Mulciber starts with a severe familiarity disadvantage: developers and coding models already know
`wgpu` and `winit`, while Mulciber has no accumulated tutorials, answers, or training corpus. A marginally
cleaner API cannot overcome that advantage.

The repository must therefore be sufficient teaching material. Mulciber's public model should be small
enough to explain end to end, names and ownership should be unsurprising, compiler errors should point
to corrective action, and canonical examples should cover complete game tasks rather than isolated
methods. A developer—or an LLM given only the checked-out repository—should not need backend source
spelunking to create a window, render a scene, handle resize, and shut down correctly.

This is not a separate AI-specific API. The properties that make Mulciber legible to an unfamiliar model
also make it legible to an unfamiliar human: local source-of-truth documentation, explicit state,
few hidden conventions, searchable terminology, and examples kept executable by tests.

### Evidence before abstraction

Public APIs are extracted from validated native implementations. Metal/AppKit, Vulkan/Win32,
Vulkan/Wayland, and Vulkan/X11 probes first demonstrate real resource, rendering, presentation, and
failure paths. Once that evidence constrains a narrow shared slice, an explicitly unstable extraction
may begin so its coherence and value can be tested. Stable and first-class claims still require the
remaining physical evidence and viability decisions. The abstraction should encode shared game-facing
needs while preserving important differences, rather than beginning as an idealized API and forcing
the backends underneath it.

### First-class means tested

Platform support is not complete when code compiles or a triangle appears. A first-class backend must
be exercised on physical hardware for resize, minimize and restore, fullscreen and display changes,
suspension, device loss, memory pressure, validation cleanliness, frame pacing, and shutdown. Support
claims should be backed by reproducible capability and validation reports.

## Non-goals

Mulciber is not intended to be:

- A WebGPU implementation or a drop-in replacement for the `wgpu` API.
- A general-purpose GUI or windowing toolkit.
- A compatibility layer for browsers, mobile devices, every operating system, or legacy hardware.
- An engine that dictates scenes, entities, assets, physics, networking, or gameplay architecture.
- The easiest graphics entry point for small applications.
- Dependency-free at the expense of correctness, standards compliance, or maintainability.
- Artificially identical across Vulkan and Metal when their best implementations differ.

Direct3D and Intel Mac support are outside the initial contract. They can be reconsidered from actual
demand and test capacity rather than included speculatively.

## The test for whether Mulciber deserves to exist

Mulciber earns its maintenance cost only if it eventually lets a serious Rust game:

1. Ship across the supported desktop platforms from one stable game-facing contract.
2. Use modern native GPU features and backend-specific escape hatches that would otherwise require
   maintaining custom backends.
3. Achieve predictable performance, frame pacing, ownership, and failure handling under native API
   validation.
4. Keep runtime policy and dependencies narrow enough for an engine team to understand and control.
5. Add a supported platform or capability once in Mulciber instead of rebuilding its lifecycle in every
   game.
6. Be learned from its own documentation and examples faster than familiarity with established
   alternatives can outweigh Mulciber's technical advantages.
7. Remain materially worthwhile for a Metal-only or Vulkan-only game when cross-backend source reuse
   is excluded from the evaluation.

If Mulciber becomes merely a younger, less portable `wgpu`/`winit` combination, it has failed this test.
Its reason to exist is the combination of native capability reach, game-specific lifecycle
coherence, and an evidence-backed support contract. Any one of those in isolation is insufficient.

## Current reality

Mulciber is presently a research foundation, not a consumable game platform. Representative native
Metal and Vulkan workloads now cover owned resources, uploads and readback, graphics and compute,
multiple render passes, synchronization, diagnostics, and native pipeline artifacts. Initial physical
lifecycle evidence exists for AppKit, Win32, and Wayland; X11 has automated XWayland presentation
evidence but still lacks physical lifecycle interaction. Metal acquired-drawable abandonment and
Vulkan presentation retirement have targeted evidence, while the corresponding Vulkan
acquired-but-unpresented behavior remains unresolved.

The evidence is sufficient to begin the narrow unstable extraction defined in the
[API extraction and comparison plan](api-extraction-plan.md). Gate 1 remains incomplete: display and
multi-display changes, native Xorg interaction, broader hardware and drivers, the macOS 26 rendering
path, explicit suspension cases, and destructive recovery such as device loss and out-of-memory remain
coverage gaps. The public `mulciber` and `mulciber-platform` library shells are still empty, and no
stable API or first-class support claim has been made. The [implementation roadmap](roadmap.md) tracks
the extraction and remaining evidence in parallel.
