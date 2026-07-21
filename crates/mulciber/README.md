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

Optional GPU diagnostics correlate completed durations with presentation frame indices. Metal
reports whole-command-buffer time; Vulkan additionally reports the fixed shadow, scene, and
postprocess regions when its graphics queue supports timestamps. Lazy resource drops are reclaimed
in bounded batches at frame boundaries, while explicit destruction and shutdown remain synchronous
and fallible.

The API is experimental and may change without compatibility guarantees. Design contracts,
decision records, runnable examples, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
