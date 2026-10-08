# Per-entry-point resource bindings (unreleased)

One WGSL module can hold several entry points that use different resources, and each pipeline
built from it declares only the bindings its own entry points use. A typical module keeps a plain
and a skinned vertex stage beside one shared fragment stage; the skinned stage reads a bone palette
the plain one never touches:

```wgsl
@group(0) @binding(0) var<uniform> draw: Draw;
@group(0) @binding(1) var albedo: texture_2d<f32>;
@group(0) @binding(2) var albedo_sampler: sampler;
@group(0) @binding(3) var<storage, read> bones: array<mat4x4<f32>, 128>;

fn skin(position: vec3<f32>, index: vec4<u32>, weight: vec4<f32>) -> vec4<f32> {
    let p = vec4<f32>(position, 1.0);
    return (bones[index.x] * p) * weight.x + (bones[index.y] * p) * weight.y
        + (bones[index.z] * p) * weight.z;
}

@vertex fn prop_vertex(in: PropVertex) -> Surface { /* reads draw */ }
@vertex fn skinned_vertex(in: SkinnedVertex) -> Surface { /* reads draw, calls skin */ }
@fragment fn prop_fragment(surface: Surface) -> @location(0) vec4<f32> { /* reads albedo */ }
```

```rust
let shared = [
    MaterialBinding::Uniform { binding: 0, size: DRAW_SIZE },
    MaterialBinding::Texture { binding: 1 },
    MaterialBinding::Sampler { binding: 2, filter: SamplerFilter::Linear, address: SamplerAddress::Repeat },
];
let props = device.create_hdr_material_pipeline(MaterialPipelineDescriptor {
    shader,
    vertex_entry: "prop_vertex",
    fragment_entry: "prop_fragment",
    vertex_layout: PROP_LAYOUT,
    bindings: &shared, // no storage slot
    ..
})?;
let mut skinned_bindings = shared.to_vec();
skinned_bindings.push(MaterialBinding::Storage { binding: 3, size: 128 * 64 });
let skinned = device.create_hdr_material_pipeline(MaterialPipelineDescriptor {
    shader,
    vertex_entry: "skinned_vertex",
    fragment_entry: "prop_fragment",
    vertex_layout: SKINNED_LAYOUT, // e.g. Uint8x4 bone indices and Unorm8x4 weights
    bindings: &skinned_bindings,
    ..
})?;
```

There is no new public API: `MaterialPipelineDescriptor`, `ShadowPipelineDescriptor` and
`MaterialBinding` are unchanged, and only what a declaration must match has narrowed.

## Contract

`mulciber-shader` records, for each entry point, the module bindings it uses. A binding is used
when Naga's analysis finds the global reachable from the entry point, in its own body or in any
function it calls; the SPIR-V and MSL writers emit an entry point's resources from the same
analysis, so the record is exactly the set its native code binds.

`create_material_pipeline` and `create_hdr_material_pipeline` validate the declaration against the
union of the vertex and fragment entry points' bindings, and `create_shadow_pipeline` against the
vertex entry point's plus the fragment entry point's when one is given. The rules inside that set
are the existing ones: every used binding must be declared with its recorded kind and size, and
nothing else may be. Two diagnostics name the difference:

- a declared slot the module records but the pair never uses: `material bindings declare slot 3,
  which the shader module records but entry points `prop_vertex` and `prop_fragment` do not use`;
- a used slot left undeclared: `the shader artifact records binding slot 3 for the pipeline's
  entry points, but the material bindings do not declare it`.

A slot the module does not record at all keeps its `does not record` diagnostic. Bindings no
entry point of the pipeline uses are ignored entirely, including ones outside group 0, so one
module can carry resources for pipelines the vocabulary would refuse to build. Material records
supply what their own pipeline declares, so a plain record passes no storage bytes.

The fixed recipes — postprocess, HDR composite, bloom filter and volumetrics — keep validating the
whole module, since their entry point names and slots are fixed.

## Artifact format

