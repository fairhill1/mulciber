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
vertex-input locations with formats, plus the module's bindings with kinds and, for uniform
and read-only storage data, WGSL byte sizes — in the artifact container (`MULSHDR2`).
`texture_depth_2d` and `sampler_comparison` bindings record as their own kinds inside the same
container: existing artifacts stay valid, while artifacts using the new kinds require the
paired crate. Read-only storage records under the kind the container already reserved, with
its creation-fixed byte size; read-write access and runtime-sized arrays are compile errors. Pipeline
creation validates the application's declaration against that record and rejects a mismatch as
an invalid request naming the offending attribute, slot, or entry point; interface constructs
outside the vocabulary (non-zero groups, storage buffers) are rejected as unsupported. The
container bump was deliberately breaking: the tool and crate ship together and no artifact
stability was promised.

`Device::create_mesh_with_layout` uploads raw vertex bytes against a declared layout with
16- or 32-bit indices (`MeshIndices`), lifting the fixed path's `u16` bound for chunked or
merged geometry; every mesh (including the fixed `Vertex` path, which carries
`VertexLayout::VERTEX`) retains its layout, and a draw whose mesh and pipeline layouts differ
is rejected at submission. Uniform
data is supplied per record as plain bytes of exactly the declared size — the application owns
WGSL memory-layout correctness — and flows through the session's frame-transient per-draw
uniform region; no persistent application-owned buffer handle was forced by this slice.

`Device::create_rgba8_srgb_texture_with_mips` uploads an application-supplied mip chain: the
base level through 1x1, each level halving and flooring at one texel, every level's byte count
validated against its extent. Sampler slots follow their declared filter across levels —
`Linear` interpolates between them, `Nearest` picks one — so mip behavior costs no new
declaration axis, and single-level textures sample exactly as before. Mip content
(downsampling filter, color-space handling) is application policy; native generation is not
part of the vocabulary.

A material pipeline may declare one read-only storage slot (`MaterialBinding::Storage`): a
WGSL `var<storage, read>` whose creation-fixed byte size must match the recorded type exactly,
capped at `MATERIAL_STORAGE_SIZE_LIMIT` (64 KiB). Each record supplies the bytes per frame
through the same frame-transient region model as uniforms — `MaterialRecord.storage` — sized
exactly to the declaration; the slot exists for skeletal animation's bone palettes, which
outgrow the 256-byte uniform stride. Shadow pipelines accept the same slot
(`ShadowRecord.storage`), so a skinned caster shadows with the same palette as its material
record. No persistent application-owned buffer handle is part of the vocabulary.

`SceneContent` grows one form: `Material(&[MaterialRecord])`, each record selecting a material
pipeline, a layout-matching mesh, one texture per declared texture slot in ascending binding
order, and its uniform bytes. Each sampler slot declares its filter (`Nearest`/`Linear`) and
address mode (`Repeat`/`ClampToEdge`); the pipeline owns one native sampler per declared slot.
The descriptor also declares the pipeline's `BlendMode` — `Opaque`, alpha-to-coverage `Cutout`,
or `PremultipliedTranslucent` source-over — and `DepthMode` (`TestWrite`, `TestOnly`, `Off`), a
fixed mode set baked into the native pipeline at creation rather than a general state object;
ordering translucent records after the opaque geometry they composite over stays
application-owned through record order.
`MaterialPipeline` joins the existing explicit-destroy and drop-reclamation paths, and
mixed-session diagnostics name the new handle kind.

