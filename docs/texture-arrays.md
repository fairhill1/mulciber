# 2D texture arrays (graphics 0.19.0, shader 0.5.9, texture 0.7.0)

A 2D texture array is several images of one extent, format and chain behind one material slot, the
shader picking one by index: a terrain's ground materials, a set of decals, a sprite sheet's
frames. A game that binds a slot per image runs into `MATERIAL_TEXTURE_COUNT_LIMIT` (16) and
rebinds per draw; an array costs one slot for up to the device's layer limit.

## Uploading

`TextureShape::D2Array { width, height, layers }` uploads `layers` `width`×`height` images, layer
after layer, as one `Texture` with `dimension() == TextureDimension::D2Array`:

- Every `TexelData` format a 2D texture takes is accepted: `Rgba8Unorm`, `Rgba8Srgb`,
  `Rgba16Float`, `Rgba16FloatBits` and each `BlockCompression`.
- `MipChain::Single` takes one image per layer, `MipChain::Supplied` a complete chain per layer,
  layer after layer (layer 0's levels, then layer 1's). `MipChain::Generated` is `Unsupported`,
  being for single 2D images only.
- No layers is `InvalidRequest`. A wrong image count says how many the shape takes; a wrong level
  names its layer (`2D array layer 2: ...`).
- More layers than the device holds is `Unsupported`: Vulkan's `maxImageArrayLayers` (at least 256
  by the specification) or Metal's 2048.
- `Device::update_texture` replaces 2D textures only; an array is refused with `InvalidRequest`.

```rust
let texture = device.create_texture(&TextureDescriptor {
    shape: TextureShape::D2Array { width: 256, height: 256, layers: 4 },
    data: TexelData::Rgba8Srgb(&[&grass, &dirt, &rock, &snow]),
    mips: MipChain::Single,
})?;
```

## Binding and sampling

The array binds only through `MaterialBinding::TextureArray { binding }`, declared by WGSL
`texture_2d_array<f32>`. `mulciber-shader` records the slot as its own binding kind (10), so a
pipeline whose shader declares a `texture_2d_array<f32>` where the descriptor says `Texture`, or
the other way round, is refused when the pipeline is created, and a record supplying a 2D texture
for an array slot (or an array for a 2D slot) is refused when the frame is submitted. On Metal the
array takes a texture slot like any other texture.

```wgsl
@group(0) @binding(1) var ground: texture_2d_array<f32>;
@group(0) @binding(2) var ground_sampler: sampler;

let colour = textureSample(ground, ground_sampler, uv, layer);
```

`textureSampleLevel(map, sampler, uv, layer, lod)` picks a level, and `textureNumLevels` and
`textureNumLayers` report the chain and layer counts. Filtering never blends between layers.

## KTX 2.0

`Ktx2Texture::parse` reads a 2D array file (`layerCount` ≥ 1, one face, no depth), whose level
holds every layer's image at that level one after the other. `layer_count()` is `Some(n)` for an
array file and `None` for a plain 2D one; `image(layer, level)` returns one image; `descriptor()`
gives `TextureShape::D2Array` for an array file, so it uploads as written.

## Baking (mulciber-texture 0.7.0)

`ArrayRecipe::new(layers, output)` takes several `Recipe`s with one chain and writes them as one
`.ktx2`. `bake_array` (with `encode`) builds each layer's chain, refuses layers of different
extents, encodes every level as BC7 and writes the file with `ktx2_array_bytes`/`write_ktx2_array`
and a digest over every layer's sources. At run time `ArrayRecipe::prepare` reads the bake when it
is current, and otherwise builds every layer's whole chain from its sources in RGBA8 on the CPU,
since the GPU does not generate an array's mips; `PreparedArray::upload` uploads either with a
supplied chain. The bake's
`MulciberMean` holds four numbers per layer and `MulciberMinAlpha` one per layer, and
`PreparedArray`'s `stats` gives them back per layer.

## Evidence

`probes/texture-array` (`mulciber-texture-array`) samples arrays through `MaterialBinding::TextureArray`
and reads the samples back from an HDR target:

- an `RGBA16Float` 4×4 array of five layers with three levels, every layer at every level;
- an `RGBA8Unorm` 4×2 array, checking orientation within a layer;
- a BC7 array of three layers and four levels baked by `mulciber-texture` into one KTX 2.0 file,
  uploaded through `Ktx2Texture::descriptor` and checked against the independent decoder `bcdec`;
- `textureNumLevels` and `textureNumLayers`;
- the refusals: no layers, a missing layer counted, a NaN texel naming its layer, generated mips,
  an update of the array, a pipeline declaring a 2D texture where its shader has an array, and a
  record supplying a 2D texture for the array slot.

On 2026-11-04 it passed all 46 sampled cases and every refusal on Vulkan, with Khronos validation
reporting nothing, on an NVIDIA GeForce RTX 3060 Ti (driver 615.78.08, Vulkan 1.4.351) on
CachyOS Linux. That is the only run. The Metal path is type- and lint-checked for
`aarch64-apple-darwin` but has never been run, and the probe's Metal artifact has not been
generated, so on macOS the probe builds and then stops at startup saying the artifact is missing.
The layer-limit refusal has not been reached on any device.

```sh
cargo run -p mulciber-texture-array                    # Linux/Windows; Vulkan validation enabled
MTL_DEBUG_LAYER=1 cargo run -p mulciber-texture-array  # macOS, once the artifact exists
```

The sample shader is `probes/texture-array/src/sample.wgsl`; the composite pass is the
float-texture probe's. The Vulkan artifact is checked in with its hashes in
`vulkan-toolchain.lock.toml`. Generate the Metal one on a Mac with:

```sh
cargo run -p mulciber-shader -- build probes/texture-array/src/sample.wgsl --target metal --output probes/texture-array/artifacts/sample.metal.shaderbin
```
