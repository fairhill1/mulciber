# Cube textures (unreleased)

A cube texture is six square faces of one extent, each with its own mip chain, sampled by
direction. Environment reflections are the motivating use: a game's baked environment cubemaps
upload as stored and a material samples them along the reflected view vector.

`Device` gains a cube constructor beside every 2D sampled upload, and each returns the existing
owning `Texture`, whose `dimension()` is `TextureDimension::Cube`:

| Format | Single level | Complete mip chain per face |
|---|---|---|
| RGBA8 sRGB | `create_rgba8_srgb_cube_texture(size, [&[u8]; 6])` | `create_rgba8_srgb_cube_texture_with_mips(size, [&[&[u8]]; 6])` |
| RGBA8 UNORM | `create_rgba8_unorm_cube_texture(size, [&[u8]; 6])` | `create_rgba8_unorm_cube_texture_with_mips(size, [&[&[u8]]; 6])` |
| Any `BlockCompression` (BC1, BC2, BC3, BC5, BC7) | `create_block_compressed_cube_texture(compression, size, [&[u8]; 6])` | `create_block_compressed_cube_texture_with_mips(compression, size, [&[&[u8]]; 6])` |
| `RGBA16Float` | `create_rgba16_float_cube_texture(size, [&[[f32; 4]]; 6])` | `create_rgba16_float_cube_texture_with_mips(size, [&[&[[f32; 4]]]; 6])` |

Every constructor returns `Result<Texture, GraphicsError>`, and `destroy_texture` and drop
reclamation release a cube exactly as they release a 2D texture.

## Binding and WGSL

A material pipeline declares a cube slot with `MaterialBinding::CubeTexture { binding }` and samples
it through an ordinary `MaterialBinding::Sampler`. The WGSL is `texture_cube<f32>` with a
three-component direction, which need not be normalized:

```wgsl
@group(0) @binding(3) var environment: texture_cube<f32>;
@group(0) @binding(4) var environment_sampler: sampler;

@fragment fn shade(in: Surface) -> @location(0) vec4<f32> {
    let reflected = reflect(-in.view_direction, normalize(in.normal));
    let specular = textureSample(environment, environment_sampler, reflected).rgb;
    // textureSampleLevel(environment, environment_sampler, reflected, lod) picks a level.
    ...
}
```

```rust
use mulciber::{BlockCompression, MaterialBinding, SamplerAddress, SamplerFilter};

let environment = device.create_block_compressed_cube_texture_with_mips(
    BlockCompression::Bc1Srgb,
    64,
    [&right, &left, &top, &bottom, &front, &back], // +X, -X, +Y, -Y, +Z, -Z level chains
)?;
let bindings = [
    MaterialBinding::Uniform { binding: 0, size: 128 },
    MaterialBinding::Texture { binding: 1 },
    MaterialBinding::Sampler {
        binding: 2,
        filter: SamplerFilter::Linear,
        address: SamplerAddress::Repeat,
    },
    MaterialBinding::CubeTexture { binding: 3 },
    MaterialBinding::Sampler {
        binding: 4,
        filter: SamplerFilter::Linear,
        address: SamplerAddress::ClampToEdge,
    },
];
// The record supplies 2D and cube textures together in ascending binding order.
let record = MaterialRecord { textures: &[&albedo, &environment], /* ... */ };
```

`mulciber-shader` records `texture_cube<f32>` as its own binding kind, so pipeline creation
refuses a `Texture` declaration of a cube binding and a `CubeTexture` declaration of a 2D one,
naming the slot. Submission refuses a record that supplies a 2D texture for a cube slot or a cube
for a 2D slot, naming the texture's position in `textures`. Cube slots share the texture slot
range and `MATERIAL_TEXTURE_COUNT_LIMIT` with 2D slots. Arrayed cubes (`texture_cube_array`),
depth cubes and integer cubes have no proven mapping and fail to compile.

A cube texture feeds material pipelines only. The fixed textured pipelines (`TexturedDraw`,
`TexturedSceneDraw`, `TexturedInstanceBatch`, `PostprocessedDraw`) refuse it, shadow pipelines
refuse a cube slot and shadow records a cube texture, and `update_rgba16_float_texture` and
`update_rgba16_float_texture_with_mips` replace 2D textures only.

## Input contract

`faces` is in the standard layer order +X, -X, +Y, -Y, +Z, -Z that Vulkan, Metal and Direct3D
share, and each face is laid out as the 2D constructor of its format takes a level: row-major
RGBA8 texels, whole 4×4 blocks (`ceil(w / 4) × ceil(h / 4)`, eight bytes for BC1 and sixteen for
the rest), or RGBA f32 texels converted to binary16 exactly as the 2D float upload converts them.
`size` is the edge of every face, so faces are square and equal by construction; it must be
nonzero. A mip method needs every face to supply the complete chain from `size`×`size` to 1×1,
halving and flooring at one as the 2D chains do. A face or level whose byte count does not match
its extent is `InvalidRequest`, naming the face and level (`cube face -Z mip level 2 supplies 8
bytes but its 2x2 extent needs 16`); the summed staging size is checked before any native
allocation. Mulciber never generates mips or filters across face edges; that is the application's,
as for 2D chains.

Within a face, the first texel of the first row is where the native APIs put it: sampling
direction `(x, y, z)` selects the face of its largest-magnitude axis and reads texel coordinates
`s = (sc / |ma| + 1) / 2`, `t = (tc / |ma| + 1) / 2`, with `(sc, tc)` from the cube map face
selection table in the Vulkan specification:

