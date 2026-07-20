# Experimental custom-material contract

This checkpoint opens the first application-authored corner of the rendering vocabulary, forced
by the [material slice plan](material-slice-plan.md): shaders, vertex layouts, resource
bindings, and uniform data become application declarations instead of baked recipes, inside the
unchanged frame shape (one depth-tested, sample-count-selected scene pass with direct or
postprocessed output). The three fixed pipeline recipes and their recorded counts do not churn.

## Public checkpoint vocabulary

`Device::create_material_pipeline` consumes a `MaterialPipelineDescriptor`: a `ShaderArtifact`,
named vertex and fragment entry points, a `VertexLayout` (stride plus located, formatted,
offset `VertexAttribute`s), and a `MaterialBinding` declaration — at most one uniform slot with
an explicit byte size (capped at `MATERIAL_UNIFORM_SIZE_LIMIT`, 256), sampled-texture slots,
and sampler slots, all identified by their WGSL group-0 binding numbers and capped at
`MATERIAL_SLOT_LIMIT` (15), a range inside every native namespace both backends guarantee.

`mulciber-shader` records the module's interface — per entry point its stage, name, and
vertex-input locations with formats, plus the module's bindings with kinds and uniform WGSL
byte sizes — in the artifact container (`MULSHDR2`). Pipeline creation validates the
application's declaration against that record and rejects a mismatch as an invalid request
naming the offending attribute, slot, or entry point; interface constructs outside the
vocabulary (non-zero groups, storage buffers) are rejected as unsupported. The container bump
is deliberately breaking: the tool and crate ship together and no artifact stability was
promised.

`Device::create_mesh_with_layout` uploads raw vertex bytes against a declared layout; every
mesh (including the fixed `Vertex` path, which carries `VertexLayout::VERTEX`) retains its
layout, and a draw whose mesh and pipeline layouts differ is rejected at submission. Uniform
data is supplied per record as plain bytes of exactly the declared size — the application owns
WGSL memory-layout correctness — and flows through the session's frame-transient per-draw
uniform region; no persistent application-owned buffer handle was forced by this slice.

`SceneContent` grows one form: `Material(&[MaterialRecord])`, each record selecting a material
pipeline, a layout-matching mesh, one texture per declared texture slot in ascending binding
order, and its uniform bytes. Sampler slots bind a crate-owned linear repeat sampler.
`MaterialPipeline` joins the existing explicit-destroy and drop-reclamation paths, and
mixed-session diagnostics name the new handle kind.

## Native behavior

Vulkan derives one descriptor-set layout from the declaration (dynamic uniform buffer, sampled
images, samplers), reuses the shared 256-byte-stride dynamic uniform buffer for per-record
bytes, caches descriptor sets per texture-identity tuple, and draws through the existing
indexed-indirect path; descriptor pools reset with the same texture-reclamation and
buffer-growth rules as the fixed pipelines. Metal builds a vertex descriptor from the declared
layout at reserved buffer index 30 (collision-free because slots are capped at 15), binds the
uniform region and textures at their WGSL binding numbers on both vertex and fragment stages,
and draws through the existing indirect encoder path.

This checkpoint does not add application-composed passes, render-to-texture, load/store
policy, compute or storage buffers, blending or non-opaque state, instance-rate custom
layouts, bind-group abstractions, or new texture formats. Pass composition is expected to be
the next forcing slice.

## Evidence

Automated Linux evidence — the material-scene slice on native Wayland, under a KWin resize
storm, and on X11 through XWayland, plus thirty-one conformance cases including twelve material
cases on both paths, all validation-clean — is recorded in the
[Linux runbook](linux-validation.md). The Metal implementation compiles under the cross-host
type check but has not yet run on the M2 tier; the Metal artifacts for the new container also
await that session. No macOS claims are made at this revision.
