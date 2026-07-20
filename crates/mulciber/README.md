# Mulciber

`mulciber` is the research-stage graphics and presentation layer of the Mulciber native
game-development stack. The project is validating native Vulkan and Metal resource, rendering,
presentation, and lifecycle implementations before it extracts a stable public graphics API.

Version 0.5.0 opens the first application-authored corner of the rendering vocabulary:
`Device::create_material_pipeline` builds a pipeline from an application WGSL shader artifact,
named entry points, a declared vertex layout, slot-explicit uniform/texture/sampler bindings with
per-slot sampler filter and address modes, and a fixed blend/depth mode set (opaque,
alpha-to-coverage cutout, or premultiplied translucent; depth test-write, test-only, or off) —
all validated against the interface recorded in `mulciber-shader` 0.2 artifacts. The artifact
container bump is breaking: older artifacts are rejected and must be regenerated.
`Device::create_mesh_with_layout` uploads raw vertex bytes against declared layouts with 16- or
32-bit indices, and material records supply application-packed uniform bytes per draw. Vulkan now
also implements presentation feedback natively through `VK_EXT_present_timing` where the tier
provides it. It tracks the `mulciber-platform` 0.4 pointer-capture contract. The API remains
research-stage and may change without compatibility guarantees.

Development, design contracts, runnable examples, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
