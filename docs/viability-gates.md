# Zinc viability gates

Zinc is an experiment until evidence shows that its advantages justify owning native GPU and
platform backends. These gates prevent implementation momentum, dependency minimalism, or attachment
to the project from becoming substitutes for product value.

A gate is passed only by checked-in code, reproducible validation evidence, and a written decision.
If a gate fails, the default action is to stop, narrow, or repurpose Zinc—not to move the goalposts.

## Gate 1: native backend credibility

Before extracting a public graphics API:

- The representative workload runs cleanly through native Metal and Vulkan implementations.
- Win32, AppKit, Wayland, and X11 presentation paths are exercised on physical machines.
- Resize, minimize and restore, display changes, shutdown, synchronization, and failure paths are
  tested with native validation enabled.
- Differences between Vulkan and Metal ownership, synchronization, presentation, and capabilities
  are recorded rather than hidden prematurely.

Stop or narrow the project if comparable backends cannot be kept correct, if the supported hardware
contract proves impractical, or if platform maintenance already exceeds the value of shared code.

## Gate 2: a coherent public slice

Extract only enough `zinc-gpu` and `zinc-platform` API to build a small real game. The game must own
its architecture while Zinc coordinates the window/GPU lifecycle it claims to improve.

Pass conditions:

- Unsafe native ownership remains behind a safe, narrow public boundary.
- Correct resize, presentation, synchronization, and shutdown are the natural API path.
- Backend capabilities and escape hatches remain reachable without infecting ordinary portable code.
- The API is materially more coherent for a game than independently wiring a graphics library to a
  general windowing library.

Stop if the API becomes mostly a younger `wgpu` shape, requires routine unsafe application code, or
cannot demonstrate a concrete lifecycle advantage.

## Gate 3: cold-start learnability

Zinc cannot rely on an existing tutorial corpus or LLM training data. It must be easier to learn from
the repository than established alternatives are to recall from prior knowledge.

Maintain a small cold-start task suite that an unfamiliar Rust developer and current coding models
attempt using only the checked-out repository. It should cover:

1. Open a window and clear it to a chosen color.
2. Upload a mesh and texture and render them with depth.
3. Respond correctly to resize, minimize, and close.
4. Add an optional capability path with a documented fallback.
5. Diagnose an intentionally invalid resource or synchronization request from Zinc's error.

Pass conditions:

- A concise mental-model document is enough to understand ownership and frame flow.
- Canonical examples are complete, searchable, executable, and tested against the current API.
- Compiler and runtime errors identify the violated contract and a likely correction.
- The tasks do not require reading backend internals or guessing undocumented conventions.
- Task completion is at least as reliable and efficient as the equivalent `wgpu`/`winit` tasks when
  prior ecosystem familiarity is allowed.

Stop or redesign if Zinc is only pleasant after extensive project-specific training. Familiarity is
a real product advantage, and technical elegance does not cancel it.

## Gate 4: native-feature differentiation

Implement one feature that materially motivates native backends—initially a bindless, GPU-driven
rendering path is the leading candidate—with capability negotiation and a tested fallback on both
Vulkan and Metal.

Compare the Zinc implementation with the best practical `wgpu` implementation for feature reach,
ergonomics, CPU overhead, frame behavior, and backend-specific control. Zinc passes only if the
difference matters to a real game; exposing a native enum or producing a synthetic benchmark win is
not enough.

Stop if the feature requires an escape hatch so invasive that Zinc has no useful portable contract,
or if established libraries expose the needed path with comparable control by the time this gate is
reached.

## Gate 5: integrated runtime value

Build a dogfood game slice that exercises input snapshots, fixed and variable updates, frame pacing,
fullscreen/display changes, suspension, and device recovery across the supported platforms.

Zinc passes if coordinating these systems produces simpler game code, more predictable behavior, or
better diagnostics than composing independent libraries. It fails if `zinc-runtime` merely wraps an
event loop, dictates unrelated engine architecture, or adds no measurable coherence.

## Gate 6: maintenance reality

Before claiming production readiness, operate Zinc through real game development and at least one
release cycle. Track backend-specific defects, validation regressions, driver workarounds, platform
test time, documentation failures, and the delay between new native features and safe Zinc exposure.

Continue only if the supported target set can be maintained to the promised quality bar by the
available team. Fewer targets are justified only by stronger native capability reach, lifecycle
coherence, learnability, or support—not by the target count itself.