The shadow recipe adds two resources and one submission axis. `Device::create_shadow_map`
creates a square sampleable depth target; `Device::create_shadow_pipeline` builds a depth-only
pipeline from a vertex entry point with at most one uniform binding, consuming any subset of
its declared vertex layout. `SceneSubmission` optionally carries one `ShadowPass` — the map
plus depth-only `ShadowRecord`s — rendered before the scene pass, and material pipelines may
declare one `DepthTexture` slot (supplied per record from a shadow map) plus one
`ComparisonSampler` slot whose native state is fixed: linear filtering, clamp-to-edge, and a
less-or-equal comparison, so `textureSampleCompare` returns one where the reference depth is
at most the stored depth. Depth bias is application policy in the authored shader. A record
must supply a map exactly when its pipeline declares the slot, and sampling a map no shadow
pass has rendered — this frame or earlier — is rejected by name before the frame is consumed.
`ShadowMap` and `ShadowPipeline` join the explicit-destroy and drop-reclamation paths.

The cascaded extension generalizes both ends of that recipe while keeping cascade policy
application-owned. `Device::create_shadow_map_array` creates a square layered depth target
(one through eight layers, one cascade per layer), and `SceneSubmission.shadow` becomes a
`ShadowPrepass`: `Single` wraps the existing one-map `ShadowPass`, while `Cascaded` carries a
`CascadedShadowPass` — the layered map plus one depth-only record list per layer, in layer
order, with the list count validated against the map's layer count. Each cascade renders with
its own record list because every cascade carries its own light matrix in its record uniforms
and the application may cull casters per cascade; a cascade with no records still clears its
layer, leaving that cascade fully lit. Material pipelines may declare a `DepthTextureArray`
slot (a `texture_depth_2d_array`, recorded as its own interface kind by `mulciber-shader`)
instead of the plain `DepthTexture` slot — at most one depth-texture slot of either kind — and
records supply the matching resource through `ShadowSource::Map`/`ShadowSource::Array`, with
kind mismatches rejected by name. Split distances, per-cascade light matrices, texel snapping,
depth bias, and per-fragment cascade selection all stay in application code and shaders; the
crate sees only the layered map, the per-cascade record lists, and bytes. `ShadowMapArray`
joins the explicit-destroy and drop-reclamation paths.

## Native behavior

Vulkan derives one descriptor-set layout from the declaration (dynamic uniform buffer, sampled
images, samplers), reuses the shared 256-byte-stride dynamic uniform buffer for per-record
bytes, caches descriptor sets per texture-identity tuple, and draws through the existing
indexed-indirect path; descriptor pools reset with the same texture-reclamation and
buffer-growth rules as the fixed pipelines. Metal builds a vertex descriptor from the declared
layout at reserved buffer index 30 (collision-free because slots are capped at 15), binds the
uniform region and textures at their WGSL binding numbers on both vertex and fragment stages,
and draws through the existing indirect encoder path. Both backends bake the declared blend and
depth modes into native creation-time state: Vulkan through the pipeline's color-blend,
multisample (alpha-to-coverage), and depth-stencil create info; Metal through pipeline-descriptor
blending and alpha-to-coverage plus the pipeline-owned depth-stencil state.

Record storage bytes flow through a second shared frame-transient buffer at 256-byte-aligned
per-record offsets, the specification's cap on `minStorageBufferOffsetAlignment`. Vulkan binds
it as one dynamic storage-buffer descriptor in the pipeline's set — its dynamic offset ordered
with the uniform's by binding number — growing and resetting descriptor pools under the same
rules as the uniform ring; Metal binds the buffer at the record's offset on the WGSL slot for
both scene stages, and vertex-only in the shadow encoder.

The shadow pass shares the frame's per-draw uniform region, with shadow-record slots packed
after the material records'. Vulkan encodes it as a depth-only dynamic-rendering pass at the
map's extent before the scene pass, transitioning the map from its prior state into depth
writing and out to fragment-sampled reading, and binds the map's view plus the pipeline-owned
comparison sampler through the material descriptor set (cached per sampled-identity tuple,
shadow map included). Metal encodes it as its own render command encoder — depth attachment
only, store-to-texture — ordered before the scene encoder on the same command buffer, and
binds the map and comparison sampler at their WGSL slots. Shadow pipelines run the vertex
entry alone: no fragment function, no color attachments, one sample, depth test-write.