Artifacts are now `MULSHDR3`: each entry point's record gains a `u32` count and that many
strictly ascending `u32` indices into the module's binding table, which follows the entry points
as before and stays sorted by group and binding. `ShaderArtifact::new` rejects an index past the
table, a repeat, or a descending pair as a malformed interface.

`MULSHDR2` artifacts stay readable. They carry no per-entry record, so every entry point of one is
read as using every binding: such a pipeline must still declare the whole module, exactly as
before, and nothing that validated before changes. Regenerate an artifact with this
`mulciber-shader` to scope its pipelines. Older `mulciber` releases reject `MULSHDR3` artifacts
by header rather than misreading them, as the paired tool and crate have always required. The
repository's checked-in artifacts stay `MULSHDR2` until they are next regenerated, which
exercises the compatible path in every existing probe and example.

## Native backends

**Vulkan.** The pipeline's descriptor set layout holds only the declared slots. A module global
that the pipeline's entry points do not use stays declared in the SPIR-V module but is absent from
their `OpEntryPoint` interfaces (SPIR-V 1.4), so no shader stage statically uses it and the layout
need not describe it. Nothing in the Vulkan backend changed.

**Metal.** `mulciber-shader` gives every entry point the module's whole resource map, keyed by
WGSL binding and mapping binding *n* to buffer, texture or sampler index *n*. Naga emits an entry
point function's arguments only for the globals that entry point uses, so `prop_vertex` takes
`[[buffer(0)]]` and `skinned_vertex` takes `[[buffer(0)]]` and `[[buffer(3)]]`: the slots each
pipeline binds are the slots its functions declare, and the shared fragment function is the same
in both. A unit test generates this MSL and checks every entry point's argument list. Nothing in
the Metal backend changed. **The Metal path has not run** for a module with different
per-entry resources: the MSL is checked on Linux, but no metallib has been built or drawn from.

## Validation boundary

Unit tests in `mulciber-shader` cover per-entry recording, including a global reached only
through a called function, and the MSL argument lists per entry point. Unit tests in `mulciber`
cover `MULSHDR3` parsing, malformed usage indices, `MULSHDR2` compatibility, and pipeline
declarations scoped to an entry point pair in both directions.

`mulciber-entry-bindings` is the native probe. One module holds `prop_vertex`, `skinned_vertex`
and a shared `prop_fragment`; the skinned stage reads a 64-byte bone palette through a helper
function with `Uint8x4` bone indices and `Unorm8x4` weights. The probe checks that declaring the
palette for the plain pair and omitting it from the skinned pair are both refused, creates both
pipelines, and reads back three HDR draws: the plain pipeline with no storage, and the skinned one
with two different palettes. On 2026-10-08, Linux / KDE Wayland / NVIDIA GeForce RTX 3060 Ti
(driver 615.71.09), Vulkan with `vulkan-validation` (Khronos validation layer 1.4.363) and no
validation messages, all three draws matched within two half ULPs and both refusals fired. The
`mulciber-cube-texture`, `mulciber-vertex-formats`, `mulciber-float-texture` and
`mulciber-api-conformance` probes, whose artifacts are `MULSHDR2`, passed unchanged. Metal was not
run.

```sh
cargo run -p mulciber-entry-bindings                  # Linux/Windows; Vulkan validation enabled
MTL_DEBUG_LAYER=1 cargo run -p mulciber-entry-bindings  # macOS, once the Metal artifact exists
```

Only the Vulkan artifact is checked in, with its hash in `vulkan-toolchain.lock.toml`. Generate
both on a Mac with:

```sh
cargo run -p mulciber-shader -- build probes/entry-bindings/src/skinning.wgsl --target metal --output probes/entry-bindings/artifacts/skinning.metal.shaderbin
cargo run -p mulciber-shader -- build probes/entry-bindings/src/skinning.wgsl --target vulkan --output probes/entry-bindings/artifacts/skinning.vulkan.shaderbin
```

Writing the probe found a Vulkan bug unrelated to entry points: the record storage region and
three other per-frame regions started with one frame's capacity, so a palette of 256 bytes or less
drawn from the second frame slot wrote past the buffer. It is fixed in its own change; see the
changelog.
