# Offline shader toolchain evaluation: Naga and Slang

This document covers two evaluations: the SPIR-V paths (evaluated 2026-07-16 on Linux) and
the Metal/MSL paths (evaluated 2026-07-16 on macOS, recorded in the
[Metal output evaluation](#metal-output-evaluation) section).

Milestone 2 requires graphics and compute pipelines with offline shader compilation, and
milestone 4 adds bindless resource tables, mesh shading, and hardware ray tracing. This
evaluation measures whether Naga (WGSL) and Slang can produce Vulkan-1.4-valid SPIR-V for
representative Mulciber workloads today, offline, from pinned toolchain revisions.

The harness, corpus, and usage are documented in `tools/shader-toolchain/README.md`. Every
claim below is reproducible from `report.json` in `validation-artifacts/shader-toolchain/`
after a run.

## Exactly what was tested

- Naga 30.0.0 (crates.io, locked in `tools/shader-toolchain/Cargo.lock`), WGSL front end and
  SPIR-V back end used as a library, emitting SPIR-V 1.4, default writer options except for
  the bindless binding map noted below.
- Slang 2026.10.2 (official `slang-2026.10.2-linux-x86_64` release binary), invoked as
  `slangc <source> -target spirv -fvk-use-entrypoint-name -o <output>`, emitting its default
  SPIR-V 1.5.
- Every emitted module was checked with `spirv-val --target-env vulkan1.4` from SPIRV-Tools
  v2026.2, the same SPIRV-Tools release pinned in `vulkan-toolchain.lock.toml`.
- Seven scenario pairs (14 cases): three milestone 2 workload shapes mirroring the probe
  renderer (textured uniform-driven scene, compute with storage buffer/image and workgroup
  barrier, compute-written indexed-indirect arguments) and four milestone 4 capabilities
  (bindless binding arrays with non-uniform indexing, inline ray query, ray-tracing pipeline
  stages, task + mesh shading).
- Evaluated 2026-07-16 on a Linux x86-64 host. This is standalone SPIR-V validation only:
  no driver consumed these modules, no pipelines were created, and nothing was rendered.
  Metal/MSL output paths of both toolchains were not evaluated.

## Results

| Scenario | Milestone | Naga 30.0.0 (SPIR-V 1.4) | slangc 2026.10.2 (SPIR-V 1.5) |
| --- | --- | --- | --- |
| scene | 2 | valid | valid |
| compute_storage | 2 | **invalid** (VUID-10684, workgroup `ArrayStride`) | valid |
| indirect_args | 2 | valid | valid |
| bindless | 4 | valid, fixed-size remap required | valid, true runtime array |
| ray_query | 4 | valid (`SPV_KHR_ray_query`) | valid (`SPV_KHR_ray_query`) |
| ray_pipeline | 4 | valid (`SPV_KHR_ray_tracing`, all four stages) | valid (`SPV_KHR_ray_tracing`, all four stages) |
| mesh | 4 | **invalid** (VUID-09658, duplicate `LocalInvocationIndex`) | valid (`SPV_EXT_mesh_shader`) |

Slang compiled all fourteen entry points across seven modules with zero diagnostics. Naga
passed 5 of 7 scenarios; both failures are SPIR-V back-end code generation defects, not WGSL
language gaps.

## Findings

### Naga

1. **Naga output only validates at SPIR-V 1.4 or lower.** Naga shares layout-decorated
   struct/array types between buffer and `Function` storage classes. SPIR-V 1.5 removed the
   allowance for `Offset`/`ArrayStride` decorations reaching `Function` and `Private`
   variables, so requesting SPIR-V 1.5+ fails `spirv-val` (VUID-StandaloneSpirv-None-10684)
   on ordinary shaders (the scene and ray-query cases). SPIR-V 1.4 is still sufficient for
   `SPV_EXT_mesh_shader` and `SPV_KHR_ray_tracing`, and Vulkan 1.4 consumes any SPIR-V
   version through 1.6, so this is a configuration constraint rather than a blocker.
2. **Naga decorates `Workgroup` storage with explicit layout, which is invalid at every
   SPIR-V version.** Any WGSL shader with a workgroup-shared array or struct — most real
   compute shaders and every mesh shader — currently fails strict validation under
   SPIRV-Tools v2026.2 (the compute_storage failure). Known upstream as
   [gfx-rs/wgpu#7696](https://github.com/gfx-rs/wgpu/issues/7696) with fix PR
   [gfx-rs/wgpu#9295](https://github.com/gfx-rs/wgpu/pull/9295) still open at evaluation
   time. Under Mulciber's every-warning-is-a-failure rule this blocks Naga for
   workgroup-memory compute until the fix ships.
3. **Naga's mesh-shader output is additionally broken by the workgroup zero-init polyfill.**
   With default options the mesh entry point lists two `LocalInvocationIndex` builtin input
   variables (VUID-StandaloneSpirv-OpEntryPoint-09658); with
   `ZeroInitializeWorkgroupMemoryMode::None` that duplicate disappears and the failure
   reduces to finding 2 (layout decorations on the workgroup mesh-output struct). No
   upstream report was found for the duplicate-builtin defect. Task, mesh, and fragment
   stages otherwise parse, validate, and reach `MeshShadingEXT` SPIR-V.
4. **Naga cannot emit true bindless (runtime-sized) descriptor arrays.** The SPIR-V back end
   has no `RuntimeDescriptorArray` path; an unsized `binding_array` compiles only when the
   back-end binding map rewrites it to a fixed size (the harness remaps to 64). Non-uniform
   indexing works (`ShaderNonUniform` + `SPV_EXT_descriptor_indexing`), so milestone 4
   "bindless tables" through Naga are bounded-size tables. Known upstream as
   [gfx-rs/wgpu#7347](https://github.com/gfx-rs/wgpu/issues/7347).
5. **Milestone 4 WGSL is nonstandard.** Bindless, ray query, ray pipelines, and mesh shading
   all require Naga `enable wgpu_*;` extensions (`wgpu_binding_array`, `wgpu_ray_query`,
   `wgpu_ray_tracing_pipeline`, `wgpu_mesh_shader`) plus matching
   `naga::valid::Capabilities` bits. These shaders are Naga-dialect WGSL, not portable
   WebGPU WGSL.
6. On the positive side, Naga's full ray-tracing pipeline path (ray generation, miss,
   any-hit, closest-hit in one module) and inline ray queries compiled and validated
   cleanly, as did the scene, indirect-argument, and remapped bindless scenarios.

### Slang

7. **All seven scenarios produced Vulkan-1.4-valid SPIR-V on the first attempt with zero
   diagnostics**, including true runtime descriptor arrays (`RuntimeDescriptorArray` +
   `NonUniformResourceIndex`), amplification + mesh stages, and all four ray-tracing
   pipeline stages.
8. Two defaults needed correction for Mulciber's purposes: entry points are renamed `main`
   unless `-fvk-use-entrypoint-name` is passed (ambiguous for multi-pipeline modules), and
   `RWTexture2D` needs a `[format("rgba8")]` attribute to avoid relying on the
   `StorageImageWriteWithoutFormat` device feature.
9. Slang is an external native toolchain rather than a Rust crate: reproducibility comes
   from pinning the release binary (2026.10.2 is also packaged on the evaluation host's
   distribution), not from `Cargo.lock`. Its version and hash should join
   `vulkan-toolchain.lock.toml` if adopted.

## Milestone implications

- **Milestone 2 (offline graphics + compute compilation):** Slang covers the workload today.
  Naga covers it except for workgroup-memory compute, which emits standalone-invalid SPIR-V
  until gfx-rs/wgpu#9295 lands; adopting Naga now would mean either accepting that known
  validation failure, post-processing modules, or waiting on upstream.
- **Milestone 4 (bindless, mesh, ray tracing):** Slang expresses all evaluated capabilities
  with valid output. Naga handles ray tracing (both flavors), supports only bounded bindless
  tables, and cannot yet emit valid mesh-shader modules.
- Both toolchains satisfy the offline and pinning requirements; neither result covers driver
  acceptance, pipeline creation, or rendered output, which remain probe work.

## Metal output evaluation

Metal is half of Mulciber's backend surface, and the SPIR-V evaluation above deliberately
excluded both toolchains' Metal paths. This second evaluation measures whether the same
corpus reaches Apple-toolchain-valid Metal Shading Language today.

### Exactly what was tested

- The same Naga 30.0.0 revision, now through its MSL back end as a library
  (`back::msl::write_string`, MSL language version 3.1, `fake_missing_bindings` enabled so
  resource slot assignment does not gate emission validity), and the same Slang 2026.10.2
  release in its macOS arm64 packaging (`slang-2026.10.2-macos-aarch64.zip`, SHA-256
  `5f37e80b16ee332669fa2355485f6cf2795fa5d406bf6e0b1533b3ea2f0e6d76`), invoked as
  `slangc <source> -target metal -fvk-use-entrypoint-name -o <output>`.
- Every emitted MSL module was compiled with Apple's `metal` front end
  (`xcrun metal -c <source> -std=metal3.1`, Apple metal version 32023.620 from Xcode on
  macOS 15.7.7, Apple M2). MSL 3.1 covers mesh shading (3.0) and intersection queries (2.4)
  within Mulciber's Metal 3 baseline.
- The identical seven scenario pairs, driven by the harness's new `--metal` path
  (`--no-spirv` skipped SPIR-V because SPIRV-Tools was not installed on this host).
- Evaluated 2026-07-16. This is standalone front-end acceptance only: no `metallib` was
  shipped, no Metal pipeline was created, and nothing was rendered. A manual
  `xcrun metallib` link of two accepted modules succeeded as a smoke check.
- The emitted sources, compiled objects, `report.json`, and host environment are archived
  as `validation-artifacts/shader-toolchain-metal-20260716-215355.tar.gz` with SHA-256
  `9533244aee2e4269ae98ea454936e00ba5d0e6c8a9be80c9fe03a08ce1a29fc3`.

### Results

| Scenario | Milestone | Naga 30.0.0 (MSL 3.1) | slangc 2026.10.2 (MSL) |
| --- | --- | --- | --- |
| scene | 2 | valid | valid |
| compute_storage | 2 | valid | **failed** (`GetDimensions` unavailable for Metal) |
| indirect_args | 2 | valid | valid |
| bindless | 4 | valid, true unsized argument-buffer array | **failed** (`NonUniformResourceIndex` unavailable; unsized arrays emit invalid MSL) |
| ray_query | 4 | valid (`intersection_query`) | **failed** (`GetDimensions` unavailable) |
| ray_pipeline | 4 | **failed** (MSL back-end `not implemented` panic) | **failed** (no Metal lowering for ray-pipeline stages) |
| mesh | 4 | valid (`[[object]]`/`[[mesh]]`) | **failed** (requires whole-struct output assignment) |

Naga passed 6 of 7; Slang passed 2 of 7. The outcome inverts the SPIR-V table almost
scenario for scenario.

### Findings

1. **Naga's two SPIR-V defects do not exist on its MSL path.** The workgroup-memory layout
   decorations and duplicate mesh builtins are SPIR-V back-end code generation bugs;
   compute_storage and the complete task + mesh + fragment module emit MSL that Apple's
   compiler accepts.
2. **Naga emits true unsized bindless on Metal.** The unsized `binding_array` becomes a
   `constant NagaArgumentBufferWrapper<texture2d<...>>*` argument-buffer pointer with no
   fixed-size remap, the opposite of its SPIR-V limitation. Non-uniform indexing needs no
   decoration in MSL, so the distinction dissolves on this target.
3. **Naga has no MSL ray-pipeline lowering, and the failure mode is a panic.** The writer
   hits an explicit `not implemented` panic rather than returning a backend error; Metal
   itself has no separate ray-pipeline stage model, so the gap is expected even though the
   failure mode is hostile. The harness catches the panic and records it as a finding.
4. **Slang's Metal back end rejects two core intrinsics the corpus relies on.**
   `RWStructuredBuffer.GetDimensions` (used by compute_storage and ray_query) and
   `NonUniformResourceIndex` (used by bindless) both fail entry-point availability checks
   for the `metal` target. Isolated variants confirmed each intrinsic is individually
   sufficient to fail compilation. Mulciber's probe shaders pass sizes through uniforms and
   Metal needs no non-uniform decoration, so both are avoidable in practice, but the same
   portable source cannot currently serve both Slang targets unchanged.
5. **Slang cannot emit valid unsized descriptor arrays for Metal.** Both a plain unsized
   `Texture2D x[]` and the documented `ParameterBlock` argument-buffer form emit a
   flexible array member in a non-final struct position, which Apple's compiler rejects;
   bounded arrays compile. Slang bindless on Metal is therefore bounded-size today,
   mirroring Naga's SPIR-V limitation.
6. **Slang's Metal mesh support requires a different shader idiom.** Assigning mesh output
   vertices member-by-member fails with an explicit "whole struct must be assigned" error;
   a whole-struct variant of the corpus shader compiles for Metal and also emits SPIR-V.
   Re-validating that variant with the pinned `spirv-val` remains pending because
   SPIRV-Tools was not installed on the macOS host, so the checked-in corpus is unchanged.
7. **Both toolchains preserve entry-point names and stage attributes in MSL**, including
   Slang's `[[object]]`/`[[mesh]]`/`[[fragment]]` forms and Naga's qualifier forms, so
   multi-entry-point modules remain addressable for pipeline creation.

### Milestone implications

- **Milestone 2 offline compilation for Metal:** Naga covers the workload shapes today.
  Slang covers them only if shaders avoid `GetDimensions`, which the probe workload already
  does; the corpus scenario fails as written.
- **Milestone 4 for Metal:** Naga reaches mesh shading, inline ray query, and true unsized
  bindless; ray pipelines are out of reach on both (and largely inapplicable to Metal's
  model). Slang currently requires Metal-specific shader idioms and bounded bindless.
- **Cross-backend view:** neither toolchain currently compiles the identical corpus to
  both valid SPIR-V and accepted MSL. Slang is strongest for SPIR-V (7/7) and weak for MSL
  (2/7); Naga is the reverse (5/7 and 6/7, with the SPIR-V failures pending upstream
  fixes). A single-source strategy therefore either constrains shaders to the
  intersection, pairs each backend with its stronger toolchain, or keeps Metal shaders in
  MSL as the probes do today.
- These results cover Apple front-end acceptance only; `metallib` packaging in the build,
  pipeline creation, and rendered output remain probe work.
