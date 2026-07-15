# Architecture decisions

## Native probes precede the abstraction

The first implementations are intentionally unshared probes for Metal/AppKit, Vulkan/Win32,
Vulkan/Wayland, and Vulkan/X11. Zinc's public types will be extracted from working call sequences,
not copied from Vulkan, Metal, WebGPU, or another portability library.

## Project boundaries

- `zinc-platform` will own native windows, events, input, monitors, and lifecycle.
- `zinc-gpu` will own devices, queues, resources, synchronization, pipelines, and presentation.
- `zinc-shader` will eventually own offline compilation, reflection, and binding validation.
- `zinc-runtime` will eventually own timing, the game loop, jobs, and platform/GPU coordination.

Only the first two library shells exist today. Their APIs remain empty until the probes establish
the necessary contracts.

## Dependency policy

Zinc does not depend on `wgpu` or `winit`. Thin ABI bindings, generated bindings, shader compiler
components, validation layers, and operating-system libraries are acceptable dependencies when they
do not impose resource, synchronization, event-loop, or lifecycle policy.

The initial Metal capability probe uses the Objective-C runtime directly. Vulkan bindings should be
generated from Khronos `vk.xml`; constants and layouts must not be transcribed manually.

Foundational crates default to zero third-party runtime dependencies. A dependency is accepted only
when it removes substantial correctness or maintenance risk, remains below Zinc's policy layer, and
has a small auditable transitive tree. ABI bindings and file-format parsers are better candidates than
frameworks, executors, global logging systems, or general-purpose utility collections.

Build tools may use a broader dependency set because they do not ship in the game process. Generated
runtime code must remain inspectable and reproducible from pinned inputs. Dependency additions record
their purpose, alternatives, transitive packages, and removal boundary in an architecture decision.

The Vulkan probe applies this boundary directly. Its checked-in Rust ABI is generated from pinned
Khronos headers and has no package dependency. `tools/vulkan-bindgen` is a separate Cargo workspace;
its libclang/bindgen graph is used only when regenerating that file and cannot enter Zinc's runtime
lockfile or binary.

## Safety boundary

Native backends are necessarily unsafe. Unsafe operations stay inside backend modules and expose
validated owned handles to the rest of Zinc. Debug builds use the strongest available API validation,
object labels, and lifetime diagnostics. Release builds retain cheap structural validation at public
API boundaries.

## Feature model

Optional GPU advancements are capability flags, not a single `low/medium/high` tier. This avoids
assuming that ray tracing, mesh shading, sparse resources, descriptor indexing, and presentation
features always advance together.

## Shader toolchain

Native probe shaders use each backend's native input—MSL for Metal and SPIR-V for Vulkan—to create a
ground-truth implementation. This does not select Zinc's eventual authoring language.

Before choosing an authoring language or compiler, Zinc will compile representative raster, compute,
bindless, mesh, and ray-tracing shaders through the leading candidates. The selected offline pipeline
must produce SPIR-V and MSL/metallib, expose deterministic reflection, preserve debuggable source
locations, support backend-specific escape hatches, and add no compiler dependency to the shipped
game runtime.

WGSL with Naga and Slang are candidates, not commitments. WGSL brings WebGPU semantics and familiarity
for Rust graphics developers. Slang targets a broader native feature model, but its Metal feature
coverage must be verified against Zinc's required advanced paths. Maintaining separate production MSL
and Vulkan shader sources is a fallback, not the desired workflow.
