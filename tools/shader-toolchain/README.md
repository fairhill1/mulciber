# Shader toolchain evaluation

`mulciber-shader-toolchain` compiles a representative shader corpus through two candidate
offline toolchains and validates every emitted module against Vulkan 1.4:

- **Naga** (`naga` crate, WGSL front end, SPIR-V back end), used as a library.
- **Slang** (`slangc`), invoked as an external compiler.

The corpus lives in `shaders/wgsl/` and `shaders/slang/` as seven scenario pairs. Milestone 2
scenarios mirror the probe workloads (uniform-driven textured scene, compute with storage
buffer/image and a workgroup barrier, compute-written indexed-indirect arguments). Milestone 4
scenarios exercise bindless binding arrays with non-uniform indexing, inline ray queries,
ray-tracing pipeline stages, and task + mesh shading.

Like `tools/vulkan-bindgen`, this is an isolated Cargo workspace so evaluation dependencies
never enter Mulciber's runtime dependency graph.

## Usage

`slangc` and the SPIRV-Tools binaries (`spirv-val`, `spirv-dis`) must be available. `slangc`
is discovered on `PATH` or through the `SLANGC` environment variable:

```sh
SLANGC=/path/to/slang/bin/slangc \
  cargo run --manifest-path tools/shader-toolchain/Cargo.toml
```

The run writes every compiled `.spv` module and a machine-readable `report.json` (toolchain
versions, per-case diagnostics, `spirv-val` verdicts, SPIR-V versions, entry points,
capabilities, and extensions) to `validation-artifacts/shader-toolchain/`, and prints a
summary table. Per-scenario compilation or validation failures are findings, not harness
errors; the process still exits successfully and preserves the failure text.

Pass `--metal` to also compile both corpora to Metal Shading Language (Naga's MSL backend
with explicit resource slots and `slangc -target metal`). Every accepted source is compiled
at `-std=metal3.1` and linked into a `.metallib`; accepted milestone-2 functions must also
create native Metal render or compute pipelines. This path requires a macOS host with
Xcode's Metal toolchain and writes the emitted `.metal` sources, compiled `.air` objects,
linked `.metallib` files, and `metal_cases` report entries alongside the SPIR-V artifacts.
Pass `--no-spirv` to skip the SPIR-V path on hosts without SPIRV-Tools:

```sh
SLANGC=/path/to/slang/bin/slangc \
  cargo run --manifest-path tools/shader-toolchain/Cargo.toml -- --metal --no-spirv
```

The harness removes each case's previous outputs before attempting it, so a failed case
cannot leave a stale artifact in a later evidence archive. Pipeline creation is stronger
than front-end acceptance, but the harness does not bind resources, dispatch compute work,
or render; those remain probe responsibilities.

Recorded findings and their milestone implications are written up in
`docs/shader-toolchain-evaluation.md`.
