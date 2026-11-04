# Anisotropic filtering (graphics 0.20.0)

Trilinear filtering picks a texture's mip level from the longer side of a pixel's footprint. Where
a surface recedes from view (ground, a floor, a deck, water toward the horizon) the footprint is
long and thin, so trilinear reads a level blurred enough for the long side and the whole surface
goes soft. Anisotropic filtering takes several samples along the long side and picks the level
from the short one, keeping it sharp.

## Declaring it

`SamplerFilter::Anisotropic { max }` is `SamplerFilter::Linear` taking up to `max` samples along a
receding surface, declared on `MaterialBinding::Sampler` or `MaterialBinding::SamplerPerAxis` like
any other filter:

```rust
MaterialBinding::Sampler {
    binding: 2,
    filter: SamplerFilter::Anisotropic { max: 16 },
    address: SamplerAddress::Repeat,
}
```

- `max` runs from 1 (plain linear filtering) to `SAMPLER_ANISOTROPY_LIMIT` (16), the most either
  backend filters. Anything else is `InvalidRequest` when the pipeline is created, or by
  `MaterialPipelineDescriptor::validate`.
- A device that filters fewer samples uses as many as it has: Vulkan clamps to the adapter's
  `maxSamplerAnisotropy`. One without the optional `samplerAnisotropy` feature filters linearly
  rather than refusing, since the result is only less sharp. Every Metal device filters 16.
- Shaders don't change: `textureSample` and `textureSampleGrad` on that sampler filter
  anisotropically, and `textureSampleLevel` names its level, so there's nothing to filter.
- `SamplerFilter` is now `#[non_exhaustive]`, so a later filter is not another breaking change.

Use it where textures are seen at grazing angles: terrain, floors, roads, water. A HUD, a
lightmap, a sky panorama or a texture seen face-on gains nothing from it.

## Native mapping

Vulkan enables `samplerAnisotropy` at device creation where the adapter reports it, and each
anisotropic slot's sampler sets `anisotropyEnable` and `maxAnisotropy` to `max`, clamped to
`maxSamplerAnisotropy`. With linear minification, magnification and mip filtering, as
`SamplerFilter::Linear`. Metal sets `MTLSamplerDescriptor.maxAnisotropy` to `max` on the same
linear sampler. Material and shadow pipelines both take it.

## Evidence

`probes/anisotropic-filter` (`mulciber-anisotropic-filter`) samples a 64×64 `RGBA16Float` texture
whose every texel of level n holds n, through a trilinear sampler and anisotropic samplers of 4
and 16 samples, with `textureSampleGrad` gradients describing a footprint. Red read back from an
HDR target is the level each sampler chose:

| Footprint (texels) | Trilinear | 4 samples | 16 samples |
| --- | --- | --- | --- |
| point | 0 | 0 | 0 |
| 8 × 8 | 3 | 3 | 3 |
| 1 × 8 | 3 | 1 | 0 |
| 1 × 32 | 5 | 3 | 1 |

These are the levels the specification's ideal filter chooses. How a GPU approximates the
footprint is up to it, so the probe checks only what any anisotropic filter must do: an isotropic
footprint reads the same level through every sampler, a stretched one reads at least a level
sharper through 4 samples than trilinear and sharper again through 16. It also checks that 0 and
17 samples are refused.

On 2026-11-04 every one of the twelve cases read exactly the level in the table, and both refusals
fired, on Vulkan with Khronos validation reporting nothing, on an NVIDIA GeForce RTX 3060 Ti
(driver 615.78.08, Vulkan 1.4.351) on CachyOS Linux. That is the only run. The Metal path is type-
and lint-checked for `aarch64-apple-darwin` but has never run, and the probe's Metal artifact is
not generated. The linear fallback on an adapter without `samplerAnisotropy` is unit-tested but
has not been reached on a device. No image of a receding surface was looked at; the evidence is
the levels read back.

```sh
cargo run -p mulciber-anisotropic-filter                    # Linux/Windows; Vulkan validation enabled
MTL_DEBUG_LAYER=1 cargo run -p mulciber-anisotropic-filter  # macOS, once the artifact exists
```

The sample shader is `probes/anisotropic-filter/src/sample.wgsl`; the composite pass is the
float-texture probe's. The Vulkan artifact is checked in with its hashes in
`vulkan-toolchain.lock.toml`. Generate the Metal one on a Mac with:

```sh
cargo run -p mulciber-shader -- build probes/anisotropic-filter/src/sample.wgsl --target metal --output probes/anisotropic-filter/artifacts/sample.metal.shaderbin
```
