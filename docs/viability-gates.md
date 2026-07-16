# Mulciber viability gates

Mulciber is an experiment until evidence shows that its advantages justify owning native GPU and
platform backends. These gates prevent implementation momentum, dependency minimalism, or attachment
to the project from becoming substitutes for product value.

A gate is passed only by checked-in code, reproducible validation evidence, and a written decision.
If a gate fails, the default action is to stop, narrow, or repurpose Mulciber—not to move the goalposts.

The gates distinguish an **experimental extraction** from a **supported public contract**. An unstable
API slice may be built once native evidence has constrained its design enough to test Gate 2. That
permission is not a Gate 1 pass, a stability promise, or a first-class platform claim. Stable public
claims require the applicable gates to pass in full.

## Gate 1: native backend credibility

### Entry to experimental extraction

An unstable API extraction may begin when:

- Representative native Metal and Vulkan workloads run cleanly with validation and exercise the
  resource, pipeline, synchronization, and presentation behavior the first slice will expose.
- Win32, AppKit, Wayland, and X11 implementations exist as peer native paths and have at least initial
  execution evidence on physical machines, with the exact interaction and display coverage recorded.
- Candidate shared invariants and unresolved native differences are recorded in
  [the backend contract ledger](backend-contracts.md).
- The extraction is scoped as a falsifiable experiment with a written comparison and stop plan rather
  than treated as inevitable product progress.

Mulciber has reached this entry threshold for the narrow slice in the
[API extraction and comparison plan](api-extraction-plan.md). Physical coverage gaps remain explicit,
so this is not a Gate 1 completion decision.

### Completion conditions

Before claiming the extracted API as a supported first-class graphics/platform contract:

- The representative workload runs cleanly through native Metal and Vulkan implementations.
- Win32, AppKit, Wayland, and X11 presentation paths are exercised on physical machines.
- Resize, minimize and restore, display changes, shutdown, synchronization, and failure paths are
  tested with native validation enabled.
- Differences between Vulkan and Metal ownership, synchronization, presentation, and capabilities
  are recorded rather than hidden prematurely.

Stop or narrow the project if comparable backends cannot be kept correct, if the supported hardware
contract proves impractical, or if platform maintenance already exceeds the value of shared code.

## Gate 2: a coherent and intrinsically valuable public slice

Extract only enough `mulciber` and `mulciber-platform` API to build a small real game. The game must own
its architecture while Mulciber coordinates the window/GPU lifecycle it claims to improve. Follow the
[API extraction and comparison plan](api-extraction-plan.md); candidate types remain unstable while
this gate is being tested.

Pass conditions:

- One unified game-facing contract drives Metal/AppKit and Vulkan with Win32, Wayland, or X11 without
  ordinary application branches for backend lifecycle machinery.
- Unsafe native ownership remains behind a safe, narrow public boundary.
- Correct resize, presentation, synchronization, and shutdown are the natural API path.
- Backend capabilities and escape hatches remain reachable without infecting ordinary portable code.
- The API is materially more coherent for a game than independently wiring a graphics library to a
  general windowing library.
- A Metal-only evaluation and a Vulkan-only evaluation each demonstrate material value from
  ownership, lifecycle, diagnostics, capability negotiation, or ergonomics when portability is
  explicitly awarded no credit.
- A build using one backend does not link, initialize, or impose ordinary-frame dispatch through the
  unused backend, and any dependency, binary-size, compile-time, or performance cost is measured.
- The same tasks are compared fairly with direct native implementations, practical single-backend
  Rust stacks, `wgpu`/`winit`, SDL3 GPU, Vulkano where applicable, and raylib as a scoped convenience
  reference. Exact versions, sources, validation, measurements, and non-equivalent functionality are
  preserved.

Stop if the API becomes mostly a younger `wgpu` shape, requires routine unsafe application code, or
cannot demonstrate a concrete lifecycle advantage. Redesign or stop if portability is the only reason
to prefer Mulciber over a reasonable direct Metal or Vulkan stack for its claimed serious-game slice.

## Gate 3: cold-start learnability

Mulciber cannot rely on an existing tutorial corpus or LLM training data. It must be easier to learn from
the repository than established alternatives are to recall from prior knowledge.

Maintain a small cold-start task suite that an unfamiliar Rust developer and current coding models
attempt using only the checked-out repository. Reuse the tasks and measurement rules in the
[comparison plan](api-extraction-plan.md). It should cover:

1. Open a window and clear it to a chosen color.
2. Upload a mesh and texture and render them with depth.
3. Respond correctly to resize, minimize, and close.
4. Add an optional capability path with a documented fallback.
5. Diagnose an intentionally invalid resource or synchronization request from Mulciber's error.

Pass conditions:

- A concise mental-model document is enough to understand ownership and frame flow.
- Canonical examples are complete, searchable, executable, and tested against the current API.
- Compiler and runtime errors identify the violated contract and a likely correction.
- The tasks do not require reading backend internals or guessing undocumented conventions.
- Task completion is at least as reliable and efficient as the equivalent `wgpu`/`winit` tasks when
  prior ecosystem familiarity is allowed.

Stop or redesign if Mulciber is only pleasant after extensive project-specific training. Familiarity is
a real product advantage, and technical elegance does not cancel it.

## Gate 4: native-feature differentiation

Implement one feature that materially motivates native backends—initially a bindless, GPU-driven
rendering path is the leading candidate—with capability negotiation and a tested fallback on both
Vulkan and Metal.

Compare the Mulciber implementation with direct native implementations and the best practical `wgpu`
implementation for feature reach, ergonomics, CPU overhead, frame behavior, and backend-specific
control. Include SDL3 GPU or another planned comparison when it can express a meaningful equivalent;
record inability to express the feature as a reach result rather than forcing a false substitute.
Mulciber passes only if the difference matters to a real game; exposing a native enum or producing a
synthetic benchmark win is not enough.

Stop if the feature requires an escape hatch so invasive that Mulciber has no useful portable contract,
or if established libraries expose the needed path with comparable control by the time this gate is
reached.

## Gate 5: integrated runtime value

Build a dogfood game slice that exercises input snapshots, fixed and variable updates, frame pacing,
fullscreen/display changes, suspension, and device recovery across the supported platforms.

Mulciber passes if coordinating these systems produces simpler game code, more predictable behavior, or
better diagnostics than composing independent libraries. It fails if `mulciber-runtime` merely wraps an
event loop, dictates unrelated engine architecture, or adds no measurable coherence.

Extend the Gate 2 comparison rather than inventing a new favorable baseline. SDL3 and `winit` remain
direct lifecycle comparisons; raylib and selected full engines may be used as scoped usability or
integrated-game references when their additional policy is reported rather than credited as graphics
API behavior.

## Gate 6: maintenance reality

Before claiming production readiness, operate Mulciber through real game development and at least one
release cycle. Track backend-specific defects, validation regressions, driver workarounds, platform
test time, documentation failures, and the delay between new native features and safe Mulciber exposure.

Continue only if the supported target set can be maintained to the promised quality bar by the
available team. Fewer targets are justified only by stronger native capability reach, lifecycle
coherence, learnability, or support—not by the target count itself.