The cascaded pre-pass encodes the same fixed depth-only recipe once per layer: Metal targets
one array slice per encoder through the depth attachment's slice selection, Vulkan renders
into per-layer views of the layered image and samples through a separate 2D-array view, and
both bind the whole array plus the fixed-recipe comparison sampler for scene sampling.
Shadow-record uniform and storage slots pack after the material records' in cascade order
across every layer.

This checkpoint does not add general pass composition — the shadow pre-pass is one fixed
depth-only recipe (singly or once per cascade layer), not an application-ordered graph — nor
color render-to-texture beyond the postprocess recipe, load/store policy, compute, read-write
or runtime-sized storage, persistent application-owned buffer handles, arbitrary blend
equations beyond the fixed mode set, instance-rate custom layouts, bind-group abstractions,
new texture formats, packed vertex formats, native mip generation, per-cascade map
resolutions, engine-side cascade selection or blending, or textured cutout shadow casters.

## Evidence

Automated Linux evidence — the material-scene slice on native Wayland, under a KWin resize
storm, and on X11 through XWayland, plus fifty-three conformance cases including the material,
index-width, sampler-mode, blend/depth-mode, mip-chain, shadow, and read-only-storage skinning
cases on both paths, all validation-clean — is recorded in the
[Linux runbook](linux-validation.md).

On 2026-07-20, at `8b7e5c3`, the Metal implementation ran physically on the Apple M2 tier:
`mulciber-shader` regenerated every Metal artifact for the new container natively (the probe
rejected the old-container artifacts with an invalid-header error, as intended), all 34 Metal
conformance cases passed under Metal API Validation, and `mulciber-material-scene` ran
validation-clean (Metal, four samples) through a scripted titlebar close. This is automated
execution evidence; visual confirmation of the material scene remains an operator claim.
Details are in the [macOS runbook](macos-validation.md).

Later on 2026-07-20, at `3ba9d47`, the mip-chain and shadow slices ran on the same M2 tier:
`mulciber-shader` regenerated the changed `lava` and new `shadow` Metal artifacts natively, a
Metal-only clippy failure on the new shadow-pipeline creation function was fixed with the
established long-native-creation allow, all 45 Metal conformance cases (including the three
mip-chain and eight shadow cases) passed under Metal API Validation, and the shadowed
material-scene example ran validation-clean (Metal, four samples) through a scripted titlebar
close. Visual confirmation of the Metal crystal shadows remains an operator claim; the Linux
operator confirmation covers the native Wayland session only.

On 2026-07-20/21, on the working tree committed as `34b8b13` and `687539e`, the cascaded
extension ran on the same M2 tier: `mulciber-shader` regenerated both `lava` artifacts for the
new depth-texture-array interface kind, all 58 Metal conformance cases (including the six new
cascade and render-scale cases) passed under Metal API Validation, and the three-cascade
material-scene example ran validation-clean (Metal, four samples) through a scripted titlebar
close, with agent-captured screenshots showing seam-free moving shadows across the cascade
boundaries. The Vulkan peer implementation passed check and clippy for the Windows target from
the same host; its physical validation-layer, visual, and lifecycle evidence remains
outstanding, per the [macOS runbook](macos-validation.md).

Still later on 2026-07-20, at `97c5a13`, the read-only storage slot ran on the same M2 tier:
`mulciber-shader` generated the new `skinned` and `skinned-shadow` Metal artifacts natively,
another Metal-only clippy line-count failure (on `prepare_material_scene`, which grew the
storage-binding path) took the same established allow, all 52 Metal conformance cases
(including the seven storage cases) passed under Metal API Validation, and the skinned
material-scene example ran validation-clean (Metal, four samples) through a scripted titlebar
close. On the same date, at `cf75bb7`, the operator ran the skinned material scene and visually
confirmed the swaying kelp strand with its shadow animating correctly on both backends;
per-session details stay in the [macOS](macos-validation.md) and [Linux](linux-validation.md)
runbooks.
