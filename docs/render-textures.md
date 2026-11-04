# Render textures

A render texture is a color texture a scene submission renders into before its scene pass, then
samples like any uploaded texture, in that frame and in later ones. The motivating use is a
portrait: a character's head rendered once by the world's own material pipelines through a camera
of its own, then shown on the HUD for as long as it stays the same. A single-sample render texture
whose passes keep its contents serves caches updated a piece at a time, such as a clipmap ring
that redraws only the strip the camera moved onto.

## API

```rust
impl Device<'_> {
    pub fn create_render_texture(&self, descriptor: &RenderTextureDescriptor)
        -> Result<RenderTexture, GraphicsError>;
    // `create_render_texture` with `samples: RenderTextureSamples::Session`.
    pub fn create_hdr_render_texture(&self, width: u32, height: u32)
        -> Result<RenderTexture, GraphicsError>;
    pub fn destroy_render_texture(&self, target: RenderTexture) -> Result<(), GraphicsError>;
}

pub struct RenderTextureDescriptor {
    pub width: u32,
    pub height: u32,
    pub samples: RenderTextureSamples,
}

impl RenderTextureDescriptor {
    pub fn validate(&self) -> Result<(), GraphicsError>; // the extent, without a device
}

pub enum RenderTextureSamples {
    Session, // the session's sample count; drawn into by SceneColor::Hdr pipelines
    Single,  // one sample per pixel; drawn into by SceneColor::HdrSingleSample pipelines
}

impl RenderTexture {
    pub const fn texture(&self) -> &Texture; // for material records' texture slots
    pub const fn width(&self) -> u32;
    pub const fn height(&self) -> u32;
    pub const fn samples(&self) -> RenderTextureSamples;
}

pub struct OffscreenPass<'resources> {
    pub target: &'resources RenderTexture,
    pub records: &'resources [MaterialRecord<'resources>],
    pub load: OffscreenLoad,
}

pub enum OffscreenLoad {
    Clear(ClearColor),
    Keep, // single-sample targets an earlier submission rendered only
}

pub enum SceneColor {
    Surface,
    Hdr,
    HdrSingleSample, // material pipelines for single-sample render textures only
}

pub enum SceneOutput<'resources> {
    // ...
    Postprocessed {
        // ...
        offscreen: &'resources [OffscreenPass<'resources>], // empty for none
    },
}
```

```rust
let portrait = device.create_hdr_render_texture(128, 128)?;
// Once, the frame the portrait is needed:
let passes = [OffscreenPass {
    target: &portrait,
    records: &head,
    load: OffscreenLoad::Clear(backdrop),
}];
let output = SceneOutput::Postprocessed {
    postprocess,
    foreground_start: None,
    offscreen: &passes,
    overlay: &[],
};
let scene = Scene { records: &world, shadow: None, output, timing_marks: &[] };
queue.render_and_present(frame, SceneSubmission { scene, clear })?;
// That frame and every later one, until the head changes:
let hud = MaterialRecord { textures: &[portrait.texture()], /* ... */ };
```

```rust
// A cache redrawn a piece at a time, by pipelines built with SceneColor::HdrSingleSample.
let ring = device.create_render_texture(&RenderTextureDescriptor {
    width: 512,
    height: 512,
    samples: RenderTextureSamples::Single,
})?;
// The first submission into it clears and fills it whole:
let fill = [OffscreenPass { target: &ring, records: &whole, load: OffscreenLoad::Clear(black) }];
// Later ones keep it and redraw only the texels that changed:
let update = [OffscreenPass { target: &ring, records: &strip, load: OffscreenLoad::Keep }];
```

## Contract

- **Format.** The color is linear `RGBA16Float`, one mip level, 1 to
  `RENDER_TEXTURE_SIZE_LIMIT` (8192) texels along each axis. Only HDR material pipelines draw into
  it; a surface-format pipeline is refused as an HDR mismatch. Tonemapping, if the sampled result
  needs it, belongs to the shader that samples it: nothing between the pass and the sampler
  changes the values.
- **Samples.** A `RenderTextureSamples::Session` texture owns a D32 depth target and, at a
  multisample count, multisample color resolved into the sampled texture at the end of every pass.
  Both are built for the session's sample count when the texture is created, like the scene
  targets, and `SceneColor::Hdr` pipelines draw into it; after `Device::set_sample_count` changes
  the count, submitting a pass into an older texture is refused by name. A
  `RenderTextureSamples::Single` texture owns a one-sample D32 depth target and no multisample
  color: its passes render straight into the sampled texture at one sample per pixel whatever the
  session's count, so changing the count leaves it usable, and only `SceneColor::HdrSingleSample`
  pipelines draw into it. Every offscreen record's pipeline must match its pass's target that way;
  `SceneColor::HdrSingleSample` pipelines are refused in scene, foreground and overlay records,
  may not declare scene depth, and cannot build postprocess targets.
