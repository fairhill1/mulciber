# mulciber-texture

`mulciber-texture` bakes a game's textures for Mulciber: it builds each texture's mip chain with the
filter its content needs, encodes every level as BC7 with Intel's ISPC encoder (`intel_tex_2`), and
writes a KTX 2.0 file beside the sources that records the digest of what it was built from. At run
time the same crate reads the bake while it is current and otherwise builds the identical chain from
the sources in RGBA8, leaving the mips to the GPU where its box filter is the chain's (colour in
linear light, linear data). A texture edited since its bake still shows, and a release that ships only
bakes needs no sources. It is Isle of Rán's texture baker, made game-agnostic.

```console
mulciber-texture bake assets/textures            # bakes stale or missing textures
mulciber-texture bake assets/textures --force    # rebakes everything
mulciber-texture check assets/textures           # lists stale bakes and fails if there are any
```

`bake` tries every texture: it prints each one baked and each one that failed with its error, and
exits with failure if any did. A game's own bake binary can forward to `run`, or call
`bake_materials(paths, defaults, BakeMode::IfStale)` for every texture's own result.

A game's runtime depends on the crate without the encoder:

```toml
[dependencies]
mulciber-texture = { version = "0.4.0", default-features = false }
```

```rust
use mulciber_texture::{MaterialDefaults, MaterialMaps, Origin};

let maps = MaterialMaps::beside("assets/textures/deco/carpet.png".as_ref(), &MaterialDefaults::default());
let albedo = maps.albedo.prepare()?;           // the bake, or the PNG's chain when it is stale
if let Origin::Sources(why) = &albedo.origin {
    eprintln!("{}: not baked ({why:?})", maps.albedo.output.display());
}
let texture = albedo.upload(&device)?;         // BC7 blocks, or RGBA8 with GPU mips
let bounce = albedo.stats.mean;                // linear mean colour, for a lightmap's bounce
```

## Recipes and chains

A `Recipe` is four `Channel`s (an image's red, green, blue, alpha or grey value, or a constant), a
`Chain`, and the output path. The chain decides the mip filter and the encoding:

| `Chain` | Mips | Bake |
| --- | --- | --- |
| `Color` | RGB averaged in linear light (sRGB decoded and re-encoded), alpha box filtered | BC7 sRGB |
| `ColorFlatDistant` | `Color`, its last five levels pulled toward the average (tiled ground) | BC7 sRGB |
| `Cutout` | `Color`, each level's alpha rescaled to keep the base level's covered fraction | BC7 sRGB |
| `Normal` | RGB averaged as unit vectors and renormalised, alpha box filtered | BC7 UNORM |
| `NormalRoughness` | `Normal`'s normals, with alpha as perceptual roughness widened by how much each level's base normals disagree (Toksvig, through a von Mises–Fisher fit), so distant bumps read rough rather than shiny | BC7 UNORM |
| `Linear` | every channel box filtered | BC7 UNORM |

Any size works, not only powers of two, but for a few just past one: each level halves both axes
and floors at one texel, the rule Mulciber's mip uploads check, and a level that is not a multiple
of four is padded for the encoder by repeating its edge. An extent Apple GPUs misread a level of
(`mulciber::misread_block_mip`, 4608×576 for one) is refused before encoding, the error naming the
nearest whole-block extents that are sampled right; `refuse_misread_chain(width, height)` asks the
same, and `ktx2_bytes` refuses to write such a chain. The encoder uses the slow (best) BC7 settings, in opaque modes when
every texel is opaque.

A bake's key/value data carries `MulciberSourceDigest` (FNV-1a over the bake version, the chain, the
packing and the sources' bytes, not their paths), `MulciberMean` (the base level's mean RGBA, RGB in
linear light for sRGB chains) and `MulciberMinAlpha`. The mean and minimum alpha let a game use a bake
without decoding it: the mean is what a lightmap bounces off the surface, and the minimum alpha says
whether it is see-through.

## Materials

`MaterialMaps::beside(albedo, defaults)` is a physically based material as three textures, found
beside its albedo image by suffix:

