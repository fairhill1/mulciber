# Material scene depth checkpoint

`MaterialBinding::SceneDepth { binding }` exposes an immutable copy of world depth to
HDR materials. It builds on the sampleable HDR depth and multisampled shader reflection
introduced for volumetric scattering; those passes retain their order and bindings.

## Application contract

Declare `texture_depth_2d` for `DeviceSelection::sample_count() == SampleCount::One` or
`texture_depth_multisampled_2d` for `Four`. Creation rejects a mismatched texture kind,
non-HDR consumers and depth-writing consumers. The slot is engine-supplied: it does not
add an entry to `MaterialRecord::textures` or consume the shadow slot. It can coexist
with an ordinary shadow map or cascade array. `textureLoad` needs no sampler.

The first world record declaring scene depth creates the pass boundary. All preceding
records finish before the snapshot is copied. That record and every remaining world
record may test depth but may not write it; submission validation rejects violations.
At least one preceding record is required. Foreground, overlay, shadow and direct-output
consumers are rejected. Scenes with no scene-depth binding retain their original recipe.

The snapshot has scene resolution (including render scale), top-left texel origin, native
0..1 projection depth and every original MSAA sample. Clear pixels retain the scene's far
depth, including zero with reversed Z. Reconstruct positions with the same inverse projection
as the scene. Sample selection/reduction, intersection fading and color remain shader policy.
Do not average non-linear depths across silhouettes into fictitious surfaces.

Ordering is shadow, opaque world, depth copy, remaining world, optional volumetric scattering
and upscale, foreground with fresh depth, bloom, final composite, HUD. Volumetrics still read
the live world depth, which remaining world records cannot modify. Snapshot consumers retain
normal fixed-function depth testing against that live attachment.

## Native storage and lifetime

Both backends allocate a matching snapshot lazily per HDR target, only when first used.
It costs four bytes per scene pixel per sample (16 bytes at 4x), plus one full depth copy
per consuming frame. It is recreated with target extent/render scale/generation and released
with its parent. This checkpoint deliberately copies all samples; it does not add a resolve
shader, reduce MSAA to 1x, or redraw opaque geometry.

Vulkan adds transfer-source usage to HDR depth and validates transfer-capable sampled D32
at the selected sample count and extent. `vkCmdCopyImage2` copies depth after attachment stores;
explicit barriers order preceding depth writes, transfer access, shader sampling and subsequent
attachment reuse. Snapshot reuse waits for prior frame reads on the same ordered queue. Color
and MSAA resolve destinations receive write-to-load dependencies between scopes. Material
descriptor cache keys include target identity; destruction and stale-generation reclamation
invalidate those descriptors through the existing completed-frame resource rules.

Metal uses the private tracked HDR depth already required by volumetrics. The opaque encoder
stores color and depth, a blit copies into a separate private texture, and the remaining world
encoder loads the original attachments and samples the copy. That avoids sampling an attachment
while Metal can flush it from tile memory. Color resolves after the last world/volume/foreground
writer, and retained command buffers preserve in-flight resources. The added encoder carries no
counter attachments: existing Metal scene timing describes the opaque scope only, not the copy
and depth-reading scope; Vulkan includes both inside its scene region.

## Validation boundary

Headless checks cover the workspace build, strict clippy and tests, including record ordering,
foreground isolation and single-/multisampled depth binding reflection. Backend Rust is checked
for Linux, Windows and Apple silicon. The consumer compiles 1x/4x water WGSL to Vulkan artifacts,
checks artifact acceptance and executes the actual shader fade on the CPU, including projection
reconstruction at shallow viewing angles. Generated MSL can be inspected on Linux; native Metal
compilation requires Apple's tools.

No game, window or GUI is launched for this checkpoint. Native validation-layer execution,
MSAA edge appearance, performance, resize/retirement stress and physical Metal rendering remain
unverified. Hardware validation should exercise 1x and 4x, scaled and tiny extents, alternate target
identities, resize/recreation, optional volumetrics, foreground depth clears and final HUD ordering.
