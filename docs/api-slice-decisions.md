# API slice decision ledger

This ledger records the decisions the [API extraction and comparison plan](api-extraction-plan.md)
required the first slice to establish. Each entry states what is decided for the experimental slice,
where the deciding contract or code lives, and what deliberately stays open. Per the plan, a
decision the slice does not need stays open rather than receiving a speculative general solution;
every name remains an unstable Gate 2 experiment.

## Application and event-loop ownership

Decided. `mulciber-platform` owns a main/creating-thread-confined `Application` and `Window`; the
game calls `Application::pump_events` and receives translated lifecycle, redraw, metric, and close
events through a fallible callback whose first error the pump returns, keeping its own architecture
without a per-application error slot. Platform types are neither `Send` nor `Sync`, so
native-thread ownership is structural. Nested native dispatch (the Win32 sizing loop) may deliver
redraw inside the pump; handler errors propagate out of that nesting through the platform layer.
The platform also owns the startup wait for first drawable metrics
(`Application::wait_for_first_metrics`). Recorded in the
[experimental platform contract](api-platform-contract.md).

## Object topology

Decided for the slice. `OpenedGraphics::open` consumes a borrowed window surface target plus current
metrics and produces distinct `Device` (resource creation), `Queue` (submission), and `Surface`
(presentation) owners over one private native session, plus an observable `DeviceSelection`.
Vulkan's surface-compatible adapter selection happens inside opening without distorting Metal
initialization. The session keeps instance/adapter/queue/presentation/retirement lifetimes ordered;
explicit `shutdown` refuses to run while an acquired frame is live. Whether opening later splits
into separate public context/selection values stays open. Recorded in the
[textured-cube contract](api-cube-contract.md) and implemented in `crates/mulciber/src/graphics.rs`.

## Surface generations

Decided. `WindowRevision` is desktop-OS input; `SurfaceGeneration` is graphics-owned output that
advances on every successful replacement presentation configuration, including same-extent
replacements and Vulkan's base-swapchain abandonment fallback; suspension alone does not advance
it. Extent-dependent resources belong to exactly one generation and are rejected, then reclaimed,
when superseded. Recorded in the
[experimental graphics lifecycle contract](api-graphics-contract.md).

## Frame lifecycle

Decided. Acquisition returns a ready surface-scoped frame or a temporary-unavailability reason
(suspended, drawable absent, timed out, or reconfiguration deliberately paced). Reconfiguration for
changed metrics happens inside acquisition: a ready frame always matches the requested metrics, and
a frame whose surface information differs from the application's render targets is the one rebuild
signal, enforced by draw-time rejection. A separate reconfigured outcome was implemented first and
rejected with physical Wayland evidence — both validated native probes already reconfigure inside
their own frame machinery, and the separate outcome made trailing live resize the ergonomic default.
Every ready frame receives exactly one fallible disposition: present or explicit abandonment, with
`Drop` as best-effort deferred abandonment. Backends keep different native machinery (Metal
autorelease-scoped drawables; Vulkan acquisition fences, swapchain maintenance, or whole-generation
replacement). Recorded in the
[experimental graphics lifecycle contract](api-graphics-contract.md).

## Presentation feedback

Decided for the diagnostics slice. The surface owner drains native presentation completions
non-blockingly; samples identify frames by per-session presented index and carry an optional
display time, with `None` preserving the physically observed presented-without-a-time startup and
occlusion cases. Absence is explicit: a backend without native feedback answers `Unsupported` on
every drain instead of silently estimating. Collection is always on and bounded (registration
costs one native call per present; undrained queues are capped), while consumption is opt-in.
Cadence estimation lives in `mulciber-runtime::PacingDiagnostics` over plain `Instant`s, keeping
the runtime graphics-agnostic. Both backends now construct samples: Metal from drawable presented
handlers, Vulkan from the probe-proven `VK_EXT_present_timing` drain, whose swapchain-scoped time
domains are re-anchored per swapchain to the process clock so intervals stay native-exact and are
never paired across recreations. Pacing policy and scheduling hooks are deliberately not part of
this decision; they wait on the [Gate 4 pacing plan](gate4-pacing-plan.md) measurement runs.
Recorded in the [experimental graphics lifecycle contract](api-graphics-contract.md) and the
[runtime contract](runtime-contract.md).

