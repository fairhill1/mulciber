# Shadowed HDR scattering checkpoint

`Device::create_volumetric_hdr_postprocess_pipeline` extends the opt-in HDR path with
application-authored scattering and additive upscaling. It consumes the same
`PostprocessPipelineDescriptor`, bloom prefilter/downsample, and a `VolumetricShaders` bundle.
The existing HDR and surface-format pipeline APIs retain their pass ordering.

## Application contract

- The four volume artifacts use `post_vertex` and `post_fragment`. Provide single-sample and
  multisampled-depth variants of both the scattering and upscale shader. The backend selects the
  pair matching the device's actual scene sample count, including a negotiated 1x fallback.
- All stages share the final composite's exact uniform declaration at binding 0, now bounded to
  512 bytes. Volume binding 1 is the world depth texture at its native sample count. Scattering
  binding 2 is the submitted `texture_depth_2d_array`; upscale binding 2 is the half-resolution
  `RGBA16Float` scattering result. Texture loads need no sampler. Reflection rejects incorrect
  depth kinds, uniform sizes, missing bindings and extra bindings before native pipeline creation.
- Submit material scene content and a cascaded shadow prepass each frame. All shadow-casting
  geometry, alpha cutouts and deformation remain application-owned. The engine supplies the
  resulting shadow texture without interpreting the light or scattering model.
- HDR targets retain sampleable world depth. The scattering image is allocated lazily on first
  volumetric submission, at half scene width and height with a one-texel minimum. Both shaders
  must handle odd, scaled and tiny extents. Target generation and render-scale rules remain intact.
- Ordering is shadow prepass, world, scattering, additive upscale into world color, foreground,
  bloom, final tone-map/composite, then HUD. World depth is read before foreground clears it.
  Upscale RGB adds to HDR color and leaves its alpha unchanged. The application owns extinction,
  shadow visibility, phase, sampling, depth-aware filtering and any future temporal history.
- This pass adds direct scattering to an application's existing fog model. It does not itself
  replace surface fog with a complete participating-medium transport solution.

## Native ownership and synchronization

Vulkan checks sampled D32 image support for the chosen scene sample count and extent. World depth
stores its samples, transitions to fragment sampling for both volume stages, then returns to depth
attachment use before foreground. The half-resolution output transitions from attachment writes to
sampling. Additive upscaling uses the scene sample count and synchronizes both MSAA color and its
resolve destination. Uniform descriptors use the frame slot's independent 512-byte region. Volume
sets are invalidated along with postprocess targets or shadow resources; pipelines and textures
retire through the existing completed-frame rules, including stale surface generations.

Metal uses private tracked HDR depth rather than memoryless depth. Sequential retained render
encoders preserve that depth for reads. MSAA color stores across the world/upscale/foreground
encoders and resolves after the last writer. Resources retire with their parent pipeline/target,
while retained command buffers keep in-flight work alive. Target texture descriptors are released
after creation. Volume work currently falls inside the Vulkan scene timing region; Metal's existing
counter scopes do not separately report the new encoders.

## Validation boundary

Headless validation covers workspace formatting, checks, strict clippy and tests; Vulkan and Metal
backend Rust is cross-checked for Windows and Apple silicon. New tests distinguish single-sample
from multisampled depth reflection, reject volume interface mismatches, and generate MSL for
multisampled depth. The game compiles all four complete shaders to validated Vulkan SPIR-V, verifies
runtime artifact acceptance and reverse-depth camera reconstruction, and tests phase/integration
using Rust generated from the actual WGSL arithmetic. MSL generation of all four game shaders is
checked on Linux; Apple metallib compilation is unavailable there.

No game or GUI was launched. Native validation-layer rendering, visual correctness, resize/retirement
stress, performance and physical Metal execution remain unverified for this addition. Canopy cutouts,
window apertures, silhouettes, sunset transitions and 1x/4x MSAA need visual evaluation on hardware.
