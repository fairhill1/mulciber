# mulciber-shader

`mulciber-shader` is Mulciber's offline single-source shader compiler. It converts one WGSL module
into a cached native Vulkan or Metal artifact; it is a development tool, not an application runtime
dependency.

```console
mulciber-shader build src/scene.wgsl --target vulkan --output artifacts/scene.vulkan.shaderbin
mulciber-shader build src/scene.wgsl --target metal --output artifacts/scene.metal.shaderbin
```

Vulkan generation requires `spirv-val` and validates against `vulkan1.3`. Metal generation runs on
macOS and requires Xcode's `metal` and `metallib` tools. The initial compiler deliberately accepts
only Naga's validation-backed cross-backend feature intersection; unsupported advanced shaders fail
instead of requesting a second user-authored source.

Version 0.2.0 records the module's compiler-derived interface — entry points with their stages
and vertex inputs, plus bindings with kinds and uniform byte sizes — in the artifact container,
which `mulciber` 0.5 validates material pipeline declarations against. The container bump is
deliberately breaking: artifacts from earlier versions are rejected at load and must be
regenerated. The tool and the `mulciber` crate ship together; no artifact stability is promised.
