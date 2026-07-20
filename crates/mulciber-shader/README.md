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

Version 0.3.0 extends the recorded interface with depth-texture and comparison-sampler binding
kinds and with exact byte sizes for read-only, creation-fixed storage bindings; writable and
runtime-sized storage are rejected at compile time instead of being recorded without a proven
mapping. The container is unchanged from 0.2, so earlier artifacts remain loadable, but shaders
using the new slot kinds must be built with this version, whose records `mulciber` 0.6 validates
material and shadow pipeline declarations against. The tool and the `mulciber` crate ship
together; no artifact stability is promised.

Version 0.4.0 additionally records `texture_depth_2d_array` bindings as their own interface
kind for `mulciber` 0.8's `DepthTextureArray` material slot. The container is again unchanged,
so earlier artifacts remain loadable, but modules sampling a depth-texture array must be built
with this version and the paired crate.
