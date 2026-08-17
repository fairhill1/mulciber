# mulciber-shader

`mulciber-shader` is Mulciber's offline single-source shader compiler. It converts one WGSL
module into a cached native Vulkan or Metal artifact; it is a development tool, not an
application runtime dependency.

```console
mulciber-shader build src/scene.wgsl --target vulkan --output artifacts/scene.vulkan.shaderbin
mulciber-shader build src/scene.wgsl --target metal --output artifacts/scene.metal.shaderbin
```

Vulkan generation requires `spirv-val` and validates against `vulkan1.3`. Metal generation runs
on macOS and requires Xcode's `metal` and `metallib` tools. The compiler deliberately accepts
only Naga's validation-backed cross-backend feature intersection; unsupported advanced shaders
fail instead of requesting a second user-authored source.

Each artifact (`MULSHDR2` container) records the module's interface — per entry point its
stage, name, and vertex-input locations with formats, plus every binding's kind: uniform,
texture, sampler, depth texture, depth-texture array, comparison sampler, and read-only
storage with its creation-fixed byte size. The paired `mulciber` crate validates pipeline
declarations against that record. Writable and runtime-sized storage are rejected at compile
time instead of being recorded without a proven mapping. The tool and the `mulciber` crate
ship together; no artifact stability is promised across versions.

## Host-evaluable fields

A game whose simulation needs an answer the shader already computes — the height of a displaced
terrain surface in some direction, say — can generate a host evaluator from the same WGSL instead of
maintaining a second copy in Rust:

```console
mulciber-shader host-field src/field.wgsl --function surface_height --output src/field_host.rs
```

or, keeping the two permanently in step, from a `build.rs`:

```rust
mulciber_shader::compile_host_field(
    "src/field.wgsl",
    std::path::Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("field_host.rs"),
    &["surface_height"],
)
.expect("generate the host field");
```

Each named WGSL function becomes a `pub fn` with its WGSL argument names, `f32`/`i32`/`u32`/`bool`
scalars, `[f32; N]`-shaped vectors, and fixed-size arrays; the functions it calls are generated
privately beside it. Include the file in a module of its own, because the generated helper names are
file-local. The host answer is an ordinary synchronous call with no device involvement.

The accepted subset is pure arithmetic. Bindings, textures, derivatives, atomics, barriers,
workgroup memory, switch statements, and matrices have no host meaning and are refused rather than
approximated. WGSL semantics that differ from Rust's nearest spelling are generated as the shader
computes them: `fract` is `x - floor(x)`, `sign(0.0)` is `0.0`, `round` breaks ties to even,
integer arithmetic wraps, `mix` is `x * (1 - a) + y * a`, and an out-of-range index is clamped the
way Naga's bounds-check policy clamps it. Two differences remain by nature: transcendental
functions differ from a device by its documented precision, and integer division by zero panics on
the host where a GPU leaves it undefined. `mulciber-vulkan-triangle` measures the first of those
against a real device — see the [Linux runbook](../../docs/linux-validation.md).
