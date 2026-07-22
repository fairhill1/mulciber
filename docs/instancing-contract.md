# Experimental GPU instancing contract

This checkpoint adds native instance-rate drawing without replacing the heterogeneous multi-draw
baseline. `mulciber-instanced-scene` and `wgpu-instanced-scene` render the same 100-object field as
four homogeneous batches: cube/amber, cube/violet, pyramid/amber, and pyramid/violet. Both retain
depth, capability-selected 4x/1x MSAA, resolved scene color, and the fullscreen postprocess pass.

The existing `mulciber-scene` / `wgpu-scene` pair remains the 100-draw comparison. Keeping both
pairs makes the cost and ergonomics of grouping visible instead of silently replacing the simpler
submission model.

## Public checkpoint vocabulary

`InstancedTexturedPipeline` is distinct from `TexturedPipeline` because its vertex-input contract
requires four instance-rate matrix columns at shader locations 3 through 6. A
`TexturedInstanceBatch` borrows one mesh, texture, instanced pipeline, and a non-empty slice of
finite column-major model-view-projection matrices.

The public entry point is `Queue::render_and_present(frame, SceneSubmission { ... })`.
`SceneContent` independently selects heterogeneous textured records or homogeneous instance
batches; `SceneOutput` independently selects direct or postprocessed output. Splitting those proven
axes prevents method or variant names from growing as their cross-product while deliberately
stopping short of a general command encoder or frame graph. The older focused queue methods remain
available so the earlier comparison sources and recorded counts do not churn.

Validation rejects empty scenes, empty batches, non-finite matrices, mixed-session resources,
stale generation-bound targets, and native count/size overflow before encoding where practical.
Pipeline creation and explicit destruction remain fallible, and dropping the new pipeline kind
uses the existing deferred generational reclamation path.

The later [shared-vertex mesh-parts checkpoint](mesh-parts-contract.md) composes with the
application-authored instance-layout material and shadow records: one CPU-grouped instance byte
slice selects one borrowed part and issues one native instanced indexed draw. The older
`TexturedInstanceBatch` compatibility recipe continues to select parent part zero.

## Native behavior

Each backend packs all matrices for the frame contiguously into one growable 64-byte-stride instance
buffer and retains the four application batch boundaries. Geometry remains in the existing vertex
and index buffers. The shader receives geometry at locations 0 through 2 and the four matrix columns
at locations 3 through 6.

Metal configures buffer index 2 with `MTLVertexStepFunctionPerInstance` and issues one native indexed
instanced draw per batch. The shared instance buffer is replaced only after frame acquisition has
established completion of the previous use. Vulkan configures a second vertex binding with
`VK_VERTEX_INPUT_RATE_INSTANCE`, binds each batch's byte offset, and issues one
`vkCmdDrawIndexed` with the batch instance count. Its host-coherent instance buffer grows only after
waiting for the one in-flight frame fence. Both implementations reuse the established direct and
postprocessed attachment/lifecycle paths.

This checkpoint does not add automatic grouping, sorting, indirect multi-draw, GPU-written instance
data, per-instance materials, arbitrary vertex layouts, multiple frames in flight, bindless
resources, or a general render-pass API.

## macOS comparison checkpoint

On 2026-07-18, an uncommitted tree based on `15e6aa2` ran `mulciber-instanced-scene` on the Apple M2 /
macOS 15.7.7 machine with `MTL_DEBUG_LAYER=1`. It selected Metal and four samples. A visually
inspected screenshot showed 100 animated cubes and pyramids using both checkerboards with depth and
the expected final grade/vignette. Metal emitted no diagnostic beyond the validation-enabled banner.
The process was deliberately interrupted after the visual check, so this is not resize, close,
minimize, or broader lifecycle evidence.

The corrected `mulciber-api-conformance` probe then presented direct and postprocessed two-instance
scenes, explicitly destroyed the new pipeline resource kind, and passed all eighteen asserted cases
under Metal API Validation with no diagnostic beyond the banner. The equivalent
`wgpu-instanced-scene` selected wgpu's Metal backend and four samples; its visually inspected output
matched the workload and effect. That wgpu run did not enable Metal API Validation. Neither visual
run provides deterministic pixel equivalence.

Raw Rust application-source counts, excluding manifests, build scripts, and the shared 67-line WGSL
module, are:

| Source | Scene data | Application/GPU plumbing | Total |
| --- | ---: | ---: | ---: |
| `mulciber-instanced-scene` (`scene.rs` + `main.rs`) | 107 | 126 | 233 |
| `wgpu-instanced-scene` (`scene.rs` + `main.rs` + `gpu.rs`) | 114 | 684 | 798 |

The comparison measures application ergonomics, not total implementation or maintenance cost.
Mulciber's native Metal and Vulkan implementation remains library code and part of the wider
viability judgment.

## Windows comparison checkpoint

On 2026-07-18, a clean tree at revision `7f812a4` physically validated the native instance-rate path
under Vulkan on the Windows 11 Home build 22000 / Intel UHD Graphics 620 tier, driver 31.0.101.2115,
Vulkan device API 1.3.215, loader/validation 1.4.350. The `mulciber-api-conformance` probe, whose
Vulkan backend force-enables `VK_LAYER_KHRONOS_validation` and fails shutdown on any recorded
message, asserted all nineteen cases with exit zero and empty standard error across two identical
runs, including the direct and postprocessed instanced presentations. The `mulciber-instanced-scene`
example then selected Vulkan with four samples and the operator visually confirmed the correct
animated 100-object field. This is automated single-display conformance evidence plus an operator
visual report; interactive lifecycle and other driver tiers remain outstanding. See the
[Win32/Vulkan validation runbook](windows-validation.md).
