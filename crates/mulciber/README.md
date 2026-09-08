# Mulciber

`mulciber` is the experimental graphics and presentation layer of the Mulciber native
game-development stack: one narrow public API implemented directly against Metal on macOS and
Vulkan on Windows and Linux, selected at compile time with no portability layer in between.
The project is validating native resource, rendering, presentation, and lifecycle
implementations before it commits to a stable public graphics API.

The current slice owns device and surface lifecycle with tracked presentation retirement, and
renders one fixed frame shape: an optional depth-only shadow pre-pass (a single map or a
cascaded layered array), a multisampled scene pass of ordered draws, and an optional fullscreen
postprocess pass whose offscreen targets accept a render scale. Applications author their own
materials — WGSL modules compiled offline by `mulciber-shader`, with declared vertex layouts
and binding slots validated against the interface recorded in the artifact — and supply
per-record uniform, read-only storage, and frame-transient geometry as plain bytes: the
application owns the layouts, the engine sees bytes. Fullscreen postprocess pipelines likewise
declare an optional group-0/binding-0 uniform of at most 256 bytes and borrow exactly that many
bytes per submission, independently from scene/material uniform storage. Policy that engines
commonly absorb
(cascade fitting and selection, depth bias, mip content, draw ordering) deliberately stays in
application code. Sampled RGBA8 uploads support both sRGB color data and linear UNORM data such as
normal maps, with either one level or a complete application-authored mip chain.
Immutable meshes may keep one vertex region with multiple mixed-width indexed parts; material and
shadow records borrow a selected part without creating another resource lease or allocation, while
the existing mesh APIs remain the one-part/default-part path.

On Vulkan a frame is recorded while earlier frames are still executing: command buffers, fences,
semaphores, GPU timestamp blocks, and every host-visible per-frame region belong to one of three
frame slots, so building a frame no longer costs the previous frame's GPU time as well as its own.
Metal also uses three slots, matched to its drawable pool. Each owns a retained command buffer
and separate CPU-written uniform, storage, transient-geometry, transform, and record-instance
buffers. Acquisition waits only before reusing a slot; unavailable and abandoned acquisitions do
not rotate the ring. Completed GPU timings remain correlated and ordered by submission. The depth
is not an application-visible latency policy.

Optional GPU diagnostics correlate completed durations with presentation frame indices. Metal
reports whole-command-buffer time and, on devices supporting stage-boundary timestamp counters,
shadow, scene, and postprocess spans with separate vertex/fragment intervals. Vulkan reports
the fixed regions when its graphics queue supports timestamps. Stage intervals can overlap and
region spans can include pipeline gaps; neither is additive GPU utilization. Lazy resource drops are reclaimed
in bounded batches at frame boundaries. Vulkan drains every frame in flight before reclamation;
Metal releases its owner references while retained command buffers preserve submitted resource
uses. Explicit destruction is fallible; shutdown waits and checks every outstanding submission.

The API is experimental and may change without compatibility guarantees. Design contracts,
decision records, runnable examples, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).

## Depth-isolated first-person geometry

`SceneContent::MaterialWithForeground { records, foreground_start }` draws the world
records before the split, preserves color, clears depth, and draws the foreground
records before postprocessing and the HUD overlay. Both groups must be non-empty
and use the same depth comparison direction. This recipe requires postprocessed
output and retains material lighting, shadow sampling, scene resolution and MSAA.

Material and shadow records expose their per-record instance upload cap through
`mulciber::INSTANCE_SUPPLY_SIZE_LIMIT`. Split larger supplies on whole-instance
boundaries and submit multiple records using the same mesh and pipeline.
