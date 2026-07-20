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
