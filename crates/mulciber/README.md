# Mulciber

`mulciber` is the research-stage graphics and presentation layer of the Mulciber native
game-development stack. The project is validating native Vulkan and Metal resource, rendering,
presentation, and lifecycle implementations before it extracts a stable public graphics API.

Version 0.6.0 extends 0.5's application-authored material vocabulary along three edges, each
exercised validation-clean on both native backends:
`Device::create_rgba8_srgb_texture_with_mips` uploads an application-supplied full mip chain
(native generation stays deliberately closed) sampled through the declared per-slot filters;
`Device::create_shadow_map` and `Device::create_shadow_pipeline` plus `SceneSubmission::shadow`
compose one fixed depth-only shadow pre-pass whose map material pipelines sample through declared
depth-texture and fixed-recipe comparison-sampler slots; and material and shadow pipelines
declare one read-only storage slot supplied as per-record bytes, opened for skeletal animation's
bone palettes. The new slot kinds validate against the interface recorded by `mulciber-shader`
0.3 artifacts; 0.2-generation artifacts without the new slots remain loadable. It tracks the
`mulciber-platform` 0.4 pointer-capture contract. The API remains research-stage and may change
without compatibility guarantees.

Development, design contracts, runnable examples, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