- **Loading.** `OffscreenLoad::Clear(color)` starts the pass from that color, discarding what the
  texture held. `OffscreenLoad::Keep` starts it from the texture's contents as the last pass into
  it left them, so its records draw over them and texels they do not cover keep their value; it is
  refused for a session-sampled texture, whose multisample storage holds nothing between passes,
  and for a texture no earlier submission has rendered. Depth is never kept: every pass clears it
  to the far value its records' depth modes select (1.0, or 0.0 for greater-compare records; mixing
  the two in one pass is refused).
- **Order.** Offscreen passes run in slice order after any shadow prepass and before the scene
  pass. Their records may sample this frame's shadow map, render textures rendered by earlier
  passes of the same submission, and render textures rendered in any earlier submission. Scene,
  foreground and overlay records may sample any render texture rendered by this submission or an
  earlier one.
- **Refusals.** Before the frame is consumed, a submission is refused when a record samples a
  render texture nothing has rendered yet, when an offscreen record samples its own pass's target
  or scene depth, when two passes target the same texture, when a pass has no records, when a
  record's pipeline does not match its target's samples, or when a pass keeps a session-sampled
  or never-rendered target. Passes
  exist only in `SceneOutput::Postprocessed`, so passes with direct output are unrepresentable. Render textures are never updated from the CPU: `update_texture` refuses them.
- **Lifetime.** A render texture holds its last render until the next pass into it, so sampling it
  every frame costs only the sample. Rendering it again, cleared or kept, while earlier frames in
  flight still sample it is ordered after those reads. Destroying it, or dropping it, frees its color, depth and
  multisample storage once the last submitted frame that used it completes.

## Native implementation

Metal encodes each pass in a render encoder of its own on the frame's command buffer, between the
shadow encoders and the scene encoder, with the multisample color and depth memoryless and a
multisample-resolve store into the private `RGBA16Float` texture. A single-sample pass renders
straight into the texture with a store action of `MTLStoreActionStore`; a kept one loads it with
`MTLLoadActionLoad` instead of clearing. Hazard tracking orders a pass after earlier command
buffers' reads and writes of the same texture.

Vulkan records each pass as a dynamic-rendering scope after the shadow region: the depth and
multisample images, and the color of a cleared pass, move from `UNDEFINED` to attachment layouts
behind a barrier whose first scope covers earlier vertex- and fragment-stage sampling and color
writes; a kept pass's color moves instead from the `SHADER_READ_ONLY_OPTIMAL` the pass that last
rendered it left, with color-attachment reads and writes in the second scope, and loads with
`VK_ATTACHMENT_LOAD_OP_LOAD`. Multisample color resolves with `VK_RESOLVE_MODE_AVERAGE_BIT`, a
single-sample pass stores the color itself, and the color moves to `SHADER_READ_ONLY_OPTIMAL` for
the vertex and fragment stages of everything recorded after it. Pipelines and render textures built
for one sample skip the session sample-count check. The records stage after the overlay's
in the frame's uniform, storage, transient-geometry and instance regions, before the shadow
records.

## Evidence

`mulciber-render-texture` is the native probe. It reuses three checked-in artifacts: the material
scene's HUD shader (vertex colors) drawing a four-quadrant pattern into a 48x40 render texture,
followed by a full-cover white record at the same depth that the depth test must reject; the
float-texture probe's sampler reading one texel of the texture per screen quadrant; and that
probe's HDR composite. It checks the refusals above by their wording, then captures four frames
and compares every pixel away from the quadrant edges: the pattern sampled in the frame that
rendered it, the same pattern two frames later without a pass, a second pattern rendered while
earlier frames still sampled the first, and that pattern again a frame later. It then checks the
single-sample refusals (keeping a never-rendered texture, a session-sampled pipeline into a
single-sample texture, a single-sample pipeline in the scene, keeping a session-sampled texture)
and captures two more: the first pattern rendered into a 48x40 single-sample texture by the HUD
shader built for `SceneColor::HdrSingleSample`, and, two frames later, a kept pass that redraws
only its top-left quadrant, the other three surviving.

```sh
cargo run -p mulciber-render-texture                        # Linux/Windows; Vulkan validation
cargo run -p mulciber-render-texture -- --force-one-sample
MTL_DEBUG_LAYER=1 cargo run -p mulciber-render-texture      # macOS
```

**Metal native evidence:** on an Apple M2 (macOS 15.8) the probe passed with Metal API Validation
at four samples and with `--force-one-sample`: every refusal matched, and all four captures matched
75,684 pixels each, before single-sample textures and kept passes existed. **Single-sample and
kept passes unexercised:** their Vulkan path compiles and passes clippy for Linux, and their Metal
path is type- and lint-checked for aarch64-apple-darwin only; neither has run the extended probe
on a device yet. **Vulkan unexercised:** the Vulkan path compiles and passes clippy but the probe
has not run on a device or under validation.
