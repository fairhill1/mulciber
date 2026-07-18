# Experimental multi-object scene contract

This checkpoint extends the resource-backed graphics slice from one object to a non-empty ordered
sequence of independently transformed objects. `mulciber-scene` and `wgpu-scene` render the same
100-object field: two meshes, two textures, one scene pipeline, depth, capability-selected 4x/1x
MSAA, and one fullscreen postprocess pass.

The focused cube, input, postprocess, and showcase pairs remain unchanged. This pair measures the
first small-game-shaped submission rather than replacing those narrower comparisons.

## Public checkpoint vocabulary

`TexturedSceneDraw` borrows one mesh, texture, and textured pipeline and carries one column-major
model-view-projection matrix. `TexturedScene` submits a non-empty borrowed slice of those records
directly to generation-matched render targets. `PostprocessedScene` submits the same ordered slice
to generation-matched offscreen targets, followed by one postprocess pipeline.

`Queue::draw_textured_scene_and_present` and
`Queue::draw_textured_scene_postprocessed_and_present` validate every resource session, the target
generation, a non-empty draw list, and finite transforms before consuming the acquired frame. The
existing single-object queue methods are compatibility conveniences over a one-element scene, so
the earlier examples retain their source and behavior.

This is ordered multi-draw, not GPU instancing. The application provides 100 records and each
backend emits 100 indexed draws in one scene pass. That deliberately establishes a heterogeneous
baseline—objects may select different meshes, textures, and pipelines—before an instanced path adds
its distinct grouping and instance-data contract.

## Native behavior

Metal keeps one render encoder open for the scene. It stores matrices at 256-byte offsets in a
growable shared buffer and changes pipeline, mesh, texture, and uniform offset for each object. The
buffer can be replaced only after acquisition has established completion of the previous frame.

Vulkan uses one dynamic-rendering scene pass and one host-coherent dynamic uniform buffer. Each
draw binds its texture descriptor plus a checked dynamic offset, then issues the existing indirect
indexed command. Uniform capacity grows geometrically after waiting for the frame slot; descriptor
pools that reference a replaced buffer are rebuilt. The fixed 256-byte stride satisfies Vulkan's
maximum baseline uniform-buffer offset alignment.

Both postprocessed paths retain their existing resolve and producer-to-fragment-consumer behavior.
No command encoder, arbitrary pass graph, material system, sorting, batching, or instance buffer is
claimed by this checkpoint.

## macOS comparison checkpoint

On 2026-07-18, an uncommitted tree based on `a00bb52` ran `mulciber-scene` on the Apple M2 / macOS
15.7.7 machine with `MTL_DEBUG_LAYER=1`. It selected Metal and four samples. The animated field
showed 100 distinct cubes and pyramids using both checkerboards, with depth and the expected final
grade/vignette. A screenshot was visually inspected; no deterministic readback was performed. Metal
printed no validation diagnostic beyond its enabled banner. The process was deliberately stopped
after the visual check rather than closed through a lifecycle pass.

`mulciber-api-conformance` then presented both a direct two-object scene and a postprocessed
two-object scene using replacement resources. All sixteen Metal cases passed under Metal API
Validation with no diagnostic beyond the banner. This asserts ordered multi-draw after resource
reclamation; it is not physical Vulkan evidence.

The equivalent `wgpu-scene` selected wgpu's Metal backend and four samples on the same machine. Its
visually inspected output matched the Mulciber workload and effect. That run did not enable Metal
API Validation and does not establish deterministic pixel equivalence or lifecycle behavior.

Raw Rust application-source counts, excluding manifests, build scripts, and the shared 62-line WGSL
module, are:

| Source | Scene data | Application/GPU plumbing | Total |
| --- | ---: | ---: | ---: |
| `mulciber-scene` (`scene.rs` + `main.rs`) | 105 | 109 | 214 |
| `wgpu-scene` (`scene.rs` + `main.rs` + `gpu.rs`) | 111 | 698 | 809 |

The near-equivalent scene-data columns keep geometry and transform math visible rather than
crediting either API for them. The plumbing comparison includes wgpu's 597-line `gpu.rs`; splitting
that file for readability does not exclude it. These figures measure application ergonomics, not
total implementation or maintenance cost: Mulciber's Metal and Vulkan backend code belongs to the
library and remains part of the viability judgment.

On 2026-07-18, the `mulciber-api-conformance` probe gave the new multi-object paths their first
physical Vulkan exercise at revision `33d779f` on the Windows 11 Home / Intel UHD Graphics 620 tier
(driver 31.0.101.2115, Vulkan device API 1.3.215, loader/validation 1.4.350). Both the direct
`draw_textured_scene_and_present` and postprocessed `draw_textured_scene_postprocessed_and_present`
two-object cases presented after explicit resource reclamation, and all seventeen asserted Vulkan
cases passed twice with exit zero and no validation or loader message. See the
[Win32/Vulkan validation runbook](windows-validation.md) for the recorded evidence. The operator
then ran the interactive `mulciber-scene` example on the same Intel tier and reported the animated
100-object field looked correct, an operator visual report rather than a captured validation run. The
earlier Intel verification at `a00bb52` covers resource lifetime only.
