# Packed vertex formats (unreleased)

`VertexFormat` gains six formats that are narrower in the vertex buffer than the WGSL type the
shader reads them as. The GPU widens them during vertex fetch, so a skinned vertex can carry its
bone indices and weights in eight bytes instead of thirty-two:

| `VertexFormat` | Bytes | WGSL input type | Vulkan | Metal |
|---|---|---|---|---|
| `Uint8x4` | 4 | `vec4<u32>` | `VK_FORMAT_R8G8B8A8_UINT` | `MTLVertexFormatUChar4` |
| `Unorm8x4` | 4 | `vec4<f32>` in 0..1 | `VK_FORMAT_R8G8B8A8_UNORM` | `MTLVertexFormatUChar4Normalized` |
| `Uint16x2` | 4 | `vec2<u32>` | `VK_FORMAT_R16G16_UINT` | `MTLVertexFormatUShort2` |
| `Uint16x4` | 8 | `vec4<u32>` | `VK_FORMAT_R16G16B16A16_UINT` | `MTLVertexFormatUShort4` |
| `Unorm16x2` | 4 | `vec2<f32>` in 0..1 | `VK_FORMAT_R16G16_UNORM` | `MTLVertexFormatUShort2Normalized` |
| `Unorm16x4` | 8 | `vec4<f32>` in 0..1 | `VK_FORMAT_R16G16B16A16_UNORM` | `MTLVertexFormatUShort4Normalized` |

They work in vertex and instance layouts of material and shadow pipelines alike. Pipeline creation
compares the WGSL type a format is read as with the type `mulciber-shader` recorded for the input,
so `Uint8x4` feeds `@location(n) bones: vec4<u32>` and `Unorm8x4` feeds `vec4<f32>`; a mismatch
names both (`declared layouts supply location 1 as Unorm8x4 (read as vec4<f32>) but the shader
artifact records vec4<u32>`). The artifact format is unchanged, because the shader still records
only WGSL types.

A three-bone skinned vertex, as Source models carry, packs into 24 bytes:

```rust
use mulciber::{VertexAttribute, VertexFormat, VertexLayout};

const SKINNED: VertexLayout<'static> = VertexLayout {
    stride: 24,
    attributes: &[
        VertexAttribute { location: 0, offset: 0, format: VertexFormat::Float32x3 },
        VertexAttribute { location: 1, offset: 12, format: VertexFormat::Uint8x4 }, // bone indices
        VertexAttribute { location: 2, offset: 16, format: VertexFormat::Unorm8x4 }, // weights
        VertexAttribute { location: 3, offset: 20, format: VertexFormat::Unorm16x2 }, // uv in 0..1
    ],
};
```

```wgsl
@group(0) @binding(1) var<storage, read> bones: array<mat4x4<f32>, 128>;
@vertex fn skinned_vertex(
    @location(0) position: vec3<f32>,
    @location(1) indices: vec4<u32>,
    @location(2) weights: vec4<f32>,
    @location(3) uv: vec2<f32>,
) -> Raster {
    let skin = bones[indices.x] * weights.x + bones[indices.y] * weights.y
        + bones[indices.z] * weights.z;
    ...
}
```

Unused fourth index and weight bytes are the application's to fill (zero weight is the usual
choice). Normalized weights are `byte / 255`, so three weights that should sum to one may sum to
one within 1/255; renormalize in the shader if that matters.

## Alignment rule

Every vertex layout's stride and every attribute offset must now be a multiple of four bytes,
for the 32-bit formats as well as the packed ones. Metal requires both; holding Vulkan to the same
rule keeps one layout valid on both backends. Layouts built from the 32-bit formats were already
four-byte aligned in practice, and an unaligned one now fails at mesh or pipeline creation with
`InvalidRequest` naming the location instead of failing natively on Metal.

## Capability

Vulkan requires vertex-buffer support for the 32-bit formats and `R8G8B8A8_UINT`/`UNORM`; the
16-bit ones are queried. Material and shadow pipeline creation reads each declared attribute's
`bufferFeatures` and returns `Unsupported`, naming the location, where
`VK_FORMAT_FEATURE_VERTEX_BUFFER_BIT` is absent. Every Metal device supports all six formats.

## Validation boundary

Unit tests cover each format's byte size and WGSL type, the diagnostic spelling, the four-byte
stride and offset rule, packed attributes fitting inside the stride, and a packed attribute
satisfying only its wide WGSL type.

`mulciber-vertex-formats` uploads one mesh carrying all six formats at known values (bone indices
3, 17, 200, 255; weights 0, 51, 204, 255; 16-bit integers up to 4096; 16-bit normalized values
0, 16384, 32768, 49152, 65535), forwards each attribute from the vertex stage through a flat
varying into an HDR target and reads back the native half bits, and checks that a mismatched
declaration and an unaligned offset are refused. On 2026-10-08, Linux / KDE Wayland / NVIDIA
GeForce RTX 3060 Ti (driver 615.71.09), Vulkan with `vulkan-validation` and no validation messages,
all six formats matched within two half ULPs and both refusals fired. Metal maps the formats but
has not run; the probe's Metal shader artifact is not generated yet, so a macOS build gets a
placeholder and the probe stops at startup with a message. No viability gate is advanced.

```sh
cargo run -p mulciber-vertex-formats                  # Linux/Windows; Vulkan validation enabled
cargo run -p mulciber-shader -- build probes/vertex-formats/src/fetch.wgsl --target metal --output probes/vertex-formats/artifacts/fetch.metal.shaderbin  # on a Mac
```