| Texture | Sources | Channels | Chain | Bake |
| --- | --- | --- | --- | --- |
| albedo | `name.png` | sRGB colour, A = opacity | `Color` | `name.ktx2` |
| normal + roughness | `name_normal.png`, `name_rough.png` | tangent-space normal (OpenGL, green up), A = perceptual roughness | `NormalRoughness` | `name_normal.ktx2` |
| metallic + occlusion | `name_metal.png`, `name_ao.png` | R = metallic, G = ambient occlusion, B = 0, A = 1 | `Linear` | `name_metal.ktx2` |

The normal + roughness packing is Isle of Rán's. Sources may be `.png`, `.jpg` or `.jpeg`; the
roughness, metallic and occlusion maps are grey. A missing map is filled from `MaterialDefaults`
(roughness 0.8, not metal, unoccluded, by default; a flat normal). A texture with none of its maps is
not baked at all: the game binds a 1×1 texture of `normal_roughness_texel()` or
`metallic_occlusion_texel()` instead, so a material that is only an albedo image costs nothing more.
`find_materials(dir)` lists every albedo image under a directory, which is every image whose stem
does not end in a map suffix.

## Texture arrays

`ArrayRecipe::new(layers, output)` takes several recipes of one `Chain` and bakes them as the layers
of one 2D texture array in one `.ktx2`, for a shader to sample them all through one
`MaterialBinding::TextureArray` slot (a terrain's ground materials, say). `bake_array(&array, mode)`
builds each layer's chain, refuses layers of different extents, encodes every level as BC7 and
writes the file with a digest over every layer's sources. `ArrayRecipe::prepare` reads the bake
while it is current and otherwise builds every layer's whole chain from its sources in RGBA8 on the
CPU, since the GPU does not generate an array's mips; `PreparedArray::upload` uploads either. The
bake's `MulciberMean` holds four numbers per layer and `MulciberMinAlpha` one per layer, read back
as `PreparedArray`'s `stats`, one per layer. `ktx2_array_bytes` and `write_ktx2_array` write a chain
per layer the game encoded itself. See [2D texture arrays](../../docs/texture-arrays.md).

```rust
let ground = ArrayRecipe::new(vec![grass, dirt, rock], "assets/ground.ktx2")?;
let texture = ground.prepare()?.upload(&device)?;
```

## Light

Light the game bakes itself (a sky, a lightmap, a reflection probe) is linear RGB in floats above
one, which BC7 cannot hold. `hdr_chain(width, height, texels)` builds its complete mip chain, each
level the mean of 2×2 texels of the one above (odd extents clamp the footprint), and
`encode_bc6h(width, height, texels)` (the default `encode` feature) encodes one level as BC6H
unsigned half float for `BlockCompression::Bc6hUfloat`: a byte a texel, an eighth of `RGBA16Float`.
Values clamp to 0 and `HDR_MAX` (65504), alpha is dropped, a non-finite texel is an error naming it,
and a level that is not a multiple of four is padded by repeating its edge. The encoder is ISPC's,
at its basic quality, spread over the machine's threads. `ktx2_bytes(BlockCompression::Bc6hUfloat,
…)` writes the chain to KTX 2.0. There is no recipe or source image for light: the game computes it,
so it calls these directly.

## SVG sheets

The `svg` feature (off by default) adds `SvgSheet`: one SVG of a HUD's pictures, each a group by id
drawn about its own origin wherever the sheet places it, rasterised with resvg at the size it's
shown, so the HUD is crisp at any window size and has no bake to keep in step with its source.
`SvgSheet::read(path, required)` fails on loading if a picture the game needs is missing.
`draw(ids, width, height, scale, under)` draws the pictures in order, their origin at the canvas's
middle and `scale` pixels to a unit, over premultiplied RGBA or a clear canvas, and returns
straight-alpha sRGB RGBA, ready for `mulciber_text::AtlasBuilder::image`. `gradient(id)` gives a
linear gradient's stops, for shapes drawn in code to match the sheet. Text is not drawn: convert the
sheet's type to paths.

```toml
mulciber-texture = { version = "0.6.2", default-features = false, features = ["svg"] }
```

## Building

The `encode` feature (default) links Intel's encoder, which carries a C++ object; the crate's build
script links the C++ runtime (`stdc++` on Linux, `c++` on macOS) for it. A game's runtime leaves the
feature off. The KTX 2.0 container is read by `mulciber::Ktx2Texture` and written with the `ktx2`
crate's header, level index and data format descriptor serialisers.