## Resource use and synchronization

Decided for the slice, deliberately narrow. The queue first exposed one resource-backed operation —
draw one indexed textured mesh with depth into generation-matched targets and present — with all
hazard translation backend-owned. A later two-pass checkpoint adds generation-bound resolved scene
color and a fixed fullscreen sampled pass. Metal uses ordered encoders; Vulkan derives the explicit
color-attachment-write to fragment-sampled-read transition behind the same safe operation.

The multi-object checkpoint extends the scene pass to a non-empty ordered slice whose records may
select different meshes, textures, pipelines, and transforms. Both backends keep one scene pass open
and issue one indexed draw per record. This establishes heterogeneous multi-draw but deliberately
does not call it instancing or batching.

The instancing checkpoint adds a distinct instance-rate pipeline and homogeneous batches containing
one mesh, texture, pipeline, and a non-empty finite transform slice. Both backends pack transforms
into one frame-local instance buffer and issue one native indexed draw per batch. Fixed recipes are
composed through `Queue::render_and_present`, `SceneSubmission`, `SceneContent`, and `SceneOutput`;
separating content from output prevents their cross-product from producing composition-sized method
or variant names while leaving the earlier focused methods intact.

The custom-material checkpoint opens the first application-authored corner of this vocabulary,
forced by the slice pre-registered in the [material slice plan](material-slice-plan.md):
material pipelines are created from an application shader artifact, named entry points, a
declared vertex layout, and slot-explicit bindings, all validated against the interface the
shader compiler records in the artifact; meshes upload raw vertex bytes against declared
layouts that submission matches to pipelines; uniform data is per-record plain bytes through
the frame-transient uniform region (no persistent buffer handle was forced, and a separate
per-record transform field was dropped as redundant because the application's own uniform
carries its matrices). Recorded in the [material contract](material-contract.md).

No general render-pass or command-encoder vocabulary exists yet. These operations establish a real
intermediate-resource dependency, heterogeneous ordered draws, native instance-rate batches, and
application-authored materials, but still do not constrain arbitrary pass ordering, load/store
policy, transient allocation, copy/compute integration, automatic grouping, sorting, or
GPU-written data enough to justify a broad API. Recorded in the
[textured-cube contract](api-cube-contract.md), [two-pass postprocess contract](postprocess-contract.md),
[multi-object scene contract](scene-contract.md), [GPU instancing contract](instancing-contract.md),
and [custom-material contract](material-contract.md).

## Blend and depth state

Decided for the material slice as a fixed mode set, not a general state object. Material pipeline
creation declares a `BlendMode` — `Opaque`, `Cutout` (alpha-to-coverage, degrading to a hard alpha
threshold at one sample), or `PremultipliedTranslucent` (source-over with premultiplied alpha) —
and a `DepthMode` (`TestWrite`, `TestOnly`, `Off` — off never writes). The set matches the material
taxonomy a voxel-style dogfood needs (opaque terrain, foliage cutouts, translucent water, skybox
and overlay depth control) without opening arbitrary blend equations, factors, per-attachment
state, or comparison functions; both backends bake the modes into creation-time native pipeline
state. Draw ordering stays application-owned through record order: translucent records composite
over whatever the target holds when they draw. wgpu-style freeform state objects were considered
and rejected — no slice has forced any combination outside these six values. The fixed pipeline
recipes keep their recorded opaque test-write behavior. Recorded in the
[material contract](material-contract.md).

## Texture mip chains

