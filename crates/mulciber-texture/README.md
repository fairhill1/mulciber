# mulciber-texture

`mulciber-texture` bakes a game's textures for Mulciber: it builds each texture's mip chain with the
filter its content needs, encodes every level as BC7 with Intel's ISPC encoder (`intel_tex_2`), and
writes a KTX 2.0 file beside the sources that records the digest of what it was built from. At run
time the same crate reads the bake while it is current and otherwise builds the identical chain from
the sources in RGBA8. A texture edited since its bake still shows, and a release that ships only
bakes needs no sources. It is Isle of Rán's texture baker, made game-agnostic.

```console
mulciber-texture bake assets/textures            # bakes stale or missing textures
mulciber-texture bake assets/textures --force    # rebakes everything
mulciber-texture check assets/textures           # lists stale bakes and fails if there are any
```

A game's runtime depends on the crate without the encoder:

```toml
[dependencies]
mulciber-texture = { version = "0.1.0", default-features = false }
```

```rust
use mulciber_texture::{MaterialDefaults, MaterialMaps, Origin};

let maps = MaterialMaps::beside("assets/textures/deco/carpet.png".as_ref(), &MaterialDefaults::default());
let albedo = maps.albedo.prepare()?;           // the bake, or the PNG's chain when it is stale
if let Origin::Sources(why) = &albedo.origin {
    eprintln!("{}: not baked ({why:?})", maps.albedo.output.display());
}
let texture = albedo.upload(&device)?;         // BC7 blocks, or RGBA8 mips
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
| `Linear` | every channel box filtered | BC7 UNORM |

Any size works, not only powers of two: each level halves both axes and floors at one texel, the
rule Mulciber's mip uploads check, and a level that is not a multiple of four is padded for the
encoder by repeating its edge. The encoder uses the slow (best) BC7 settings, in opaque modes when
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
| normal + roughness | `name_normal.png`, `name_rough.png` | tangent-space normal (OpenGL, green up), A = perceptual roughness | `Normal` | `name_normal.ktx2` |
| metallic + occlusion | `name_metal.png`, `name_ao.png` | R = metallic, G = ambient occlusion, B = 0, A = 1 | `Linear` | `name_metal.ktx2` |

The normal + roughness packing is Isle of Rán's. Sources may be `.png`, `.jpg` or `.jpeg`; the
roughness, metallic and occlusion maps are grey. A missing map is filled from `MaterialDefaults`
(roughness 0.8, not metal, unoccluded, by default; a flat normal). A texture with none of its maps is
not baked at all: the game binds a 1×1 texture of `normal_roughness_texel()` or
`metallic_occlusion_texel()` instead, so a material that is only an albedo image costs nothing more.
`find_materials(dir)` lists every albedo image under a directory, which is every image whose stem
does not end in a map suffix.

## Building

The `encode` feature (default) links Intel's encoder, which carries a C++ object; the crate's build
script links the C++ runtime (`stdc++` on Linux, `c++` on macOS) for it. A game's runtime leaves the
feature off. The KTX 2.0 container is read by `mulciber::Ktx2Texture` and written with the `ktx2`
crate's header, level index and data format descriptor serialisers.