| Face | Major axis `ma` | `sc` | `tc` |
|---|---|---|---|
| +X | `x` | `-z` | `-y` |
| -X | `x` | `+z` | `-y` |
| +Y | `y` | `+x` | `+z` |
| -Y | `y` | `+x` | `-z` |
| +Z | `z` | `+x` | `-y` |
| -Z | `z` | `-x` | `-y` |

Mulciber does not convert between coordinate conventions. Data authored for another axis
convention, such as a Z-up engine's cubemaps, is reordered or rotated by the application at upload,
or the shader swizzles the direction before sampling.

## Capability

The format rules are the 2D rules. BC cubes need the same capability as BC 2D textures
(`textureCompressionBC` on Vulkan, `supportsBCTextureCompression` on Metal) and answer
`Unsupported` without it; RGBA16Float on Metal needs the Metal 3 family and an edge of at most
16384. On Vulkan the image-format query is made with `VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT`, so the
extent limit it returns is the adapter's cube limit, and it must also admit six array layers;
otherwise the upload is `Unsupported`. On Metal an extent the device cannot allocate fails at
texture creation.

## Native ownership and synchronization

**Vulkan.** One optimal-tiling image with `VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT`, six array layers,
the supplied level count, sampled and transfer-destination usage, behind one
`VK_IMAGE_VIEW_TYPE_CUBE` view over every layer and level. The faces are packed layer-major into
one staging buffer (every level of +X, then of -X, and so on) and copied with one
`vkCmdCopyBufferToImage2` carrying a region per face and level, with `baseArrayLayer` set to the
face. The layout transitions, upload fence, descriptor writes (`SAMPLED_IMAGE` with the cube view)
and destruction ordering are the 2D paths, extended to six layers.

**Metal.** A `MTLTextureDescriptor` from `texture2DDescriptorWithPixelFormat:width:height:mipmapped:`
at `size`×`size` (which counts the full chain) with `textureType` set to `MTLTextureTypeCube`, in
the same storage and usage as the 2D uploads. Each face and level is written with
`replaceRegion:mipmapLevel:slice:withBytes:bytesPerRow:bytesPerImage:`, slice equal to the face
index, `bytesPerRow` the level's texel or block row and `bytesPerImage` the whole level. The texture
binds with `setVertexTexture:atIndex:`/`setFragmentTexture:atIndex:` as a 2D texture does.
**This path has not run on a Mac.** It was written beside the 2D path and passes `cargo clippy` for
`aarch64-apple-darwin`, but no Metal device has created, uploaded or sampled a cube texture yet.

## Validation boundary

Unit tests cover face count and extent checks, per-face byte sizing for RGBA8, RGBA16Float, BC1 and
BC3 including one-block tail levels, complete-chain checks per face with the offending face named,
the artifact's cube binding kind, binding declarations that disagree with the recorded kind in
either direction, the texture count limit across cube slots, and records that put a 2D texture in a
cube slot or the reverse.

`mulciber-cube-texture` is the native probe. It uploads four cubes through the public API and
samples them through `MaterialBinding::CubeTexture` into an HDR target, reading back the native
half bits of each case:

- RGBA8 UNORM, 2×2 faces, every texel encoding its face, column and row, sampled at all 24 texel
  centres: face order and in-face orientation against the table above;
- RGBA8 sRGB, 4×4 faces with three levels, and BC1 UNORM, 8×8 faces with four levels (the last
  three one block each), every face and level a different non-black colour, sampled at every face
  centre and level through a direction that is not normalized;
- RGBA16Float, 1×1 faces with signed values outside 0..1.

It also checks that a short face, mis-sized BC3 faces, a float update of a cube, a 2D declaration
of the cube binding and a 2D texture in the cube slot are refused.

On 2026-10-08, Linux / KDE Wayland / NVIDIA GeForce RTX 3060 Ti (driver 615.71.09), Vulkan with
`vulkan-validation` enabled (Khronos validation layer 1.4.363) and no validation messages, all 72
cases matched within two half ULPs and every refusal fired. Metal was not run, BC2, BC3, BC5, BC7
and the sRGB BC variants were not sampled as cubes (BC3 was exercised only by the sizing refusal),
and no viability gate is advanced. Validation-layer success and pixel readback do not establish
visual correctness of a real environment map; that waits for the consuming game.

```sh
cargo run -p mulciber-cube-texture                  # Linux/Windows; Vulkan validation enabled
MTL_DEBUG_LAYER=1 cargo run -p mulciber-cube-texture  # macOS, once the Metal artifact exists
```

The probe's sample shader is `probes/cube-texture/src/sample.wgsl`; its composite pass is the
float-texture probe's. Only the Vulkan artifact is checked in, with its hash in
`vulkan-toolchain.lock.toml`. The Metal artifact needs Xcode's tools, so a macOS build without it
gets an empty placeholder and a build warning, and the probe stops at startup with a message rather
than rendering. Generate both on a Mac with:

```sh
cargo run -p mulciber-shader -- build probes/cube-texture/src/sample.wgsl --target metal --output probes/cube-texture/artifacts/sample.metal.shaderbin
cargo run -p mulciber-shader -- build probes/cube-texture/src/sample.wgsl --target vulkan --output probes/cube-texture/artifacts/sample.vulkan.shaderbin
```

## Generated Vulkan bindings

`VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT` comes from adding `VkImageCreateFlagBits` to
`tools/vulkan-bindgen/symbols.txt` and regenerating `vk.rs` from the pinned Vulkan-Headers
(v1.4.356, `8d6039a`); regenerating without the new symbol first reproduced the checked-in file
exactly, so the diff is only the new enum's constants. `VK_IMAGE_VIEW_TYPE_CUBE` was already generated.
