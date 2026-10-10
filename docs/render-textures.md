# Render textures

A render texture is a color texture a scene submission renders into before its scene pass, then
samples like any uploaded texture, in that frame and in later ones. The motivating use is a
portrait: a character's head rendered once by the world's own material pipelines through a camera
of its own, then shown on the HUD for as long as it stays the same.

## API

```rust
impl Device<'_> {
    pub fn create_hdr_render_texture(&self, width: u32, height: u32)
        -> Result<RenderTexture, GraphicsError>;
    pub fn destroy_render_texture(&self, target: RenderTexture) -> Result<(), GraphicsError>;
}

impl RenderTexture {
    pub const fn texture(&self) -> &Texture; // for material records' texture slots
    pub const fn width(&self) -> u32;
    pub const fn height(&self) -> u32;
}

pub struct OffscreenPass<'resources> {
    pub target: &'resources RenderTexture,
    pub records: &'resources [MaterialRecord<'resources>],
    pub clear: ClearColor,
}

pub struct SceneSubmission<'resources> {
    // ...
    pub offscreen: &'resources [OffscreenPass<'resources>], // empty for none
}
```

```rust
let portrait = device.create_hdr_render_texture(128, 128)?;
// Once, the frame the portrait is needed:
let passes = [OffscreenPass { target: &portrait, records: &head, clear: backdrop }];
queue.render_and_present(frame, SceneSubmission { offscreen: &passes, /* ... */ })?;
// That frame and every later one, until the head changes:
let hud = MaterialRecord { textures: &[portrait.texture()], /* ... */ };
```

## Contract

- **Format.** The color is linear `RGBA16Float`, one mip level, 1 to
  `RENDER_TEXTURE_SIZE_LIMIT` (8192) texels along each axis. Only HDR material pipelines draw into
  it; a surface-format pipeline is refused as an HDR mismatch. Tonemapping, if the sampled result
  needs it, belongs to the shader that samples it: nothing between the pass and the sampler
  changes the values.
- **Depth and multisampling.** Each render texture owns a D32 depth target and, at a multisample
  count, multisample color resolved into the sampled texture at the end of every pass. Both are
  built for the session's sample count when the texture is created, like the scene targets; after
  `Device::set_sample_count` changes the count, submitting a pass into an older texture is refused
  by name. Neither is kept between passes: each pass clears color to its `clear` and depth to the
  far value its records' depth modes select (1.0, or 0.0 for greater-compare records; mixing the
  two in one pass is refused).
- **Order.** Offscreen passes run in slice order after any shadow prepass and before the scene
  pass. Their records may sample this frame's shadow map, render textures rendered by earlier
  passes of the same submission, and render textures rendered in any earlier submission. Scene,
  foreground and overlay records may sample any render texture rendered by this submission or an
  earlier one.
- **Refusals.** Before the frame is consumed, a submission is refused when a record samples a
  render texture nothing has rendered yet, when an offscreen record samples its own pass's target
  or scene depth, when two passes target the same texture, when a pass has no records, or when
  passes accompany anything but material content with postprocessed output. Render textures are
  never updated from the CPU: `update_rgba16_float_texture*` refuses them.
- **Lifetime.** A render texture holds its last render until the next pass into it, so sampling it
  every frame costs only the sample. Rendering it again while earlier frames in flight still
  sample it is ordered after those reads. Destroying it, or dropping it, frees its color, depth and
  multisample storage once the last submitted frame that used it completes.

## Native implementation

Metal encodes each pass in a render encoder of its own on the frame's command buffer, between the
shadow encoders and the scene encoder, with the multisample color and depth memoryless and a
multisample-resolve store into the private `RGBA16Float` texture. Hazard tracking orders a pass
after earlier command buffers' reads of the same texture.

Vulkan records each pass as a dynamic-rendering scope after the shadow region: the color, depth and
multisample images move from `UNDEFINED` to attachment layouts behind a barrier whose first scope
covers earlier vertex- and fragment-stage sampling and color writes, multisample color resolves
with `VK_RESOLVE_MODE_AVERAGE_BIT`, and the color moves to `SHADER_READ_ONLY_OPTIMAL` for the
vertex and fragment stages of everything recorded after it. The records stage after the overlay's
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
earlier frames still sampled the first, and that pattern again a frame later.

```sh
cargo run -p mulciber-render-texture                        # Linux/Windows; Vulkan validation
cargo run -p mulciber-render-texture -- --force-one-sample
MTL_DEBUG_LAYER=1 cargo run -p mulciber-render-texture      # macOS
```

**Metal native evidence:** on an Apple M2 (macOS 15.8) the probe passed with Metal API Validation
at four samples and with `--force-one-sample`: every refusal matched, and all four captures matched
75,684 pixels each. **Vulkan unexercised:** the Vulkan path compiles and passes clippy for the
Windows target but has not run on a device or under validation.
