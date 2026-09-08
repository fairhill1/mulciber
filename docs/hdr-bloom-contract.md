# HDR scene and bloom checkpoint

Isle of Rán needs the sun, water highlights and emissive coals to retain radiance above one until
bloom and final tone mapping. This opt-in material path renders scene and foreground records into
linear `RGBA16Float`, then runs six downsampling passes and a surface-format composite. Existing
surface-format material pipelines and postprocessing keep their original behavior.

## Application contract

- `Device::create_hdr_material_pipeline` takes the existing material descriptor. Its output must
  target HDR scene storage; it cannot be used for a direct surface pass or a HUD overlay.
- `Device::create_scaled_hdr_postprocess_targets` creates generation-bound scene color, scene depth,
  optional MSAA color, and six single-sample HDR textures. Each bloom level halves the preceding
  width and height, with a one-texel minimum. Recreate the bundle on resize or render-scale changes.
- `Device::create_hdr_postprocess_pipeline` takes the composite descriptor and two application
  shader artifacts: prefilter and downsample. All use `post_vertex` and `post_fragment`. Filters
  declare only texture binding 1 and sampler binding 2. Composite binding 1 is the resolved HDR
  scene, binding 2 is its linear clamp sampler, and bindings 3 through 8 are the six bloom levels.
  The existing optional, exact-size uniform at binding 0 belongs only to the composite.
- `SceneOutput::Postprocessed` submits the path. The HDR pipeline and targets must agree. Scene and
  foreground materials must also agree with the target format. HUD overlays use surface-format,
  depth-off material pipelines and execute after the composite at native resolution.
- The application owns thresholds, filtering, exposure, bloom strength, and tone mapping. The
  engine owns allocation, pass ordering, dependencies and retirement. No shader compiler is added
  to the runtime. HDR scene storage does not imply HDR monitor output: presentation remains sRGB.

## Independently optional effects

`Device::create_hdr_composite_pipeline` accepts optional `BloomShaders` and `VolumetricShaders`.
Applications can create all four combinations ahead of time and select one per frame. A disabled
stage has no child pipelines, descriptors or render passes. The final tone-map and underwater
composite still runs against the same HDR scene targets. Target-owned bloom storage remains
allocated for instant re-enabling; optional stages control GPU work, not target allocation.

Without bloom the final shader must omit bindings 3 through 8. Validation rejects a composite
that would sample unwritten bloom levels. With bloom the complete six-texture contract remains
required. Existing constructors delegate to this path with their previous effect sets intact.
Headless interface tests and the consuming game's pipeline-selection/uniform tests cover this
addition; new native visual or performance evidence is not claimed.

## Native implementation

Vulkan checks RGBA16Float attachment, blending and linear-filter support, plus image-format-specific
sample counts and extent limits. Unsupported combinations return an error; there is no silent
sample-count or color-format fallback. Material pipelines declare the HDR attachment format at
creation. Each bloom render scope transitions its output from discard to color attachment, then to
shader read. Reuse waits for preceding frame reads and writes, including final-composite reads.
Scene, foreground and bloom retain their ordinary sample counts (bloom/composite are always 1x).
All six filter descriptor sets and the composite set share the parent's pool/reset lifetime.
The postprocess GPU timing scope includes the bloom work.

Metal uses RGBA16Float material pipelines, private tracked scene and bloom textures, and sequential
render encoders in the same retained command buffer. Its selected scene sample count is preserved.
The scene and foreground resolve before the bloom chain; the final composite and overlay use the
sRGB drawable. Retained command buffers preserve in-flight resources through owner release.

Both backends retire every bloom texture with its target bundle, including stale surface
generations. Filter pipelines retire with their composite. Partial bloom-chain creation releases
already-created resources. The six bloom levels use approximately one third of a scene image's
pixel count, excluding the one-texel minimum at very small extents.

## Validation boundary

This is a headless checkpoint. CPU tests cover format mismatch rejection, complete filter/composite
binding recipes, and ordinary, scaled, portrait, odd, and minimum bloom extents. The consuming game
compiles WGSL to validated Vulkan artifacts and tests its tone-map and prefilter arithmetic through
Rust generated from the exact same WGSL by `mulciber-shader::compile_host_field`.

No new native presentation, validation-layer rendering, image readback, visual, resize-storm,
performance, or physical Metal execution evidence is claimed. Exercise those on hardware before
promoting this checkpoint to an established cross-backend capability.

## Optional shadowed scattering

The [volumetric extension](volumetric-contract.md) inserts scattering and additive upscaling
before foreground and bloom. HDR world depth is now sampleable at the scene sample count, and
the shared postprocess uniform limit is 512 bytes. The additional half-resolution image is
allocated only when a volumetric pipeline submits to the targets.
