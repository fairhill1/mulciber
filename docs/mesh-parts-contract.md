# Experimental shared-vertex mesh-parts contract

This checkpoint adds immutable indexed parts to one owning mesh. It is forced by Skyrimlike
character dismemberment, but the API is ordinary indexed-geometry vocabulary suitable for GLTF
primitives, material sections, LOD/index variants, and destructible geometry.

## Forcing workload and decision

Skyrimlike needs four immutable triangle selections for one skinned character vertex set: intact,
left arm hidden, right arm hidden, and both arms hidden. Its current shader-side hiding widened the
skinned vertex stride from 64 to 68 bytes, expanded vertices along anatomical triangle boundaries,
widened instance data from 8 to 12 bytes, and added visibility processing plus fragment discard to
material and shadow shaders. With 15–30 visible animated NPCs, the measured cost was roughly
1.5–1.7% of total GPU frame time and 12–16% of the shadow pass.

The selected abstraction is one immutable vertex owner plus immutable indexed parts. The
application classifies and builds indices, groups instances by the selected variant, and submits one
part per group. Mulciber does not know body parts, hidden masks, anatomical classification, or
dismemberment events.

## Public vocabulary

- `Device::create_mesh_with_parts(&[Vertex], &[MeshIndices])` uploads the fixed vertex layout once
  with one or more index parts.
- `Device::create_mesh_with_layout_and_parts(VertexLayout, &[u8], &[MeshIndices])` is the raw-layout
  form. Each part may independently use `MeshIndices::U16` or `MeshIndices::U32`.
- Existing `create_mesh` and `create_mesh_with_layout` create the ordinary one-part case. Part zero
  is their default and remains selected by existing whole-mesh draw APIs and
  `GeometrySource::Mesh(&mesh)`.
- `Mesh::part(index)` validates the number and returns a borrowed, `Copy` `MeshPart`. `part_count`
  exposes the immutable count. A bad number returns `GraphicsErrorKind::InvalidRequest` naming the
  requested number and available count.
- A material record selects a non-default part with `GeometrySource::MeshPart(mesh.part(index)?)`.
  A shadow record selects its default parent or a part through
  `MeshSource::Mesh(&mesh)` / `MeshSource::MeshPart(mesh.part(index)?)`.

Part selection changes only the bound index region, count, and native index type. It does not enter
pipeline, texture, sampler, or descriptor identity, and both non-instanced and instance-layout
material and shadow pipelines use the same selection.

## Ownership and validation

`Mesh` remains the only owning lease and generational resource identity. `MeshPart` contains a
borrow of that parent plus a part number; it owns no lease, native allocation, or deferred-drop
entry. Rust prevents moving the parent into explicit destruction while a part is borrowed. Dropping
or explicitly destroying the parent retires its vertex data, every index region, and every indirect
command together after the established completed-frame boundary. Mixed-session, stale-generation,
and vertex-layout validation all inspect the parent identity.

Creation rejects empty or stride-ragged vertex data, no parts, an empty part, an index outside the
shared vertex count, counts outside backend integer limits, and checked-arithmetic or allocation
overflow. Diagnostics identify the invalid part. `Mesh::part` rejects invalid selection before a
submission can consume its acquired frame. Submission validates the parent session and layout
before taking the frame token.

## Native implementation

Vulkan keeps one suballocation in the existing coalescing mesh arena. The vertex region comes first;
each index region begins at a four-byte-aligned offset, followed by one aligned
`VkDrawIndexedIndirectCommand` per part, and the complete parent allocation is 16-byte aligned.
Encoding binds the parent vertex offset and the selected part's index offset,
`VkIndexType`, count, and indirect-command offset. Instanced records use the selected count in
`vkCmdDrawIndexed`; non-instanced records retain the established indirect path.

Metal uses one `MTLBuffer` for the parent. It stores the vertices once, then four-byte-aligned index
regions and one indirect argument per part, with the complete storage length aligned to 16 bytes.
Encoding binds that buffer once as vertex storage and selects the part's index offset,
`MTLIndexType`, count, and indirect offset. Instanced records retain direct indexed-instanced draws;
non-instanced records retain indirect indexed draws.

## Deliberate exclusions

This checkpoint adds no mutable mesh, public buffer API, render graph, bind-group layer,
application-managed synchronization, per-vertex part attribute, per-instance visibility mask,
shader branch/varying/discard requirement, automatic instance grouping, anatomical policy, detached
limb asset handling, or stump-cap generation. Transient geometry remains the separate
per-frame-authored overlay/debug path and is not a mesh-part owner.

## Evidence

On 2026-07-22, the native-Wayland Vulkan conformance probe passed all 94 asserted cases on the
Linux/Nvidia RTX 3060 Ti tier with `VK_LAYER_KHRONOS_validation` 1.4.350 enabled and no validation
message. The additions cover missing/empty/out-of-range parts, invalid selection before acquisition,
selected-part layout and mixed-session rejection, mixed `u16`/`u32` material and shadow parts,
instanced selected-part material and shadow draws, explicit destruction, and 32 rounds of
multi-part create/drop churn. The visible workload forms one quad from two separately submitted
triangle parts sharing four vertices; this automated run is validation evidence, not an
operator-inspected visual-correctness claim.

Focused Vulkan and Metal backend tests pin the two mixed-width index offsets, counts, native types,
indirect offsets, and alignment. The complete workspace cross-target checks for
`aarch64-apple-darwin` with `MULCIBER_METAL_TYPECHECK_ONLY=1`; Metal cannot execute on this Linux
machine, so native Metal API Validation and visual evidence remain outstanding under the
[macOS runbook](macos-validation.md).

## Skyrimlike integration target

This API is the `mulciber` 0.13.0 contract. The release is intentionally minor-version-breaking in
the crate's pre-1.0 series because `ShadowRecord.mesh: &Mesh` becomes
`ShadowRecord.geometry: MeshSource`; existing `create_mesh`, `create_mesh_with_layout`, and
`GeometrySource::Mesh(&mesh)` call sites remain unchanged.

Skyrimlike can restore its 64-byte skinned vertex and 8-byte instance formats; remove `body_part`,
`hidden_parts`, the visibility varying, and fragment discard; build its four index variants once;
group visible NPC instances by variant; and use the corresponding `MeshPart` in both material and
shadow records. Regrouping does not alter the existing bone-palette storage bytes or arbitrary
`bone_base` offsets. The intact/common group remains one indexed instanced draw per existing storage
batch. Detached limbs and stump caps remain event-only game assets outside Mulciber.
