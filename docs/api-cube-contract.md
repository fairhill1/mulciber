# Gate 2 textured-cube API contract

Status: implementation contract for the first resource-backed shared graphics slice.

This slice must run one application source on Metal/AppKit and Vulkan/Win32, Wayland, and X11. It
is intentionally smaller than either native triangle probe. The probes remain the evidence bed for
compute, indirect drawing, shadows, post-processing, pipeline caches, and compressed-texture
fallbacks.

## Checkpoint

The ordinary example must:

- open a surface-compatible native device;
- report the selected backend and either four-sample MSAA or the observable one-sample fallback;
- upload indexed cube geometry and an RGBA8 sRGB texture;
- create a depth-tested textured pipeline from a target-selected offline shader artifact;
- recreate extent-dependent color and depth targets after surface generation changes;
- render a perspective-correct spinning cube and present it;
- drain native GPU and presentation ownership during fallible shutdown.

The companion public-API validation probe must additionally select both the preferred four-sample
and forced one-sample paths, safely abandon an acquired frame, submit later recovery frames, and
support finite automated execution. Those controls are evidence machinery, not normal application
behavior, and do not belong in the example.

Ordinary example source must not contain Metal/Vulkan or AppKit/Win32/Wayland/X11 branches. It has
one WGSL shader source. The separately installed `mulciber-shader` development tool emits cached
native artifacts. The application's dependency-free build script packages the artifact for the Rust
compilation target. Only the selected native backend and shader artifact may be linked or embedded.

## Object and ownership model

Opening graphics produces distinct `Device`, `Queue`, and `Surface` handles plus a selection report.
They share a private session so backend-required instance, adapter, queue, presentation, and
retirement lifetimes cannot be torn apart in an invalid order. This is a logical ownership split,
not a claim that Vulkan and Metal have identical native object graphs.

`Device` creates owning, non-`Copy` `Mesh`, `Texture`, and `TexturedPipeline` handles. `Device` also
creates owning `RenderTargets` for one `SurfaceInfo`; those targets contain depth storage and any
backend-private multisample color storage. A target from an older surface generation is rejected
instead of being silently stretched or rebound. The postprocess checkpoint follows the same model
for its pipeline and targets.

Native resource destruction is session-coordinated. Consuming `Device::destroy_*` operations report
completion or destruction failure immediately; dropping a handle queues the same reclamation for the
next mutable graphics operation, while shutdown destroys anything still live. Generational arena
identities permit bounded slot reuse without allowing an old identity to name its replacement.
Metal releases the session's Objective-C retains immediately because retained command buffers keep
their referenced resources alive through GPU completion. Vulkan first waits the slice's one
in-flight frame fence, invalidates descriptor pools that reference a destroyed texture or
postprocess target, and then destroys the native device children. Superseded render targets remain
eligible for earlier backend reclamation so continuous resize cannot grow device memory without
bound; their owning handles remain valid identities but drawing them reports that their native
targets were reclaimed.

An acquired `Frame` owns exactly one drawable or swapchain image. Acquisition reconfigures the
surface internally for changed window metrics, so a ready frame always matches the requested
metrics and its surface information is the generation render targets must match; the application
rebuilds targets when the frame reports different information. Presenting consumes the frame.
Explicit abandonment consumes it through the backend-specific safe path. Dropping it performs the
same best-effort abandonment and defers any failure to the next surface operation.

## First command vocabulary

The queue exposes one resource-backed operation: draw an indexed textured mesh with a column-major
model-view-projection matrix into matching render targets and present the frame. The draw clears
color and depth, uses depth comparison and writes, and renders one mesh with one texture.

This is deliberately not named a general render pass or command encoder. A reusable command model
will be extracted only after a second materially different operation provides evidence for its
boundaries. The narrow operation must still validate that every handle belongs to the same session
and that the render targets match the acquired surface generation.

The later [two-pass postprocess checkpoint](postprocess-contract.md) supplies that second operation:
resolved offscreen color becomes sampled input to a fullscreen pass. It initially adds dedicated
`PostprocessTargets`, `PostprocessPipeline`, `PostprocessedDraw`, and one fixed two-pass queue method.
That is deliberate pressure evidence rather than an immediate generalization: one sequence still
does not settle arbitrary pass ordering, attachment load/store policy, multiple draws, transient
allocation, or copy/compute integration.

## Fixed first-slice formats

- Vertex: three `f32` position components, three `f32` color components, and two `f32` UV
  components.
- Index: `u16`.
- Texture: tightly packed RGBA8 sRGB.
- Depth: backend-selected 32-bit floating depth where supported by the proven probes.
- Transform: column-major 4-by-4 `f32` matrix.
- Samples: prefer four, visibly report and support fallback to one.

These formats are checkpoint constraints, not a permanent capability ceiling.

## Shader boundary

`ShaderArtifact` is opaque validated bytes. The cube author writes one WGSL module. The
`mulciber-shader` development tool uses pinned Naga to emit SPIR-V 1.4 for Vulkan or MSL 3.1 and an
Apple metallib for Metal. Generated artifacts are cacheable and may be checked in or produced by CI;
ordinary application builds do not compile or depend on Naga. The application uses the same Rust
expression to include target-selected build output.

This choice is intentionally scoped to Naga's validated cross-backend intersection. The baseline
textured vertex/fragment workload has already produced valid Vulkan output and an Apple-accepted
native pipeline. Known workgroup-memory and mesh SPIR-V failures are rejected rather than hidden or
worked around with a second user-authored shader. Advanced capabilities retain independent native
paths until a single-source compiler path has equivalent evidence.

## Errors and validation

Creation, acquisition, upload, pipeline creation, explicit destruction, drawing, presentation, and
shutdown remain fallible. Invalid dimensions, byte counts, indices, non-finite transforms,
mixed-session handles, and stale render targets are rejected before native calls where practical.
Validation-layer or Metal debug-layer warnings and errors fail their validation runs.

Acceptance requires automated Windows preflight plus physical visual, resize, abandonment/recovery,
and lifecycle evidence on each available target. Automated abandonment, fallback, and finite-run
coverage comes from the public-API probe; the example remains interactive. A single machine or
display is never recorded as broader coverage.