Decided as an application-supplied chain, not native generation.
`Device::create_rgba8_srgb_texture_with_mips` uploads a complete chain from the base level to
1x1 — each level halving both extents and flooring at one texel, every level's byte count
validated against its extent, partial chains rejected by name. Mip content is application
policy in the same sense as packed uniform bytes and WGSL modules: the downsampling filter,
color-space handling, and any per-level authoring stay outside the engine, and blit-based
native generation stays closed until a slice forces it. Mip filtering derives from each
sampler slot's declared filter — `Linear` interpolates between levels, `Nearest` picks one —
so no separate mip-filter axis opens; single-level textures sample exactly as before. The
fixed-recipe texture and postprocess samplers keep their recorded single-level behavior.
Recorded in the [material contract](material-contract.md).

## Shadow pass and depth sampling

Decided as one fixed depth-only pre-pass recipe, not general pass composition.
`Device::create_shadow_map` creates a square sampleable `D32` depth target (extent capped at
8192, filtered-depth support checked at creation); `Device::create_shadow_pipeline` builds a
depth-only pipeline from an application vertex entry point — no fragment stage, one sample, at
most one uniform binding, consuming any subset of the declared vertex layout so one shadow
module serves every caster layout. A `SceneSubmission` optionally carries one `ShadowPass` (a
map plus depth-only records) that both backends encode before the material scene pass: Vulkan
as an additional dynamic-rendering pass with explicit depth-attachment-to-sampled-read
transitions, Metal as an ordered encoder on the frame's command buffer. Material pipelines
declare at most one `DepthTexture` slot, fed per record from a shadow map, and at most one
`ComparisonSampler` slot with fixed recipe state — linear filtering, clamp-to-edge addressing,
less-or-equal comparison — while depth bias stays application-owned in the authored WGSL.
Sampling a map no pass has rendered is rejected by name before the frame token is consumed so
the rejection cannot strand an acquired image. `mulciber-shader` records `texture_depth_2d`
and `sampler_comparison` bindings as their own interface kinds inside the unchanged `MULSHDR2`
container: existing artifacts stay valid, and shadow modules require the paired crate.
Arbitrary pass graphs, color render-to-texture, multiple shadow passes per submission, and
textured cutout shadow casters stay closed until a slice forces them. Recorded in the
[material contract](material-contract.md).

## Read-only storage and skinned records

Decided as one frame-transient read-only storage slot per pipeline, forced by skeletal
animation's bone palettes outgrowing the 256-byte uniform stride. Material and shadow pipelines
may declare at most one `MaterialBinding::Storage` slot (a WGSL `var<storage, read>` with a
creation-fixed byte size, validated exactly against the recorded type and capped at 64 KiB),
and each record supplies the bytes per frame — `MaterialRecord.storage` and
`ShadowRecord.storage` — so a skinned caster shadows with the same palette as its material
record. No persistent buffer handle was opened: palettes change every frame, and nothing in
the slice forces retained application-owned buffers. `mulciber-shader` records the storage
kind, which the container already reserved, with its byte size; read-write access and
runtime-sized arrays are compile errors (Naga reports a runtime array's size as one element,
Metal would need a sizes side-buffer, and creation-fixed sizes keep validation symmetric with
uniforms). Joint indices and weights ride the existing `Uint32x4`/`Float32x4` vertex formats —
packed vertex formats belong to the scale wall. Both backends pack record bytes into a second
shared ring at 256-byte-aligned offsets (the specification's cap on
`minStorageBufferOffsetAlignment`): Vulkan as one dynamic storage-buffer descriptor whose
dynamic offsets order by binding number alongside the uniform's, Metal as one buffer bound at
the WGSL slot per record. Compute, read-write storage, and general buffer vocabulary stay
closed until a slice forces them. Recorded in the [material contract](material-contract.md).

## Resource ownership and reclamation

