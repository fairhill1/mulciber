# mulciber-text

Text and flat shapes over a [Mulciber](https://github.com/fairhill1/mulciber) scene: the mechanics a
game's HUD, menus and chat share, without their look.

The game brings its fonts, sizes, character sets, keycap or icon glyphs, colours and layout. This
crate rasterises them into one atlas, measures and wraps text, builds the overlay geometry, and can
draw it in software for tests. It holds no fonts of its own.

- **`AtlasBuilder`** rasterises each `Font` (TrueType or OpenType bytes, through
  [fontdue](https://crates.io/crates/fontdue)) at the pixel sizes the game asks for, with the
  characters it asks for. Everything goes into one RGBA8 sRGB `Atlas`:
  - glyphs, white with their coverage in alpha;
  - a solid block for flat shapes;
  - any coverage masks or colour images the game packs beside them.

  Rebuild it when the sizes change, typically when the window's height does.
- **`Atlas::measure`, `wrap` and `fit`** work without a device:
  - `measure` gives a string's width, with tracking and scale;
  - `wrap` breaks text into lines at spaces and line breaks;
  - `fit` cuts a string short with an ellipsis.
- **`Batch`** builds one frame's geometry, in pixels from the surface's top-left corner: text,
  atlas images, convex polygons, rectangles, lines and circles. Edges are softened across a pixel,
  since an overlay pass renders one sample a pixel. It becomes one material record's transient
  geometry.
- **`Batch::rasterise`** draws the batch as the overlay shader does, so a screen can be checked or
  previewed on a machine with no GPU.
- **Beyond those**, for what a game draws of its own:
  - `Font::rasterise` gives one glyph's coverage, to alter (cut, split, compose) before packing it
    with `AtlasBuilder::coverage`;
  - `Batch::mesh` adds triangles of the solid block, coloured at each corner: rings, arcs,
    gradients;
  - `Batch::text_turned` sets text turned about its middle, as a stamp;
  - `Batch::into_parts` takes the vertices and indices out, to keep them past the atlas's borrow.

## Shader and pipeline

`mulciber-shader`'s `mulciber::overlay` module declares the vertex (`OverlayVertex`, laid out as
`VERTEX_LAYOUT`) and the shading. A game's overlay shader binds its atlas and passes through:

```wgsl
#import mulciber::overlay::{OverlayVertex, OverlayRaster, overlay_raster, overlay_shade}

@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(0) @binding(1) var atlas_sampler: sampler;

@vertex
fn overlay_vertex(vertex: OverlayVertex) -> OverlayRaster {
    return overlay_raster(vertex);
}

@fragment
fn overlay_fragment(raster: OverlayRaster) -> @location(0) vec4<f32> {
    return overlay_shade(textureSample(atlas, atlas_sampler, raster.uv), raster.color);
}
```

The material pipeline:

```rust
let pipeline = device.create_material_pipeline(MaterialPipelineDescriptor {
    shader: ShaderArtifact::new(OVERLAY_SHADER)?,
    vertex_entry: "overlay_vertex",
    fragment_entry: "overlay_fragment",
    vertex_layout: mulciber_text::VERTEX_LAYOUT,
    instance_layout: None,
    bindings: &[
        MaterialBinding::Texture { binding: 0 },
        // Text at its baked size lands a texel on a pixel; nearest reads it exactly.
        MaterialBinding::Sampler {
            binding: 1,
            filter: SamplerFilter::Nearest,
            address: SamplerAddress::ClampToEdge,
        },
    ],
    blend: BlendMode::PremultipliedTranslucent,
    depth: DepthMode::Off,
    scene_color: SceneColor::Surface,
})?;
```

## A frame

```rust
// When the window's height changes: the game's faces at sizes that follow it.
let font = Font::from_bytes(include_bytes!("../assets/fonts/MyFace.ttf"))?;
let mut builder = AtlasBuilder::new(1024)?;
let text = builder.face(&font, (height as f32 / 36.0).round(), ' '..='~')?;
let title = builder.face(&font, (height as f32 / 18.0).round(), ' '..='~')?;
let atlas = builder.build();
let atlas_texture = atlas.upload(&device)?;

// Each frame.
let mut batch = Batch::new(&atlas, width, height);
batch.rect([20.0, 20.0, 420.0, 140.0], [0.0, 0.0, 0.0, 0.6]);
let mut baseline = 20.0 + atlas.metrics(title).ascent;
batch.text(title, "Inventory", 36.0, baseline, [1.0, 0.9, 0.7, 1.0]);
for line in atlas.wrap(text, description, 368.0) {
    baseline += atlas.metrics(text).line_height;
    batch.text(text, &line, 36.0, baseline, [1.0; 4]);
}
let overlay = batch.geometry().map(|geometry| MaterialRecord {
    pipeline: &pipeline,
    geometry,
    textures: &[&atlas_texture],
    shadow_map: None,
    uniform: &[],
    storage: &[],
    instances: &[],
});
```

The record goes in a `SceneOutput::Postprocessed` submission's `overlay`, drawn at the surface's
native extent after the postprocess pass.

## Licence

MIT OR Apache-2.0. The tests' font, a subset of Fira Sans Medium, is under the SIL Open Font
License (`tests/fonts/FiraSans-OFL.txt`) and is not part of the published crate.
