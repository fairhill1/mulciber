# Changelog

Release notes moved from the README. This is a partial history of changes.

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
