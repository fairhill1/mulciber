# Custom-material slice plan

The roadmap keeps one graphics-vocabulary item open: generalize the deliberately narrow
scene-submission vocabulary into owned buffer, binding, command/pass-composition, and
synchronization facilities **only as representative game slices force those concepts**. This
document pre-registers the next forcing slice and the vocabulary decisions it is allowed to open,
before implementation begins, so the opening stays evidence-driven rather than speculative.

Today every pipeline constructor is a baked recipe: applications already ship their own offline
`ShaderArtifact`, but `create_textured_pipeline`, `create_instanced_textured_pipeline`, and
`create_postprocess_pipeline` each demand fixed entry points, the one fixed `Vertex` layout, fixed
binding slots, and a fixed uniform shape, and `Device::create_mesh` accepts only `&[Vertex]`. That
menu can express Forge Run; it cannot express a game's own materials. Closing that gap is the
current viability long pole.

## Forcing workload

A new same-source example (working name `examples/material-scene`) renders a scene the fixed
recipes cannot express, inside the existing frame shape (one depth-tested, capability-selected
MSAA scene pass, direct or postprocessed output):

- **Two application-authored WGSL materials** with entry points the crate has never heard of,
  compiled by the separately installed `mulciber-shader` tool exactly as today.
- **An application vertex layout** different from the built-in `Vertex` — position, normal, and
  texture coordinate — with meshes uploaded from application vertex bytes against that declared
  layout.
- **Application-defined uniform data** updated every frame (time, camera, and per-material
  parameters supplied as bytes), so material animation is driven by the application's own uniform
  struct rather than a crate-known one.
- **Two sampled textures in one material**, blended in the application's fragment shader.
- **Two distinct custom pipelines in one scene pass**, proving material plurality rather than a
  single privileged custom slot.

Forge Run and its recorded checkpoint evidence stay untouched; this slice becomes the new
vocabulary's evidence. The workload deliberately keeps the frame shape fixed: it forces the
shader-interface, vertex-layout, binding, and uniform vocabulary and nothing else.

## Vocabulary the slice is allowed to open

- **Vertex layouts.** An application-described layout (attribute formats, offsets, stride,
  shader locations) declared at pipeline creation, plus mesh creation from raw vertex bytes
  validated against a declared layout. The fixed `Vertex` path remains for the existing recipes.
- **Binding interface.** A slot-explicit declaration — uniform data and sampled-texture slots
  identified by the same WGSL binding numbers the shader uses — supplied at pipeline creation.
  No bind-group object abstraction: nothing in this workload forces one.
- **Uniform bytes.** Per-frame uniform data supplied as plain bytes with declared size; the
  application owns WGSL memory-layout correctness, and the crate validates length, finiteness
  where it already does, and slot membership. Upload is frame-transient through the same
  machinery as the existing instance buffer; no persistent application-owned buffer handle is
  added unless the slice physically forces one, and if it does, that pressure is recorded here.
- **Custom material pipeline.** A new owning handle created from a `ShaderArtifact`, named entry
  points, a vertex layout, and a binding declaration, using the existing depth, sample-count, and
  opaque color states. It joins the existing generational reclamation and explicit-destroy paths.
- **Scene submission.** `SceneContent` grows one form: ordered records that select a custom
  pipeline, a layout-matching mesh, the declared textures, uniform bytes, and a transform. The
  existing textured records, instance batches, and `SceneOutput` forms are unchanged.

## Artifact interface metadata

The `mulciber-shader` container already carries a versioned header; it grows an interface
description emitted from the pinned Naga IR — per entry point, the vertex-input locations and
formats and the binding slots with their kinds. Pipeline creation validates the application's
declaration against the artifact's interface, and a mismatch is an `InvalidRequest` that names
the offending attribute or slot, in the spirit of the mixed-session messages. Backend behavior
never depends on the metadata beyond validation; artifacts predating the extension fail
artifact validation like any other malformed container, since the tool and crate ship together
and no artifact format stability has been promised.

## Deliberately closed

This slice does not open, and its implementation must not quietly add: application-composed
passes or render-to-texture, load/store policy, transient target allocation, compute or storage
buffers, GPU-written data, blending or non-opaque pipeline state, custom depth/rasterizer state,
instance-rate input for custom layouts, bindless or bind-group abstractions, mipmapped or
non-RGBA8 sampled formats beyond what the existing texture path provides, or multiple frames in
flight. Pass composition is expected to be the next forcing slice after this one. Whether the
three fixed recipes are later reimplemented over the opened vocabulary is a post-slice decision;
they and their recorded counts do not churn in this checkpoint.

## Evidence required before the checkpoint is claimed

- `probes/api-conformance` cases: custom pipeline creation and draw on both output forms;
  declaration/artifact mismatch rejected with the offending name; mesh-bytes/layout mismatch
  rejected; uniform length mismatch rejected; mixed-session rejection naming the new handle
  kinds; explicit destruction and drop-reclamation of the new pipeline kind.
- The standard required checks, with the validation layer enforced in every run.
- Physical Linux runs of the slice on native Wayland and X11, recorded in the
  [Linux runbook](linux-validation.md); a macOS Metal run on the M2 tier when that machine is
  next available, recorded in the [macOS ledger](macos-validation.md).
- A paired `wgpu` slice for the ergonomics comparison, matching the established checkpoint
  pattern, may follow as a separate step; its absence blocks the Gate 2 comparison entry, not
  this checkpoint.

Decisions recorded here were settled on 2026-07-20; the resulting contract text lands in the
[API slice decision ledger](api-slice-decisions.md) and a checkpoint contract document once the
implementation exists and has evidence.

Status, later on 2026-07-20: implemented as planned with two deviations recorded in the
[material contract](material-contract.md) — the record carries no separate transform field
(the application's own uniform bytes carry its matrices), and binding slots plus attribute
locations are capped at 15 so one contract fits every native namespace. Automated Wayland,
resize-storm, X11, and thirty-one-case conformance evidence lives in the
[Linux runbook](linux-validation.md); the macOS run and Metal artifacts await the next M2
session.
