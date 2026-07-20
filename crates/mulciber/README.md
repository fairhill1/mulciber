# Mulciber

`mulciber` is the research-stage graphics and presentation layer of the Mulciber native
game-development stack. The project is validating native Vulkan and Metal resource, rendering,
presentation, and lifecycle implementations before it extracts a stable public graphics API.

Version 0.8.0 extends the material vocabulary along three edges, each exercised
validation-clean on Metal with the Vulkan peer compile-verified: the shadow pre-pass composes
as a cascaded prepass (`Device::create_shadow_map_array`, `ShadowPrepass::Cascaded` with one
depth-only record list per layer, and a declared `DepthTextureArray` slot fed through
`ShadowSource`), with cascade policy — splits, light matrices, texel snapping, bias, selection
— deliberately application-owned; postprocess targets accept a validated render scale (25
through 200 percent) decoupling the offscreen scene extent from the presentable extent through
the existing fullscreen resample; and material records supply their geometry through
`GeometrySource` — an uploaded mesh, or frame-transient vertex and index bytes (capped at 4
MiB per record) for per-frame-authored overlays such as HUDs. The reshaped `MaterialRecord`
and `SceneSubmission.shadow` are breaking; depth-texture-array modules require paired
`mulciber-shader` 0.4 artifacts, while earlier artifacts remain loadable.

Version 0.7.0 carried no graphics API change; it moved to the `mulciber-platform` 0.5
contract, whose window-mode intent adds fullscreen to the platform layer this crate's surface
creation consumes, so one dependency tree can hold both crates.

Version 0.6.0 extended 0.5's application-authored material vocabulary along three edges, each
exercised validation-clean on both native backends:
`Device::create_rgba8_srgb_texture_with_mips` uploads an application-supplied full mip chain
(native generation stays deliberately closed) sampled through the declared per-slot filters;
`Device::create_shadow_map` and `Device::create_shadow_pipeline` plus `SceneSubmission::shadow`
compose one fixed depth-only shadow pre-pass whose map material pipelines sample through declared
depth-texture and fixed-recipe comparison-sampler slots; and material and shadow pipelines
declare one read-only storage slot supplied as per-record bytes, opened for skeletal animation's
bone palettes. The new slot kinds validate against the interface recorded by `mulciber-shader`
0.3 artifacts; 0.2-generation artifacts without the new slots remain loadable. The API remains
research-stage and may change without compatibility guarantees.

Development, design contracts, runnable examples, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