Decided for the slice. Mesh, texture, pipeline, and generation-dependent target handles are owning
and non-`Copy`. Consuming `Device::destroy_*` methods provide fallible immediate reclamation;
ordinary `Drop` queues reclamation for the next mutable graphics operation, and fallible shutdown
destroys all remaining native resources. Reusable generational arena slots prevent stale identities
from aliasing replacements. Metal relies on retained command-buffer references after releasing the
session's retain. Vulkan waits the one in-flight frame fence and invalidates affected descriptor
pools before native destruction. This settles bounded lifetime for the current single-queue,
single-in-flight slice, not general multi-queue dependency tracking, externally owned resources, or
allocator policy. Recorded in the [textured-cube contract](api-cube-contract.md).

## Capabilities and fallbacks

Decided for the slice. `DeviceRequest` carries the preferred sample count; unsupported four-sample
rendering falls back observably to one through `DeviceSelection`, which also reports the selected
backend. Required capabilities (validation availability, surface-compatible device) reject opening
with a structured error. The general optional-capability vocabulary beyond multisampling stays
open. Implemented in `crates/mulciber/src/graphics.rs`; exercised by the api-cube probe's forced
one-sample path.

## Errors and recovery

Provisionally decided. Nonfatal states are typed outcomes, not errors: retry-later is
`SurfaceUnavailable`, rebuild is the frame/target generation mismatch, and `Result::Err` carries a
`GraphicsError` for genuine failures including deferred abandonment failures surfaced by the next
fallible surface operation. `GraphicsErrorKind` distinguishes invalid request, unsupported,
lifecycle, stale resource, surface failure, device failure, out of memory, validation, native
failure, and internal failure according to the recovery action current evidence supports. The
contextual message remains available separately. Vulkan result codes map directly where possible;
uncategorized backend failures deliberately remain `NativeFailure` instead of being guessed from
message text. Validation warnings and errors fail validation runs. Rich native diagnostic payloads
and physically exercised device-loss/out-of-memory recovery remain open in the
[graphics contract](api-graphics-contract.md).

Amended 2026-07-19 from the [Gate 3 cold-start run](gate3-cold-start-results.md): mixed-session
handles are always `InvalidRequest` (a caller bug to correct), including scene targets, which
previously reported `StaleResource` for the same conceptual violation; `StaleResource` on targets
now means exactly the surface-information mismatch whose correction is a rebuild. Every
mixed-session message names the offending handle (mesh, texture, pipeline, postprocess pipeline,
render targets, postprocess targets) instead of the former generic "graphics handles belong to
different sessions", closing the which-handle gap the run recorded.

## Native reach

Deliberately open. The first slice exposes no backend-specific capability boundary; the hidden
`integration` module is probe machinery, not the answer. The recorded constraint any future reach
must satisfy: it cannot invalidate session-owned resource and presentation-retirement tracking.
This stays open until Gate 4 pressure produces a real consumer.

## Backend selection and cost

Decided. The compilation target selects the backend at `cfg(target_os)` module level — Metal on
macOS, Vulkan on Windows and Linux — with no cargo features, no runtime backend dispatch, and no
unused-backend code compiled, linked, or initialized. Single-backend build proof and measured cost
are recorded in the platform validation ledgers ([Linux](linux-validation.md),
[macOS](macos-validation.md)).

## Shader inputs

Decided for the slice. Applications ship one WGSL module compiled offline by the separately
installed `mulciber-shader` tool (pinned Naga) into target-selected SPIR-V or MSL/metallib
artifacts; ordinary builds embed the checked-in or cached artifact and never depend on the
compiler. Since the custom-material checkpoint, the artifact container also records the module's
compiler-derived interface (entry points, vertex inputs, bindings with uniform sizes), which
material pipeline creation validates application declarations against; the container bump is
deliberately breaking because the tool and crate ship together. This intentionally does not
select the eventual authoring language; advanced capabilities keep independent native paths until
a single-source path has equivalent evidence. Recorded in the
[textured-cube contract](api-cube-contract.md), the
[material contract](material-contract.md), and the
[shader toolchain evaluation](shader-toolchain-evaluation.md).
