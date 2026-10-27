# Changelog

Release notes moved from the README. This is a partial history of changes.

## Ropes (model 0.7.0)

Breaking: `Hang` gains `follow`, so one written out field by field without `..Hang::new(gravity)`
needs it.

`Hanging::step_among` keeps the chains out of capsules in the world as well as those the skeleton
carries, given each frame where they are (`Around`): someone walking into a rope hung from a
ceiling. `Hang::follow` is the follow-the-leader correction to the points' speed (Müller 2012): a
long chain lightly damped, a rope's, otherwise swings on by itself, its links' lengths kept by
pulling each point after the one above. 0, the default, leaves it as before. New tests: a capsule in
the world pushing a chain aside and it swinging back, and a long lightly damped chain settling with
follow and not without.

## Kicks with a reason (net 0.3.0)

Breaking: `DisconnectReason` gains `Kicked(u8)`, so a `match` on it without a wildcard arm needs it.

`Server::kick` drops a client as `Server::disconnect` does, but its goodbye carries a code of the
game's own, and the client ends `ClientState::Disconnected(DisconnectReason::Kicked(code))`, so it
can say why it was put off: for what it did, or refused as soon as it connected, from what it sent
(`ServerEvent::Connected`'s payload). The `Disconnect` packet carries the code after the session; a
goodbye without one still reads as `ClosedByPeer`. As with `disconnect`, what was queued for the
client and not yet flushed is not sent.

## Imported structs' names in buffer layouts (shader 0.5.8)

A uniform or storage buffer whose struct an imported module declares records the struct's own name
in its layout (`BufferLayout::type_name`, and a member's `BufferMember::type_name`), as the docs
say, not the name `naga_oil` decorates it with when composing (`WorldX_naga_oil_mod_X…X` for
`World`).

## Morph targets (model 0.4.0)

Breaking: `Model` gains `morphs` and `Part` gains `morphs`, so either written out field by field
without `..Default::default()` needs them.

`mulciber-model` reads morph targets, by name: `Model::morphs` lists every target its meshes have
once, as their `extras.targetNames` name them (Blender writes these; one without a name is named by
its index in its mesh), and `Model::morph` finds one. Meshes naming the same target share it, so one
weight moves a face and the teeth fitted to it. Each part keeps a `Morph` for every target that moves
any of its vertices, of only those vertices: their offsets at full weight and their normals' changes.
The model's transforms (the normals' changes turned as the normals are), its double-sided copies
(their normals' changes reversed) and the vertices split for `MikkTSpace` tangents carry them; a flat
shaded primitive's targets turn each corner's normal to the moved triangle's. `Part::morphed` blends
them at a set of weights on the CPU. Default weights are not applied, tangents are not moved, and
animations' morph weight channels are still left out. `gltf`'s `extras` feature is now enabled.

## Raw or system pointer motion (platform 0.5.9)

`Window::set_pointer_motion` chooses where a captured pointer's `PointerDelta` motion comes from:
`PointerMotion::Raw`, the device's own motion before the system's acceleration (the default, as
in 0.5.8), or `PointerMotion::System`, the system pointer's motion, accelerated as the user's
pointer settings say, for a touchpad. It takes effect at once, captured or not;
`Window::pointer_motion` reads it back. Each backend already had both: Win32 reports the hidden
cursor's way off the client center from `WM_MOUSEMOVE` instead of `WM_INPUT`, Wayland the
relative pointer's accelerated pair instead of its unaccelerated one, X11 the warp-to-center
deltas instead of `XInput2` raw motion, and AppKit the `NSEvent` deltas instead of `GCMouse`'s.

## Unaccelerated X11 and AppKit pointer deltas (platform 0.5.8)

While the pointer is captured, every backend's `PointerDelta` now carries the mouse's own motion,
before the system's pointer acceleration, so turning speed no longer depends on how fast the mouse
moves anywhere (Win32 and Wayland already did).

- X11 selects `XInput2` raw motion (`XI_RawMotion`, `XInput2` 2.1 or later) from every master
  pointer on the root window and reports its raw values for the first two axes. The pointer stays
  grabbed and warped to the window's center, its core motion now only re-centering it. An absolute
  device's motion (a tablet, a virtual machine's pointer), which the server doesn't accelerate, is
  still reported from the warp. A server without `XInput2` 2.1 keeps the warp deltas. `libXi`
  joins the Linux link-time dependencies.
- AppKit gives every connected `GCMouse` (the GameController framework, macOS 11) a moved handler
  while captured and reports the motion it gathered once per pump, y flipped to point down. With
  no `GCMouse` (before macOS 11, or before the framework finds a mouse), the `NSEvent` deltas are
  reported as before. `GameController` joins the macOS frameworks linked.

## Muffled voices (audio 0.4.0)

Breaking: `Play` gains `muffle`, so a `Play` written out field by field without
`..Play::default()` needs it.

A voice with something between it and the listener, a wall, a shut door, a deck, can be muffled:
`Muffle { gain_db, cutoff_hz }` turns it down and dulls it through a low-pass of two poles at the
cutoff, 12 dB an octave above it (`Muffle::CLEAR`, the default, lets it through untouched). A clip
takes one as it starts (`Play::muffle`); a playing voice, a clip's or a live stream's, is changed by
`Controls::set_voice_muffle`, which glides there over about 50 ms rather than stepping. The game
decides how muffled each voice is: the mixer knows nothing of walls. Muffled voices still feed the
listener's room reverb, as sound coming through a wall rings in the room it comes into.

## Shared by both games: build scripts, sound takes, skin (shader 0.5.7, audio 0.3.1, model 0.3.1)

Three things Deadly Voyage and Isle of Rán each wrote for themselves.

- `ShaderTarget::for_build()` picks the shader target from a `build.rs`'s `CARGO_CFG_TARGET_OS`
  (`for_os` from a name), and `ShaderModules::compile_dir(dir, out, target)` compiles every
  top-level shader directly in a directory into `out/<name>.shaderbin`, printing
  `cargo::rerun-if-changed` for it and each shader.
- `mulciber-audio`'s `Takes` holds several recordings of one sound and picks one at random, never
  the one picked last, `pick_at_rate` with a playback rate from a range; `other_take` is the choice
  alone, for a game keeping its own random source.
- Pre-integrated skin (Penner, SIGGRAPH 2011): `bake_skin_table(SKIN_SPREAD)` bakes the 128 × 64
  table of how much light skin takes by n·l and curvature, the `mulciber::skin` WGSL module reads
  it (`skin_diffuse`), and `mulciber-model`'s `vertex_curvature` gives each vertex its curvature.
  The table and curvature are Deadly Voyage's, itself a copy of Isle of Rán's.

## A refresh the driver learns later (graphics 0.18.0)

Vulkan read the swapchain's refresh period once, when the swapchain was made. NVIDIA's Linux driver
on Wayland reports zero then and learns the period from the presents that follow, so
`Surface::refresh_interval` stayed `None` and a runtime fed from it never paced. The period is now
read again whenever a past-timing report's `timingPropertiesCounter` differs from the one it was
read at, and on every drain while it is still unknown. On that machine (KDE Wayland, 75 Hz)
`mulciber-game-slice` now prints `Vulkan refresh interval: 13.338490 ms (refresh mode
undetermined)` within its first seconds, and printed none before.

## Fixed intervals (runtime 0.6.1)

`RuntimeConfig::fixed_interval(Duration)` (additive) sets the fixed step directly, for a rate that is
not a whole number per second: Source's 15 ms tick, 66.67 Hz, which `fixed_hz(u32)` could not give.
`fixed_hz` is now built on it. A zero interval is refused as `UpdateRateTooHigh`, whose meaning
widens to cover it, so the error enum gains no variant.

## BC6H light (graphics 0.18.0, texture 0.6.0)

Breaking: `BlockCompression` gains `Bc6hUfloat`, so an exhaustive match on it needs an arm. BC6H holds
three channels of unsigned half float, sampled linear and unclamped from 0 to 65504 with alpha 1, in
one byte a texel: an eighth of `RGBA16Float`. It is for light that is baked and does not change, a
sky, a lightmap, a reflection probe.

- `TexelData::BlockCompressed(BlockCompression::Bc6hUfloat, …)` uploads 2D textures and cubes like
  the other BC formats (`VK_FORMAT_BC6H_UFLOAT_BLOCK`, `MTLPixelFormatBC6H_RGBUfloat`), behind the
  same BC capability.
- Cube texture arrays take BC6H as well as `RGBA16Float`, with a supplied chain.
- KTX 2.0 reads and `ktx2_vk_format` writes it as VkFormat 143.
- `mulciber-texture` 0.6.0: `encode_bc6h` encodes one level with Intel's ISPC encoder, padding odd
  extents to whole blocks, clamping to 0..65504 (`HDR_MAX`) and refusing non-finite light by texel;
  `hdr_chain` builds a mip chain of light by 2×2 means, since a GPU cannot filter BC6H mips.

Only the unsigned variant is supported, the one the encoder writes. Encoding is tested against an
independent decoder (`bcdec_rs`). `mulciber-cube-array-texture` sampled 37 BC6H cases, and passed
whole on Vulkan for the first time, on Mesa's software lavapipe under validation
([evidence](docs/linux-validation.md#cube-texture-arrays-graphics-0160-0180)); no GPU and no Metal
run yet. See [block-compressed textures](docs/block-compressed-textures.md).

## Text and overlays (text 0.1.0, shader 0.5.7)

`mulciber-text` is new, the mechanics both games on Mulciber had each built for their HUDs, menus and
chat out of the same parts (fontdue, a shelf-packed atlas, a pos/uv/colour vertex, a premultiplied
shader). The look stays the game's: it brings its fonts, sizes, character sets, keycap and icon
glyphs, colours and layout, and the crate ships no fonts.

- `AtlasBuilder` rasterises each `Font` at the pixel sizes and characters asked for into one RGBA8
  sRGB `Atlas`, glyphs white with their coverage in alpha, with a solid block for flat shapes and any
  coverage masks or colour images the game packs beside them; `Atlas::upload` makes the texture.
- `Atlas::measure`, `wrap` (at spaces and line breaks, a long word overrunning on its own line) and
  `fit` (cut with an ellipsis) need no device; `Style` adds tracking and scale.
- `Batch` builds a frame's overlay geometry in pixels: text on whole pixels at its baked size, atlas
  images, convex polygons, rectangles, lines and circles with edges softened across a pixel, every
  triangle facing the screen. `Batch::geometry` is a material record's transient geometry, laid out
  as `VERTEX_LAYOUT`.
- `Batch::rasterise` draws it in software as the shader does (bilinear, premultiplied, the top-left
  fill rule), for tests and previews without a GPU.
- For what a game draws beyond those: `Font::rasterise` gives a glyph's coverage alone, to alter
  before packing it as a mask (Deadly Voyage cuts its mouse-wheel keycaps' chevrons out to draw them
  in an accent); `Batch::mesh` adds triangles of the solid block coloured at each corner (rings,
  arcs, gradients); `Batch::text_turned` sets text turned about its middle; `Batch::into_parts`
  takes the geometry out, to keep past the atlas's borrow.

`mulciber-shader` 0.5.7 adds the `mulciber::overlay` WGSL module the overlay pipeline imports. The
README's overlay shader is compiled in a test and its pipeline declaration (`VERTEX_LAYOUT`, the atlas
and sampler bindings) passes `MaterialPipelineDescriptor::validate` against it, the check pipeline
creation makes.

Tested in software only: the atlas, layout and geometry by unit tests against a subset of Fira Sans,
and the rasteriser drawing a glyph texel for pixel, rectangles on whole pixels covering each pixel
once, and polygons either way round. Deadly Voyage's HUD is built on it now, and its screens, drawn in
software by its tests, match what its own atlas and batch drew to within two levels in 255, and it
draws through `mulciber::overlay` on Vulkan (Linux); Metal hasn't drawn it yet.

## Typed text and key tokens (platform 0.5.7, runtime 0.6.1)

`InputEvent::Character(char)` delivers what the keyboard types, for chat, consoles and names: the
native layout's character after Shift, Caps Lock and AltGr (Option on macOS), so the AZERTY Q key
types `a`, following the `Keyboard` press that typed it and repeating with it. Both games built on
Mulciber had worked it out from `KeyCode`s with an American layout table. Only printable characters
arrive, and a key held with Control, or Command on macOS, types nothing. Win32 reads `WM_CHAR`, joining
surrogate pairs; AppKit the key-down's `characters`; X11 `XLookupString` with xkbcommon's keysym
conversion; Wayland an `xkb_state` kept from the compositor's keymap. X11 and Wayland compose dead
keys and the Compose key through xkbcommon's Compose tables, and Win32 through `WM_CHAR`; AppKit
bypasses the text input system, so dead keys are not expected to compose there. IME composition is
not delivered yet (on Windows an input method's committed text may still arrive through `WM_CHAR`,
unchecked).

`InputSnapshot::typed_text` (runtime 0.6.1) collects the characters typed for a frame, latched and
cleared like key presses, for games that read input through the runtime.

`KeyCode::name` and `KeyCode::from_name` give every key a stable token, its variant's name, for
saving bindings in settings; both games kept their own, incompatible tables.

X11 and Wayland passed layout, AltGr, dead-key, Compose, repeat and Control checks under Xvfb and
headless sway ([evidence](docs/linux-validation.md#typed-text-platform-057)). Windows and macOS are
compile- and Clippy-checked only. A unit test composes dead keys and the Compose key through a
Compose table of its own, independent of the host's. See
[text entry](docs/input-contract.md#text-entry).
## The material path is the only path (graphics 0.18.0, texture 0.6.0)

Breaking. The fixed textured and instanced pipelines are removed with everything only they used:
`create_textured_pipeline`, `create_instanced_textured_pipeline` and their `destroy_*`,
`TexturedPipeline`, `InstancedTexturedPipeline`, `TexturedSceneDraw`, `TexturedInstanceBatch`,
`Scene::Textured`, `Scene::Instanced`, `Vertex`, `VertexLayout::VERTEX` and `MeshVertices`. A
textured or instanced draw is a material record whose pipeline declares that shader, layout and
bindings; `mulciber-cube` and `mulciber-game-slice` show both.

With one kind of scene left, `Scene` is what `MaterialScene` was, and `SceneOutput` what
`MaterialOutput` was; a mesh's layout sits beside its bytes:

```rust
// 0.17
let scene = Scene::Material(MaterialScene { records: &records, shadow: None, output: MaterialOutput::Direct(&targets) });
let mesh = device.create_mesh(&MeshDescriptor { vertices: MeshVertices::Bytes { layout, bytes }, parts })?;
// 0.18
let scene = Scene { records: &records, shadow: None, output: SceneOutput::Direct(&targets) };
let mesh = device.create_mesh(&MeshDescriptor { layout, vertices: bytes, parts })?;
```

Vulkan meshes no longer store a `VkDrawIndexedIndirectCommand` per part, nor take indirect usage
and the barrier for it: only the removed textured scene drew through them, material and shadow
records having drawn with `vkCmdDrawIndexed` since 0.13.12. Metal keeps its per-part indirect
arguments, which its material records draw through. The frame transform buffers both backends kept
for the fixed pipelines are gone too.

`mulciber-texture` 0.6.0 moves to `mulciber` 0.18.0, with no change of its own.

The examples compile their WGSL in `build.rs` with `mulciber-shader` instead of embedding checked-in
artifacts (`MULCIBER_METAL_TYPECHECK_ONLY=1` writes empty ones, to type-check a macOS build on
another host). `mulciber-cube` and `mulciber-input-cube` draw through a material pipeline, and
`mulciber-game-slice` through one instanced material pipeline, a fullscreen module its grade
`#import`s, and a grade with a vignette. `mulciber-scene`, `mulciber-instanced-scene`,
`mulciber-postprocess-cube` and `mulciber-showcase-cube` are deleted: each drew through the removed
API. Their shaders and artifacts move to
the probes that still use them (the cube's to `mulciber-api-cube`, the postprocess and instanced
ones to `mulciber-api-conformance`), and the `comparisons/` wgpu peers include them from there. The
postprocess-uniform shader, its artifact and its lock entry are gone. `mulciber-material-scene`
moves to `probes/material-scene`. The probes build their cubes and instances as material records;
`mulciber-api-conformance`'s first case, a non-finite transform, which a material record has no
transform to carry, is now a record uniform shorter than its pipeline declares. The docs for the
removed API are marked historical, with their dated evidence kept.

On Linux (Vulkan, validation layer enabled), every remaining example and probe ran without a
validation message, `mulciber-api-conformance` passing its 100 cases and its `--pacing` run. The
workspace passes Clippy for Linux, Windows and macOS (Metal type-checked with placeholder shaders),
its unit tests and its ignored tests. Nothing was run on Metal or on Windows, and
`scripts/validate-windows.ps1`, whose resize smoke now runs `mulciber-game-slice` in place of the
postprocess cube, has not been run since.

## Smaller API fixes (graphics 0.17.0, runtime 0.6.0, net 0.2.0, model 0.3.0, texture 0.5.0, audio 0.3.0)

Breaking unless noted.

- `Device::create_mesh(&MeshDescriptor { vertices, parts })` replaces `create_mesh`,
  `create_mesh_with_parts`, `create_mesh_with_layout` and `create_mesh_with_layout_and_parts`;
  `vertices` is `MeshVertices::Fixed(&[Vertex])` or `MeshVertices::Bytes { layout, bytes }`. Fixed
  meshes can now take 32-bit index parts, which no method offered.
- `Surface::set_vsync` is removed; it was `set_presentation_mode` with `Synchronized` or
  `Immediate`.
- `Runtime::handle_window_event(event, now)` and `Server::new(transport, config, now)` take the time
  instead of reading the wall clock, as every other call already did. A server's challenge cookies
  now age on the caller's clock, which a new test checks with a fake one.
- `Server::broadcast` queues to every client and returns those it could not, each with its
  `SendError`; it used to stop at the first failure, leaving the rest without the message.
- `bake_materials` returns every texture's result beside its output, so one failure no longer hides
  what was baked; only discovery fails the call. `BakeMode::{IfStale, Always}` replaces the
  `force: bool` of it and `bake`. `mulciber-texture bake` prints each bake and failure and fails when
  any did.
- `Model::duplicate_double_sided` clears `double_sided` once the back faces are geometry, so a
  renderer that disables culling for double-sided materials no longer draws both copies, and a
  second call adds nothing.
- `AudioEngine::hrtf_error` (additive) says why the HRTF sphere did not load when the engine fell back
  to the stereo pan, which it used to do silently.

## One way to play a clip (audio 0.3.0)

Breaking. `Controls` had a method per combination of bus, placement, handle and looping (`play`,
`play_tracked`, `play_bed`, `play_spatial`, `play_spatial_tracked`, `play_spatial_music` and
`start_music`), each taking up to five positional arguments and each missing some: a tracked
spatial voice could not have an emitter, music in the world had no rate, a bed no pitch. They are
one call taking how to play:

```rust
let waterfall = controls.play(&sound, Play {
    bus: Bus::Ambience,
    placement: Placement::World { position, emitter: Some(rock) }, // or Placement::Flat, Placement::at(p)
    fade_in: 2.0,
    looping: true,
    ..Play::default() // 0 dB, rate 1, effects bus, unpositioned, once
});
```

`play` always returns the `VoiceId`, which a one-shot ignores. The reverb send is the rule it was:
effects, and anything placed in the world, sound in the room; unpositioned music and ambience
don't. `stop_music` fades every unpositioned voice on the music bus, as it faded `start_music`'s.
`play_spatial_stream` stays as it was, since it returns the stream to feed. New combinations fall out
of the fields, such as the placed, fading, looping ambience above, which a new test plays.

## One texture upload, one scene verb, HDR as a setting (graphics 0.17.0, texture 0.5.0)

Breaking. `Device` had a method per combination of format, shape, mip source and input type,
`create_rgba16_float_cube_array_texture_with_mips_from_bits` among them: 25 constructors and six
updates, with holes where a combination had no method. They are replaced by one call taking what
the texture is:

```rust
let texture = device.create_texture(&TextureDescriptor {
    shape: TextureShape::D2 { width, height }, // or Cube { size }, CubeArray { size, layers }
    mips: MipChain::Generated,                 // or Single, Supplied
    data: TexelData::Rgba8Srgb(&[&texels]),    // Rgba8Unorm, BlockCompressed, Rgba16Float(Bits)
})?;
device.update_texture(&heights, TexelData::Rgba16Float(&[&level_zero]))?;
```

`data` holds one image per layer, face and level, in that order, so a cube with its chain is its
six faces' chains one after another (KTX 2.0 nests level outermost; a KTX cube or array would need
reordering).
`Ktx2Texture::descriptor` gives a parsed file's upload, replacing `create_ktx2_texture`.
`update_texture` takes the extent and mip mode from the texture: one image for a single-level
texture, the complete chain for one created with a supplied chain, and for one created with
generated mips either its base level (the GPU regenerates the rest) or the complete chain.
Validation and conversion are unchanged and diagnostics still name the layer, face and level; a
wrong number of images is reported as one count for the whole texture, and a few messages are
reworded for the new call. Combinations
that never had a method are refused by name: generated mips are 2D only and a cube array takes
`RGBA16Float` with a supplied chain (both `Unsupported`), and block-compressed data cannot generate
mips (`InvalidRequest`). Cube textures can now take binary16 bits (`Rgba16FloatBits`), which had no
method; the bytes reach the backend exactly as `RGBA16Float` cubes' do.

`Queue::render_and_present` is now the only way to draw a frame. `draw_textured_and_present`,
`draw_textured_scene_and_present`, `draw_textured_postprocessed_and_present` and
`draw_textured_scene_postprocessed_and_present` are removed with `TexturedDraw`, `TexturedScene`,
`PostprocessedDraw` and `PostprocessedScene`; a single draw is a one-element `Scene::Textured`.
`SceneSubmission` is now `{ scene, clear }`, and the parts that only compose with material
records live under `Scene::Material(MaterialScene { records, shadow, output })`: the shadow
prepass on `MaterialScene`, and the foreground split, offscreen passes and overlay on
`MaterialOutput::Postprocessed`. Submissions that `render_and_present` used to refuse at run time
(a shadow, offscreen pass or overlay on textured or instanced content, offscreen passes or an
overlay or a foreground without postprocessing) can no longer be written. `SceneContent` and
`MaterialWithForeground` are gone; `foreground_start: Option<usize>` replaces the latter, and
`overlay` is a slice, empty for none, where `Some(&[])` was an error. The postprocess pipeline,
targets and uniform travel together as `Postprocess`, in `SceneOutput::Postprocessed` and
`MaterialOutput::Postprocessed` alike.

HDR is a setting rather than part of a method name. `MaterialPipelineDescriptor::scene_color`
(`SceneColor::Surface` or `SceneColor::Hdr`) replaces `create_hdr_material_pipeline` and
`validate_hdr`. `PostprocessPipelineDescriptor::hdr: Option<HdrComposite { bloom, volume }>`
replaces `create_hdr_postprocess_pipeline`, `create_volumetric_hdr_postprocess_pipeline` and
`create_hdr_composite_pipeline`, so `create_postprocess_pipeline` makes every postprocess pipeline,
and bloom and scattering can only be asked of an HDR composite. `create_postprocess_targets(info,
scale, scene_color)` replaces itself, `create_scaled_postprocess_targets` and
`create_scaled_hdr_postprocess_targets`, which had no unscaled HDR form. Each was a thin wrapper
over the same code, so behaviour and errors are unchanged.

`mulciber-texture` 0.5.0 moves to `mulciber` 0.17.0 and uploads through `create_texture`.

The examples, probes and docs move to the new calls. Probe checks that the types now make
unwritable are gone: `mulciber-api-conformance` step 17 (an overlay with direct output), and in
`mulciber-float-texture` the updates at a wrong extent, which `update_texture` cannot be given,
became updates whose levels are the wrong size; `mulciber-cube-array-texture`'s face without
levels is now a short image list (refused by count) and an empty face (refused naming layer 1, -Z).
Nothing was run on a GPU: the workspace builds and passes Clippy for Linux, Windows and macOS
(Metal type-checked with placeholder shaders) apart from the `float_cmp` findings in
`mulciber-model` and `mulciber-shader` tests that predate this change, and the unit tests pass,
including new ones for the image count, refused combinations and per-face naming. The native
upload and draw calls behind the new API are the ones the old methods made.

## Khronos PBR Neutral, and the shoulder's path to white (shader 0.5.6)

`mulciber::tonemap` adds `pbr_neutral`, Khronos's PBR Neutral as Bevy 0.20 ships it, beside
`hue_preserving_shoulder`, which is unchanged. The shoulder keeps a colour's channel ratios however
bright it gets, so a saturated sky or lamp overexposed by a stop or two stays saturated all the way
up and its gradients flatten into one band just under white. PBR Neutral compresses the brightest
channel the same way (from 0.76, toward 1) and then mixes the colour toward white by how far it was
compressed, as film and the eye whiten very bright light. It also has a toe: up to 0.04 comes off
every channel by the darkest one, quadratically near black (6.25x² below 0.08), which deepens
shadows and the colour in them. That makes it a different look, not a drop-in: a game switching to
it should expect darker, richer shadows and re-check its exposure.

`pbr_neutral_inverse` gives back the exposed colour a shown one came from, exactly and in closed
form (the shown colour's brightest channel gives the compressed peak, hence the peak and the
whitening; the darkest channel past them gives the toe), for measuring a captured frame's light.
The tests check the toe, the compression of greys, monotonicity, whitening that only grows with
brightness, and the inverse's round trip from 2⁻¹⁶ to 2⁷ over greys and saturated colours.

`whitening_shoulder` is `hue_preserving_shoulder` with PBR Neutral's path to white and nothing else:
identical up to 0.6 and in its brightest channel above it, the colour mixed toward white by how far
the shoulder compressed it, for a game that wants highlights to whiten without PBR Neutral's toe
recolouring its shadows. `whitening_shoulder_inverse` undoes it exactly, the shoulder's peak taken
back as `(0.36 − 0.2n) / (1 − n)`. Its tests check it against the shoulder, that greys are left as the
shoulder leaves them, monotonicity, whitening that only grows, and the round trip.

## Rooms decay by Sabine's formula (audio 0.2.0)

`probe_room` mapped the mean ray length onto a 0..1 `room` from 3 m to 25 m, which the reverb
turned into comb feedback 0.70..0.97 (about 0.6 to 7 s). Indoors at human scale nearly every room
fell below 3 m and decayed in the same 0.6 s, whatever it was made of: a tiled washroom and a
carpeted study alike, a marble hall barely longer. Hardness only darkened the tail. And a ray that
escaped counted as 40 m long, so a covered walk open along one side rang longer than a closed hall.

Now `RoomAcoustics::decay` is the tail's RT60 in seconds, and the probe works it out as Sabine's
formula does: the mean distance of the hits stands for the mean free path, and the absorption is
the rays' mean, a hit absorbing 0.05 at hardness 1 and 0.35 at hardness 0, an escaped ray all of
it, as an open window does. Probed boxes land within a quarter of Sabine's decay for a washroom, a
study and a hall. The wet level rises gently with the decay (0.3 at 0.4 s, 0.1 more each time it
doubles, between 0.25 and 0.6), still gated by the enclosure. The reverb sets each comb's feedback
from its own length so all of them fall 60 dB in the decay asked for (held to 0.1..10 s), which a
test measures from the impulse response's backward-integrated energy.

Breaking: `RoomAcoustics::room` (0..1) is replaced by `decay` (seconds); `RoomAcoustics::DRY` has
a decay of 0.

## A material shipped without its sources keeps its maps (texture 0.4.1)

`MaterialMaps::beside` found a material's normal + roughness and metallic + occlusion textures by
their source images only, so a game that shipped its bakes without the sources (as a release
should, the bakes being what's drawn) lost those maps: the albedo was read from its bake, but the
surface drew flat and at the default roughness. Now a texture none of whose sources are there is
still the material's if its bake is (`name_normal.ktx2`, `name_metal.ktx2`): its recipe names the
first source it would have, missing, so `Recipe::prepare` reads the bake as it is, as it already did
for an albedo without its image. A source removed on purpose, to drop a map, now leaves its bake in
use until the bake is deleted too.

## Cube texture arrays (graphics 0.16.0, shader 0.5.5, texture 0.4.0)

`Device::create_rgba16_float_cube_array_texture_with_mips(size, &[[&[&[[f32; 4]]]; 6]])` and its
`_from_bits` peer upload one or more `RGBA16Float` cubes of one extent, each face a complete mip
chain in the single cube's face order and orientation, as one `Texture` whose `dimension()` is the
new `TextureDimension::CubeArray`. A material pipeline declares it with
`MaterialBinding::CubeTextureArray { binding }` and samples WGSL `texture_cube_array<f32>` with
`textureSampleLevel(map, sampler, direction, layer, lod)`, so a game can bind every room's
reflection probe in one draw. Diagnostics name the layer and face (`cube array layer 1: cube face
-Z: ...`); no layers is refused; pipeline creation refuses declarations that disagree with the
artifact and submission textures of another dimension, as for cubes. Creating the texture or a
pipeline declaring the slot is `Unsupported` without Vulkan's `imageCubeArray` feature, which the
device now enables wherever the adapter offers it, or outside Metal's Metal 3 family.

`mulciber-shader` 0.5.5 validates with Naga's `CUBE_ARRAY_TEXTURES` capability (in `compile_wgsl`,
module composition and host fields) and records `texture_cube_array<f32>` as binding kind 9,
reflected as `ShaderBindingKind::CubeTextureArray`; Mulciber before 0.16.0 rejects artifacts that
contain it.

Breaking: `TextureDimension` has a new variant; exhaustive matches need a `CubeArray` arm.

Tested on Metal (Apple M2, macOS 15.8) by the new `mulciber-cube-array-texture` probe under Metal
API Validation: 110 readback cases (every cube, face and level of a three-cube array, in-face
orientation in both cubes of a second, a `_from_bits` upload, the level and cube counts the shader
reads) and six refusals. `mulciber-cube-texture` ran on Metal for the first time and passed its 72
cases. The Vulkan path compiles and passes Clippy for Windows and Linux but has not run. See
[cube texture arrays](docs/cube-textures.md#cube-texture-arrays).

`mulciber-texture` 0.4.0 only moves to `mulciber` 0.16.0, whose types its recipes take.

## Render textures (graphics 0.15.0, texture 0.3.0)

`Device::create_hdr_render_texture(width, height)` makes a linear `RGBA16Float` color texture with
D32 depth and multisample color of its own at the session's sample count; `destroy_render_texture`
frees all three. `SceneSubmission::offscreen` takes `OffscreenPass`es (a target, HDR material
records, a clear), drawn in order after any shadow prepass and before the scene pass, with
material content and postprocessed output; `RenderTexture::texture()` then feeds any material
record's texture slot that frame or later, so a game can draw something through its own world
pipelines and a camera of its own once and show it on its HUD for as long as it stays the same.
Sampling a render texture nothing has rendered, an offscreen record sampling its own target or
scene depth, two passes on one target, and CPU updates of a render texture are refused by name.

Breaking: `SceneSubmission` has a new public field; literals need `offscreen: &[]`.

Tested on Metal (Apple M2, macOS 15.8) by the new `mulciber-render-texture` probe under Metal API
Validation at four samples and one: the refusals, then four captured frames matched pixel by
pixel (rendered and sampled in one frame, sampled again frames later, re-rendered while earlier
frames still sampled it). The Vulkan path compiles and passes clippy for Windows but has not run.
See [render textures](docs/render-textures.md).

`mulciber-texture` 0.3.0 only moves to `mulciber` 0.15.0, whose types its recipes take.

## Skins and animations (model 0.2.0)

`Model` has a `skeleton` and `animations` when its scene has a skin or an animation of its nodes;
without either, both are empty and the model loads as before. `Skeleton` holds every node of the
scene, parents first, with its name, parent and rest `Transform` (translation, rotation, scale), and
the `Joint`s of the palette: each skin's joints with their inverse bind matrices, a joint per node
carrying rigid meshes (the inverse of its rest transform), and one that never moves. Every `Part`
then has `joints` and `weights`, four per vertex, carried through flat normals, tangent splitting and
double-sided copies. A skinned primitive keeps its four heaviest of `JOINTS_0`/`WEIGHTS_0` and
`_1`, weights made to sum to 1, its vertices in the skin's bind space (glTF ignores the skinned
node's own transform). A mesh that isn't skinned but hangs under an animated node or a skin's joint
is placed at rest and bound wholly to the nearest such node, so figures built of rigid pieces on a
rig animate without skin weights.

`Animation`s keep their name, duration and `Channel`s (a node's translation, rotation or scale,
step, linear or cubic spline; morph weights and nodes outside the scene are left out; a channel
whose keys don't match its times is an error). `Animation::sample` sets the nodes it moves in a
`Pose`, holding the end keys outside them, so animations layer. `Pose::blend` cross-fades two poses
(rotations by `slerp`, along the shorter arc); `Pose::blend_masked` blends only the nodes of a mask,
`Skeleton::subtree` gives one, for an upper body swinging over walking legs. `Skeleton::palette`
gives a bone matrix per joint for a skinned vertex shader, `Skeleton::node_matrices` each node's
matrix for attaching things to it, both in the model's frame after `Model::transform`, which now
also moves the skeleton (`Skeleton::root`).

Breaking: `Part` and `Model` have new public fields; literals need `joints`, `weights`, `skeleton`
and `animations` (or `..Default::default()`).

Tested on glTF files written by the tests (a skinned quad bending at an arm joint, upright after
`Y_UP_TO_Z_UP` too; rigid quads riding an animated node; step, linear and cubic-spline keys; masked
blends), and on three of Khronos's glTF sample models, turned Z up and sampled at 21 times through
each animation: `CesiumMan` (19 joints, 3,572 vertices, loaded in 7.5 ms; its mesh is stored lying
down and stands 1.51 m tall when posed), `Fox` (24 joints, Survey, Walk and Run) and
`RiggedFigure`. Every weight sums to 1, every joint is in range, and every posed vertex is finite
and stays within the figure's size. These are numeric checks; nothing was drawn. It does no GPU
work, so it is the same on Vulkan and Metal.

## Model loading (model 0.1.0)

New `mulciber-model` crate: `Model::load` reads a `.gltf` (its buffers in files or data URIs) or a
`.glb` through the `gltf` crate (1.4.1). The default scene's nodes, else the first scene's, are
walked with their transforms baked into the vertices (normals by the inverse transpose), and each
material's primitives are merged into one `Part` of positions, unit normals, tangents, texture
coordinate set 0 and a triangle list. Strips and fans are unrolled facing the way their first
triangle does; points and lines are left out. A primitive without normals is split into flat
triangles, as glTF asks; one without tangents gets MikkTSpace's from `bevy_mikktspace` (1.0.0), fed
v turned over so the handedness is a Blender export's, each vertex split where its corners' tangents
differ. `Material` is glTF's metallic-roughness material as stated, with
`KHR_materials_emissive_strength`; `Image` is a file resolved against the model's folder
(percent-decoded) or embedded bytes with their media type, not decoded. `Model::transform` applies a
matrix, turning the winding and the tangents' handedness when it mirrors; `Y_UP_TO_Z_UP` is the
quarter turn for a Z-up world. `Model::duplicate_double_sided` adds reversed back faces for
double-sided materials, their tangents' handedness turned so normal maps read the same from either
side. Skins, morph targets, animations, cameras and lights are not read yet.

Tested on glTF and GLB files written by the tests, and on Poly Haven's `mantel_clock_01` (1k glTF,
no tangents in the file): 25,721 triangles in two parts, every normal unit, every tangent across its
normal, loaded in 34 ms on an Apple M2. It does no GPU work, so it is the same on Vulkan and Metal.

## Progressive bloom upsampling (graphics 0.14.0, texture 0.2.0)

`BloomShaders` has an optional `upsample` filter. With it, after the downsampling, each level is
read from the smallest up and the filter's output is blended one-to-one into the next larger
level, whose contents are kept, so the first level ends up holding the whole bloom and the
composite reads it alone at binding 3. The chain then halves past six levels until a level's
smaller side is at most `BLOOM_SMALLEST` (16) texels, to at most twelve, so its coarsest level
covers about the same share of the screen at any resolution. Without an upsample filter the six
levels and the six-binding composite are as before. Sampling small levels straight to the screen
showed their texels as a coarse grid round bright lights; the tent at every step up comes back
smooth (Jimenez, SIGGRAPH 2014).

Breaking: `BloomShaders` has a new public field; literals need `upsample: None` for the old
behaviour. Metal is exercised on an Apple M2 by Shiplike; Vulkan compiles and passes Clippy for
Linux but hasn't run on a device yet.

`mulciber-texture` 0.2.0 only moves to `mulciber` 0.14.0, whose types its recipes take.

## A probe's answer says how long it waited (net 0.1.3)

A server reads probes when its game calls `Server::update`, once a tick, so the round trip to a
server ticking every 15 ms came out up to 15 ms long, differently each time. A `ProbeAnswer` now
says how long the probe waited between arriving (stamped by the transport, as acks' are) and being
answered, to 100 µs, as `held`: the round trip less that is the network's, as the connection's
round-trip time is. The answer is 12 bytes, still smaller than the probe. 0.1.2's answers don't
read as 0.1.3's, nor the other way.

## Probes (net 0.1.2)

A server answers a `Probe` packet from anyone, connected or not, at once: `probe(protocol, token)`
builds one (17 bytes) and `probe_answer(bytes)` reads the answer, which carries the token back and
whether the server runs the same protocol. A server list sends one to each server it shows and
times the answer, for its ping, without joining. The answer is 10 bytes and the server keeps
nothing for it, so a forged source address gets a third party less than the forger sent. Full
servers answer too. Clients ignore the new packet kinds.

## A signal no longer stops the UDP transport (net 0.1.1)

`UdpTransport`'s receiving thread treated an interrupted wait (`EINTR`: a signal arriving, such as a
debugger attaching or the terminal stopping and continuing the process) as a broken socket, so every
later `receive` failed and a dedicated server quit. It now waits again, as it does for a timeout.

## KTX 2.0 uploads and the texture baker (graphics 0.13.36, texture 0.1.0)

`Ktx2Texture::parse` reads a KTX 2.0 file in place, through the `ktx2` crate (0.5.0), and accepts
only what the GPU samples directly: one 2D image, not supercompressed, in a `BlockCompression`
encoding, holding its base level alone or the complete chain to 1×1, each level exactly its extent
in blocks. Anything else is refused by name: `InvalidRequest` for a malformed file, `Unsupported`
for another format, a cube, array or 3D texture, or supercompression.
`Device::create_ktx2_texture` uploads it through the block-compressed uploads, and
`ktx2_vk_format` gives each encoding's `VkFormat`.

New `mulciber-texture` crate, Isle of Rán's texture baker made game-agnostic. A `Recipe` packs four
channels from images or constants, builds the mip chain with the filter its content needs (`Color`
in linear light, `ColorFlatDistant`, `Cutout` keeping alpha coverage, `Normal` as renormalised
vectors with a scalar in alpha, `Linear`), encodes every level as BC7 with Intel's ISPC encoder
(slow settings) and writes KTX 2.0 with the digest of its sources, the base level's mean and its
smallest alpha. Any size bakes, not only square powers of two. `Recipe::prepare` reads the bake while
it is current (or when its sources are not shipped) and otherwise builds the same chain from the
sources in RGBA8, saying why; `Prepared::upload` uploads either, a `Color` or `Linear` fallback as
its base level with GPU-generated mips (the sRGB and UNORM `_with_generated_mips` uploads), the
others with their CPU chains. Bakes keep CPU chains, since every level is encoded. `MaterialMaps` is a physically based
material as three textures found beside its albedo by suffix: albedo (sRGB, alpha = opacity), normal
+ perceptual roughness (Isle's packing) from `_normal` and `_rough`, and metallic + occlusion from
`_metal` and `_ao`, with `MaterialDefaults` (roughness 0.8, not metal, unoccluded) for maps that are
not there. The `mulciber-texture` CLI bakes (`bake <dir>... [--force]`) and checks (`check <dir>...`)
whole directories, and `run` lets a game's own bake binary forward to it. The encoder is behind the
default `encode` feature, so a game's runtime builds without it. Tests cover the chains, packing,
digests, the bake and fallback cycle and BC7 quality, decoded with `bcdec_rs`.

## Binary16 uploads and GPU-generated mips (graphics 0.13.36)

Every `RGBA16Float` creation and update function gains a `_from_bits` form taking binary16 bit
patterns (`[u16; 4]` texels) that upload without conversion; infinity and NaN are still rejected.
`create_rgba16_float_texture_with_generated_mips`, `create_rgba8_srgb_texture_with_generated_mips`
and `create_rgba8_unorm_texture_with_generated_mips` upload level 0 and generate the full chain on
the GPU, and `update_rgba16_float_texture_with_generated_mips` replaces level 0 of such a texture
and regenerates the chain in the next scene submission, so per-frame float textures need no CPU
downsampling. Vulkan blits level by level (`vkCmdBlitImage2`, loaded and its `BLIT` stage bit added
to the generated bindings); Metal uses `generateMipmapsForTexture:`. Additive. An ignored native
Vulkan test reads back exact box averages under validation; Metal has not run. See
[float texture uploads](docs/float-texture-uploads.md).

## Shader reflection and device-free pipeline checks (graphics 0.13.36, shader 0.5.4)

`mulciber-shader` now writes a `MULSHDR4` container whose interface adds, for every uniform and
storage binding, its memory layout: the WGSL type name and, for a struct, each member's name, byte
offset, size and type. `ShaderArtifact::reflect()` returns the recorded interface as a
`ShaderReflection`: entry points (`ShaderStage`, name, `ShaderVertexInput` location and
`VertexFormat`, used `(group, binding)` pairs) and `ShaderBinding`s (group, binding,
`ShaderBindingKind`, byte size, optional `BufferLayout` of `BufferMember`s).
`MaterialPipelineDescriptor::validate()` and `validate_hdr()` run the declaration checks of
`create_material_pipeline` and `create_hdr_material_pipeline` without a device. Additive for
`mulciber`: `MULSHDR3` and `MULSHDR2` artifacts stay readable and reflect without layouts. Older
`mulciber` releases reject `MULSHDR4` by header, as with the previous container bumps. Checked by
container round-trip tests in both crates.

## Per-axis sampler addressing (graphics 0.13.36)

`MaterialBinding::SamplerPerAxis { binding, filter, address: SamplerAddressPerAxis { u, v, w } }`
declares a material sampler whose address mode differs per axis, such as an equirectangular sky
that repeats in `u` and clamps in `v`. `MaterialBinding::Sampler` keeps one mode for every axis;
`SamplerAddressPerAxis::all` and `From<SamplerAddress>` build the uniform case. Additive: existing
declarations are unchanged. Vulkan fills `addressModeU/V/W` from the three fields; Metal now also
sets the R address mode, which 2D sampling never reads. Checked by a binding-validation test and
by Clippy on Linux and on the `aarch64-apple-darwin` target; neither backend has rendered it here.

## Shared lighting library (shader 0.5.4)

`mulciber-shader` ships Mulciber's shading model as WGSL modules every game can import, so lighting
code is no longer copied between games:

- `mulciber::photometry`: SI photometric units (lumens, candelas, lux, nits), lumens to candela for
  point lights (Φ/4π), spots (Φ/π, Filament's unfocused spot) and focused spots, Filament's
  windowed inverse-square falloff `I / max(d², 0.01²) · saturate(1 − (d/r)⁴)²`, its spot cone
  falloff, EV100 from camera settings or metered luminance, `exposure_from_ev100` (`1 / (1.2 ·
  2^EV100)`) and pre-exposure.
- `mulciber::pbr`: Lambert, GGX, exact height-correlated Smith visibility, Schlick with f90,
  metallic-workflow F0, perceptual roughness clamped at 0.089 and squared, and `punctual_light`,
  which returns diffuse and specular luminance separately. The split sum reads Filament's
  multiple-scattering DFG table, with its energy compensation and a roughness-to-LOD mapping.
- `mulciber::tonemap`: Isle of Rán's hue-preserving shoulder, identity below 0.6, unchanged in
  tuning.

`bake_dfg_table` bakes the 128 × 128 DFG table on the CPU (Filament's layout, Hammersley importance
sampling, deterministic) for a `build.rs`. The tests run on host evaluators generated from the
modules: GGX normalisation, reciprocity, a white furnace that stays below 1 and matches the baked
table, the table's smooth limit, falloff, exposure, and the tone mapper's identity, monotonicity
and hue. A lit shader importing all three passed `spirv-val`; Metal is checked as generated MSL
only. `mulciber::color` is renamed `mulciber::colorspace`, because importing a module reserves its
last path segment and `color` is a common local name.

## WGSL module imports (shader 0.5.4)

`mulciber-shader` composes shaders from importable WGSL modules with naga_oil 0.23.0, Bevy's
composer, on the same naga 30. `ShaderModules` registers modules that name themselves with
`#define_import_path`, from strings (`add_source`), files (`add_file`) or directory trees
(`add_dir`, which skips top-level shaders), and prints `cargo::rerun-if-changed` for what it read
(`rerun_if_changed`). `modules.shader(path)` gives a `WgslShader` that takes `#ifdef` shader defs
(`define`) and writes the same validated `MULSHDR3` artifact as `compile_wgsl` (`compile_wgsl`), a
host field (`compile_host_field`, which also accepts qualified module functions such as
`game::lighting::falloff`), or a build-cache key (`cache_key`). `ShaderModules::compile_host_field`
generates host evaluators straight from module functions, for CPU code that shares shader maths.
Errors point at the file and line in the module where they occur, a missing module lists the
registered ones, and import cycles are refused by name.

Modules under the reserved `mulciber::` namespace ship inside the crate and are in every set; the
first is `mulciber::colorspace` (sRGB transfer functions and BT.709 luminance). The CLI takes
`--modules <dir|file>` and `--define NAME[=VALUE]`. `compile_wgsl` and `compile_host_field` are
unchanged, and `ShaderBuildError`'s `Debug` now prints its message as written, so diagnostics stay
readable through `expect`. Vulkan artifacts composed from two modules and the engine module passed
`spirv-val` on Linux; Metal output is checked as generated MSL only.

## Audio mixer, HRTF and room reverb (audio 0.1.0)

New `mulciber-audio` crate, the engine from Isle of Rán's audio module made game-agnostic: a
callback-thread mixer with pitchable voices, effects/music/ambience buses, music and bed fades,
moving emitters, HRTF spatialization with a stereo-pan fallback, and a Freeverb room bus.
`Mixer::render` mixes offline into any buffer; `AudioEngine` runs it on the default cpal device.
`probe_room` sizes the reverb from a game-supplied ray cast and `RoomTracker` eases it, as Rust
Voxel's cave probe did. HRTF directions now reach the sphere with its front on -Z; the ported
code had front and back swapped.

## Entity component store (ecs 0.1.0)

New crate `mulciber-ecs`, independent of the graphics crates: generational entities and one sparse
set per component type, with `query`, `query_mut`, `query2` and `query2_mut`. It is Isle of Ran's
store, made a library: no systems, schedules or built-in components. Slots whose generation is
spent retire instead of wrapping back to a generation old handles could carry, and component
storages are found by passing the `TypeId`'s hash through rather than running SipHash on it.

## Fewer Vulkan buffer binds (graphics 0.13.35)

- Vulkan material and shadow passes skip vertex binds that repeat the previous draw's, and bind
  each index buffer once from its start, picking each draw's indices with `firstIndex`; parts that
  share a mesh arena block share one index bind.

With 0.13.34's changes, The Ship's viewer (cotopaxi, 3641 material records a frame) spends 0.92 ms
instead of 1.54 ms in `render_and_present` on the CPU, median over 340 frames.

## Faster Vulkan frame recording (graphics 0.13.34)

- Index range checks for meshes and transient geometry take the largest index, which vectorizes,
  instead of stopping at the first bad one.
- Vulkan maps the per-frame buffers (uniforms, record storage, instances, transient geometry,
  post-processing uniforms) once when it creates them, instead of mapping and unmapping each one
  on every frame's write.
- Vulkan finds each record's cached descriptor set by hashing instead of scanning every set its
  pipeline has cached. Material and shadow pipelines key on the record's sampled tuple held inline
  (at most `MATERIAL_TEXTURE_COUNT_LIMIT` identities, the cap every texture, shadow map and
  scene-depth slot counts against), and a hit is probed with the record's own slice, so lookups no
  longer compare against every cached tuple and a new tuple no longer allocates its key. Fixed
  textured and postprocess pipelines key on resource and frame slot the same way. The maps use a
  small FxHash-style hasher and are cleared at exactly the pool resets that cleared the lists. Metal
  binds textures per draw and has no such cache. In the ignored test `sampled_lookup_timing`
  (release, 3000 lookups of four-identity tuples), a frame's lookups went from 0.13 to 0.024 ms over
  100 tuples and from 0.50 to 0.028 ms over 500; at 16 tuples the scan's 0.056 ms becomes 0.025 ms.
  The Ship's viewer renders cotopaxi pixel-identically with a held clock.

## Faster float texture conversion (graphics 0.13.33)

`RGBA16Float` creation and replacement convert to binary16 about 15 times faster: F16C, eight values
per instruction, on x86-64 CPUs that have it, and otherwise a branch-free form of the same
round-to-nearest-even that compilers vectorize. The range check moved into the same pass. Output is
bit-identical to the previous converter for every `f32`: the ignored test
`fast_paths_match_reference_exhaustively` checks all 2^32 bit patterns through both paths (passed
on x86-64 with F16C), and a sampled version runs with the normal tests. In The Ship's viewer, six
128x128 ocean tiles with mip chains per frame went from 2.0 to 0.9 ms, the conversion alone from
1.16 to 0.07 ms.

## Mipped float texture replacement (graphics 0.13.32)

`Device::update_rgba16_float_texture_with_mips(&texture, width, height, &levels)` replaces every
level of a texture made by `create_rgba16_float_texture_with_mips`, with the same queuing as
`update_rgba16_float_texture`: input is converted and copied now, the whole chain is uploaded before
draws of the next textured/material submission, pending writes coalesce, earlier frames keep their
contents and nothing waits for device idle. The chain must be complete and match the texture's
dimensions and mip count. Both calls share one path, a single level being a chain of one. Vulkan
sizes each frame slot's staging to the whole chain and copies a region per level, with barriers
over every level; Metal blits each level from its own 256-byte-aligned rows. The float-texture
probe now replaces its mipped texture's full chain every frame; it passed all 40 cases on
Linux/NVIDIA under Vulkan validation and synchronization validation. Metal passes Clippy but has
never run. See [mip chain replacement](docs/float-texture-uploads.md#mip-chain-replacement).

## Rust 1.99 and current dependencies (graphics 0.13.32, platform 0.5.6, shader 0.5.3)

The pinned toolchain moves from 1.98.1 to 1.99.0 (the MSRV stays 1.97). Examples, probes and
comparisons move to glam 0.34.1, the Vulkan triangle probe to naga 30.0.1, the wgpu comparisons to
wgpu 30.0.1 and pollster 1.0.1, the Metal comparison to bytemuck 1.25.2, and the binding generator
to bindgen 0.73.2. Three tests use `assert_eq!` against an empty array for clippy 1.99's
`assert_is_empty`.

## Modifier keys as physical keys (platform 0.5.6)

`KeyCode` gains `ShiftLeft`/`ShiftRight`, `ControlLeft`/`ControlRight`, `AltLeft`/`AltRight`,
`SuperLeft`/`SuperRight` and `CapsLock`, and every backend now reports those keys' own press and
release as `InputEvent::Keyboard` in addition to `ModifiersChanged`, so games can bind them (Source's
duck is Ctrl). They do not auto-repeat. AppKit decodes them from `flagsChanged` and its
device-dependent bits; Win32 maps their scan codes; Wayland and X11 share the evdev table.

## Unaccelerated Wayland pointer deltas (platform 0.5.6)

While the pointer is captured, Wayland's `PointerDelta` now carries the unaccelerated motion from
`zwp_relative_pointer_v1` instead of the accelerated pair, matching the raw input Win32 already
reports, so turning speed no longer depends on how fast the mouse moves. X11 (warp deltas) and
AppKit (`NSEvent` deltas) still report accelerated motion.

## Frame capture (graphics 0.13.32)

`Surface::request_frame_capture()` asks for the next acquired frame to be read back when it is
presented, and `Surface::take_frame_capture()` returns it as a `FrameCapture`: the frame's
presented index, width, height and tightly packed, top-down RGBA8 pixels in the sRGB encoding the
display receives, whatever the swapchain's channel order, with alpha 255 on an opaque-composited
surface. Every `Queue` presenting verb captures, so direct, postprocessed, HDR, bloom, volumetric
and overlaid frames all read back their final color; the presenting call blocks until the GPU
finishes that frame, and frames without a request pay nothing. Abandoned or failed frames leave the
request pending. Vulkan swapchain images now carry `VK_IMAGE_USAGE_TRANSFER_SRC_BIT` wherever the
surface allows it, and a surface that does not answers the request with `Unsupported`; the copy
runs in the frame's own command buffer before the presentation transition. Metal turns
`framebufferOnly` off only while a capture is pending and blits the drawable before presenting.
The new `mulciber-frame-capture` probe matched direct (4x resolved and 1x), HDR-composited and
overlaid captures pixel by pixel on Linux/NVIDIA under Vulkan validation and synchronization
validation, on Wayland (`R8G8B8A8_SRGB`) and XWayland (`B8G8R8A8_SRGB`); Metal is implemented but
has never run. See [frame capture](docs/frame-capture.md).

## Per-entry-point resource bindings (graphics 0.13.32, shader 0.5.3)

`mulciber-shader` now records the bindings each entry point uses, directly or through called
functions, in a `MULSHDR3` container, and material and shadow pipelines validate their declaration
against the union of their own vertex and fragment entry points' bindings instead of the whole
module. One WGSL module can hold a plain and a skinned vertex stage sharing a fragment stage, with
the plain pipeline declaring no storage slot and the skinned one declaring its bone palette. A
declared slot the module records but the pair never uses is refused by name. `MULSHDR2` artifacts
stay readable with every binding attributed to every entry point, so they validate exactly as
before; older `mulciber` releases reject `MULSHDR3` by header. Neither backend changed: Naga's MSL
already takes arguments only for an entry point's own globals, which a unit test now checks. The new
`mulciber-entry-bindings` probe drew both pipelines from one module on Linux/NVIDIA under Vulkan
validation; Metal has not run. See [per-entry-point bindings](docs/per-entry-point-bindings.md).

## Vulkan per-frame regions sized for every frame in flight (graphics 0.13.32)

The Vulkan session created its record storage, transient geometry, instance transform and record
instance buffers with room for one frame's initial region, while every frame writes and binds its
region at its frame slot's base. Until a frame needed more than the initial capacity and the
buffer grew (growth already allocated a region per frame in flight), a frame in the second slot
wrote and bound past the end of the buffer: a skinned record with a palette of 256 bytes or less,
drawn in the first frames, failed the debug-build capacity assertion and in release wrote past the
mapped allocation. The initial buffers now hold a region per frame in flight, as the uniform buffer
always did. Found by the `mulciber-entry-bindings` probe's 64-byte bone palette; Metal keeps
separate per-frame buffers and was not affected.

## Packed vertex formats (graphics 0.13.32)

`VertexFormat` gains `Uint8x4`, `Unorm8x4`, `Uint16x2`, `Uint16x4`, `Unorm16x2` and `Unorm16x4`,
fetched narrow and read by the shader as `vec4<u32>`, `vec4<f32>`, `vec2<u32>` or `vec2<f32>`, so
skinned meshes can carry bone indices and weights in eight bytes. Pipeline creation matches the
WGSL type each format is read as against the artifact. Vertex layouts now require four-byte strides
and attribute offsets, which Metal always needed. Vulkan queries vertex-buffer support for the
16-bit formats and answers `Unsupported` without it. The new `mulciber-vertex-formats` probe read
back all six exactly on Linux/NVIDIA under Vulkan validation; Metal maps them but has not run. See
[packed vertex formats](docs/vertex-formats.md).

## Cube textures (graphics 0.13.32)

`Device` gains cube constructors beside every 2D sampled upload: RGBA8 sRGB and UNORM, any
`BlockCompression`, and `RGBA16Float`, each with a single-level and a complete-mip-chain form taking
six square faces in +X, -X, +Y, -Y, +Z, -Z order. They return the existing `Texture`, whose new
`dimension()` reports `TextureDimension::Cube`. Material pipelines sample one through
`MaterialBinding::CubeTexture` and WGSL `texture_cube<f32>`, which `mulciber-shader` records as its
own binding kind; pipeline creation refuses a declaration that disagrees with the artifact, and
submission refuses a 2D texture in a cube slot or a cube in a 2D slot. Vulkan creates a
cube-compatible six-layer image behind a cube view (regenerated bindings add
`VkImageCreateFlagBits`); Metal creates `MTLTextureTypeCube` and replaces each face and level by
slice. The new `mulciber-cube-texture` probe passed 72 face-order, orientation and per-face mip
readback cases in RGBA8 UNORM, RGBA8 sRGB with mips, BC1 with mips and `RGBA16Float` on
Linux/NVIDIA under Vulkan validation; Metal is implemented but has never run. See
[cube textures](docs/cube-textures.md).

## BC1, BC2 and BC3 uploads (graphics 0.13.32)

`BlockCompression` gains `Bc1Srgb`/`Bc1Unorm`, `Bc2Srgb`/`Bc2Unorm` and `Bc3Srgb`/`Bc3Unorm`, the
DXT1, DXT3 and DXT5 encodings older game data ships in, so it can be uploaded as stored instead of
being re-encoded to BC7. BC1 blocks are eight bytes; block sizing and mip validation follow the
format. Vulkan BC1 and BC3 sRGB chains ran under validation on Linux/NVIDIA; BC2, the UNORM
variants and Metal are unexercised. See [block-compressed textures](docs/block-compressed-textures.md).

## Metal culls back faces as Vulkan does (0.13.31)

Vulkan's fixed, material and shadow pipelines have always culled back faces with counter-clockwise
front faces, but the Metal backend never set a cull mode, and Metal's default is to cull nothing.
On macOS every triangle was rasterized from both sides, so a model that supplied both windings of
a surface (a glTF double-sided material expanded by the application) drew a coincident reverse
face that fought the front one in the depth test, and closed meshes paid for binning and, under
alpha-tested materials, shading the half of their triangles facing away. Metal now sets the same
winding and cull mode on every encoder that records scene, material or shadow draws, including
the overlay records after postprocess; the postprocess, bloom and volumetric passes still cull
nothing. Verified by build, clippy and tests only; physical Metal visual validation remains open.

## Pacing onto a refresh the backend reports (graphics 0.13.30, runtime 0.5.6)

Window metrics on Win32, Wayland and X11 report display timing as unknown, so the runtime's frame
pacer never engaged there and every delta was the wall-clock gap between build starts: on an RTX
3060 Ti at 74.97 Hz under KDE Wayland, presents landed 13.34 ms apart while deltas ran from 6 to
24 ms, which is visible judder on a display that never missed a refresh.
`Surface::fixed_refresh_interval` reports the period when `VK_EXT_present_timing` says the
refresh is fixed (`refreshInterval` equal to `refreshDuration`; variable refresh and an
undetermined mode report `None`), and `Runtime::set_fixed_refresh_interval` hands it to the pacer,
which uses it only while platform timing is unknown. NVIDIA's Linux driver (615.71.09) reports the
duration but leaves the refresh mode undetermined, so `Runtime::set_nominal_refresh_interval`
also takes `Surface::refresh_interval` and trusts it once 30 consecutive presents have landed
within 5% of its grid, revoking it at the first that does not. `Surface::active_presentation_mode` now
resolves Adaptive on a Vulkan swapchain that switches between FIFO and immediate to the mode of
its latest present, as Metal already did, so an application can pace exactly while presents are
synchronized. See [frame-pacing controls](docs/frame-pacing-controls.md).

## Material uniforms up to 512 bytes (0.13.29)

`MATERIAL_UNIFORM_SIZE_LIMIT` rises from 256 to 512 bytes. The per-draw uniform stride in both
backends is now derived from the limit rather than written down beside it, so the two cannot
disagree; it stays a multiple of 256, the largest dynamic offset alignment Vulkan permits. A lit
material whose parameters had filled 256 bytes can now take another block of state without
moving it into a storage slot. The per-draw region costs twice as much per record. See the
[material contract](docs/material-contract.md).

## Material slots capped per native table (0.13.28)

A WGSL binding number is its native index, and Metal keeps textures, samplers and buffers in
separate tables, yet material pipelines capped every binding at slot 15, the sampler table's
size. A material with thirteen textures and two samplers failed as soon as a texture landed on
binding 16. Each kind now has its own ceiling: samplers stay at `MATERIAL_SLOT_LIMIT` (15),
textures reach `MATERIAL_TEXTURE_SLOT_LIMIT` (30) and uniform or storage buffers
`MATERIAL_BUFFER_SLOT_LIMIT` (28, clear of the two vertex buffer indices). A pipeline may declare
at most `MATERIAL_TEXTURE_COUNT_LIMIT` (16) textures, Vulkan's guaranteed sampled images per
stage. See the [material contract](docs/material-contract.md).

## Adaptive judges the workload, not the present interval (0.13.27)

Adaptive now chooses synchronized or immediate presentation from Strict's CPU and GPU workload
measurement: three consecutive overloaded frames release sync and 90 frames with 5% headroom
restore it. The 0.13.26 policy released on any single present interval over 1.15 periods, which
an application capped at the refresh rate hits with every hitch, so it alternated between the two
modes about once a second and tore in the immediate stretches. See
[frame-pacing controls](docs/frame-pacing-controls.md).

## Adaptive presentation without relaxed FIFO (0.13.26)

Where a Vulkan driver exposes no FIFO relaxed, as NVIDIA's Linux driver does on every surface,
Adaptive now switches one swapchain between FIFO and immediate per present with
`VK_KHR_swapchain_maintenance1`, driven by the throughput policy Metal already used, which moves
to a shared `backend/adaptive.rs`. Latest-ready becomes the last resort: it alternated one- and
two-refresh frames whenever the workload sat near the refresh period. See
[frame-pacing controls](docs/frame-pacing-controls.md) for measurements and limits.

## Vulkan acquisition and attachment synchronization (0.13.25)

Chain first-use swapchain layout transitions to the acquisition semaphore wait
in every draw path. Synchronize direct-render depth and MSAA attachment reuse
across overlapping frames. Synchronization validation reproduced the original
hazards on Windows / RTX 3060 Ti and no longer reports them in the patched game
menu or the subsequent map/dialogue test. This does not establish resolution of
the RTX 4070 playtest device loss.
See [evidence and limits](docs/swapchain-synchronization.md).

## Strict full-rate recovery (0.13.24)

Start Strict at full refresh and step down only after sustained overload. Recovery
requires five percent headroom instead of twenty percent, permitting 85-90 FPS
workloads to run at 75 Hz. Exclude Vulkan submission and image-availability waits
from the workload measurements used by Strict. See the pacing controls document
for regression coverage and physical-validation limits.

## Queue-ordered float texture updates (0.13.23)

Add `Device::update_rgba16_float_texture` for same-sized, single-level replacements
that retain texture handles and material bindings. Uploads are ordered before the
next scene submission without waiting for device idle, with staging reuse on
Vulkan and retained blit buffers on Metal. Earlier submitted frames retain their
old contents; pending updates coalesce to the last write. The API uses the same
checked binary16 conversion as texture creation.

Linux/Vulkan numerical validation covers repeated updates across in-flight frames.
Metal cross-compiles cleanly; physical Metal validation remains outstanding. See
[the replacement contract](docs/float-texture-uploads.md#queue-ordered-replacement).

## Adaptive presentation on drivers with no relaxed FIFO (0.13.22)

`PresentationMode::Adaptive` now has two native Vulkan spellings and takes whichever the
driver offers. FIFO relaxed is still preferred, so an adapter that already served the
policy is unchanged; where a driver exposes none, the device enables
`VK_KHR_present_mode_fifo_latest_ready` (or its original EXT name) with the
`presentModeFifoLatestReady` feature, and the policy selects that mode instead. NVIDIA's
Linux driver exposes no relaxed FIFO on Wayland, XCB/Xlib or `VK_KHR_display` surfaces, so
Adaptive was unavailable on every surface it offers.

The two are not interchangeable in what the player sees, which is why neither stands in for
the other's absence. Relaxed FIFO lets a late frame through immediately and tears;
latest-ready keeps every present on a vertical blank and discards the images that went
stale waiting for one. `Adaptive` now promises only that a queued image never costs
latency, and an application that cares which it got should ask for the active native mode.
Plain `Synchronized` never takes latest-ready: it promises every rendered frame reaches the
screen. Availability is gated on the device having enabled the feature rather than on the
surface listing the mode, which it does regardless. See
[contract](docs/frame-pacing-controls.md).

## Explicit VSync and frame caps (0.13.20 / runtime 0.5.4)

Add `Surface::set_vsync` with live Metal switching and deferred Vulkan swapchain
reconfiguration. Unsupported immediate presentation is an explicit error. Runtime
caps accept rates independent of display refresh, avoid catch-up bursts, and allow
cadence smoothing to be disabled without changing fixed-step simulation. No
automatic half-rate fallback is introduced. See [contract](docs/frame-pacing-controls.md).

## Live sample count, and Metal region timing measures what a pass adds (0.13.19)

`Device::set_sample_count` changes the samples per pixel that pipelines and targets are
built for after the session is open, with the same observable fallback to one sample as
opening. Every textured, instanced, material and postprocess pipeline and every render or
postprocess target now remembers the count it was built for, and submitting one built for
another count is refused with an error naming it, on both backends, instead of a native
sample-count mismatch. Nothing is rebuilt for the caller: shaders are not retained, so the
application destroys and recreates its own resources. `DeviceSelection::sample_count` keeps
the count chosen at open.


Fix the Metal shadow, scene and postprocess regions overstating light passes. A region was
the span from its earliest stage start to its latest stage end, and on Apple's tile-based
GPUs a pass's vertex stage starts under the previous pass's fragment stage and waits
there, so an almost empty shadow cascade reported the fragment time of the cascade before
it, and the regions of a frame overlapped instead of adding up. A region is now measured
from the previous pass finishing to this pass finishing, carrying the previous frame's
last tick into the first pass, which is what removing the pass would save. Replayed
against an Instruments capture of a 31 ms frame, the old spans summed to 119 ms and the
new regions to 31.7 ms. The vertex and fragment stage intervals are unchanged and documented: the
fragment interval is a pass's own work, the vertex interval includes the wait.

## Two-sample rendering (0.13.18)

`SampleCount::Two` joins `One` and `Four`. Each backend asks the device for the requested
count (`supportsTextureSampleCount:` on Metal, the framebuffer colour and depth sample-count
limits on Vulkan) and falls back observably to one sample per pixel, as the four-sample
request already did. `SampleCount::samples`, `from_samples` and `is_multisampled` replace
the assumption that multisampled means four. Shaders that read scene depth through a
multisampled texture are unchanged: the sample count is a property of the texture, not
the shader.

## Optional Vulkan device labels (0.13.17)

Fix startup without validation: 0.13.16 stopped enabling `VK_EXT_debug_utils` on the
instance but still required its device label functions. Load and call those functions
only with validation enabled. GPU region timestamps remain available in ordinary builds.
Windowless regression tests now create a real device, load its complete function table,
and submit/read timestamp queries through the renderer's region-recording methods in
both modes. The device test reproduced the 0.13.16 failure before this correction.

## Optional Vulkan validation (0.13.16)

Published consumers no longer unconditionally require `VK_LAYER_KHRONOS_validation` or
`VK_EXT_debug_utils`. Default builds request no validation layer, install no debug messenger,
and do not resolve its extension functions. This fixes release startup on machines without
the Vulkan SDK. The opt-in `vulkan-validation` feature preserves strict layer requirements
and failure on warning/error callbacks. Repository examples and probes opt in explicitly;
`native-validation` enables it too. Metal behavior is unchanged.

Windowless native instance tests cover SDK-free creation/destruction, explicit validation
with a messenger, and rejection when requested validation is unavailable.

## Block-compressed sampled uploads (0.13.15)

`Device::create_block_compressed_texture` and its `_with_mips` peer upload already encoded BC7
(sRGB or UNORM) and BC5 blocks through the existing `Texture` and material bindings, sampled
directly by the GPU at a quarter of the RGBA8 footprint. Mulciber never encodes or decodes; the
application encodes each level of its own filtered chain. Vulkan gates on `textureCompressionBC`
and Metal on `supportsBCTextureCompression`, returning `Unsupported` rather than decoding on the
CPU. Isle of Rán uploads its BC7 material textures through this path. See the
[contract](docs/block-compressed-textures.md).

The same release moves `mulciber-shader` to 0.5.2 on naga 30.0.1, whose source is identical to
30.0.0 (only its manifest changed); the cube Vulkan artifact regenerated under 30.0.1 is
byte-identical to the checked-in one, so every artifact hash in `vulkan-toolchain.lock.toml`
stands and only its recorded compiler string moves.

## Growable Vulkan descriptor pools (0.13.14)

A Vulkan pipeline's cached descriptor sets are allocated from as many pools as the scene turns
out to need. Previously each pipeline owned one 64-set pool, so a scene whose distinct sampled
textures through one pipeline outgrew it failed with `VK_ERROR_OUT_OF_POOL_MEMORY` while Metal,
which has no such pool, ran on. Growth is transparent; reset and destruction release every pool.
See the [backend contract note](docs/backend-contracts.md#growable-vulkan-descriptor-pools-01314).

## Sampled floating-point textures (0.13.13)

Native RGBA16Float uploads accept linear f32 RGBA texels, with optional complete authored mip chains.
Existing material bindings support linear filtering and explicit LOD sampling in vertex and fragment
stages. The 40-case numerical readback probe passed on Metal/Apple M2; native Vulkan execution remains
pending. See the [input contract, validation, and consumer migration](docs/float-texture-uploads.md).

## Vulkan recording and opt-in frame-start pacing (0.13.12 / runtime 0.5.3)

Vulkan material/shadow recording avoids redundant pipeline binds and single-draw indirect
commands on Windows and Linux. Optional native refresh feedback supports the runtime's new
opt-in FrameStartLimiter, which waits before input polling; it leaves fixed-step timing
and interpolation unchanged. Isle of Ran enables limiting only on Windows, retaining Linux
FIFO behavior. Windows measurements and playtesting support the combined improvement;
native Linux/macOS performance and AMD coverage remain unmeasured. See
[validation and measurements](docs/windows-validation.md#vulkan-recording-and-opt-in-frame-start-pacing-01312--runtime-053).

## Windows mailbox presentation (0.13.11)

Windows Vulkan prefers supported mailbox presentation with FIFO fallback;
Linux FIFO and Metal behavior are unchanged. Optional presentation timing is
bounded to native queue capacity, and unavailable timestamps remain untimed.
The user confirmed the Windows input delay is gone. See
[Windows validation](docs/windows-validation.md#windows-mailbox-presentation-01311)
for measured results and hardware coverage.

## Metal HDR fixes (0.13.10)

First run of the HDR, scene-depth and volumetric passes on Apple silicon found two Metal-only
faults. The MSAA scene color carried a resolve texture on intermediate encoders whose store
action was a plain store, which Metal rejects; only the last writer resolves now. Render target
creation released an autoreleased texture descriptor, so any target created inside a frame left
a freed object in the frame pool and every error exit segfaulted instead of returning its
message. Isle of Rán renders on Metal with both fixes.

## GPU-local Vulkan meshes (0.13.9)

Immutable meshes now prefer device-local storage with frame-owned staged uploads, bounded retained
staging capacity and an observable host-memory fallback. GPU timing feedback remains ordered across
frame-slot abandonment. See the [mesh-memory policy and evidence](docs/vulkan-mesh-memory.md).
