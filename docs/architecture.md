# Architecture decisions

## Native probes precede the abstraction

The first implementations are intentionally unshared probes for Metal/AppKit, Vulkan/Win32,
Vulkan/Wayland, and Vulkan/X11. Mulciber's public types will be extracted from working call sequences,
not copied from Vulkan, Metal, WebGPU, or another portability library.

The probes now constrain a narrow experimental slice. Extraction follows the
[API extraction and comparison plan](api-extraction-plan.md): write the desired application flow first,
implement it through both native backends, preserve unresolved differences, and compare the result
before granting stable names or support promises. Remaining Gate 1 physical coverage continues in
parallel and is not implied by the existence of public Rust items.

## Project boundaries

- `mulciber-platform` will own native windows, events, input, monitors, and lifecycle.
- `mulciber` will own devices, queues, resources, synchronization, pipelines, and presentation.
- `mulciber-shader` will eventually own offline compilation, reflection, and binding validation.
- `mulciber-runtime` will eventually own timing, the game loop, jobs, and platform/GPU coordination.

The first extraction now gives `mulciber-platform` experimental peer AppKit, Win32, Wayland, and X11
application, window, event, drawable-metrics, and borrowed-target implementations consumed by the
Metal and Vulkan probes; their concrete decisions are recorded in the
[platform contract](api-platform-contract.md). The extracted Win32 path has passed its automated and
physical validation, and the first graphics lifecycle vocabulary is recorded in the
[graphics contract](api-graphics-contract.md). Extraction does not create a stable API by default.

## Unified contract and backend selection

Ordinary game code uses one platform and graphics contract. Metal/AppKit and Vulkan with Win32,
Wayland, or X11 remain explicitly named backend modules internally, with native ownership and lifecycle
state machines rather than a forced identical implementation. Shared types express game intent and
observable outcomes; they do not claim that a Metal drawable and Vulkan swapchain image have identical
fences, release behavior, or failure modes.

The supported operating-system target determines the applicable graphics backend in the initial
contract: Metal on Apple-silicon macOS and Vulkan on Windows or Linux. A single-backend build must not
link or initialize the unused backend, and ordinary frame work must not pay for portability-only
dispatch. Safe bounded native extensions remain possible when a capability cannot fit the shared
contract without loss, but they cannot bypass Mulciber's ownership or presentation-retirement tracking.

## Dependency policy

Mulciber does not depend on `wgpu` or `winit`. Thin ABI bindings, generated bindings, shader compiler
components, validation layers, and operating-system libraries are acceptable dependencies when they
do not impose resource, synchronization, event-loop, or lifecycle policy.

The initial Metal capability probe uses the Objective-C runtime directly. Vulkan bindings should be
generated from Khronos `vk.xml`; constants and layouts must not be transcribed manually.

Foundational crates default to zero third-party runtime dependencies. A dependency is accepted only
when it removes substantial correctness or maintenance risk, remains below Mulciber's policy layer, and
has a small auditable transitive tree. ABI bindings and file-format parsers are better candidates than
frameworks, executors, global logging systems, or general-purpose utility collections.

Build tools may use a broader dependency set because they do not ship in the game process. Generated
runtime code must remain inspectable and reproducible from pinned inputs. Dependency additions record
their purpose, alternatives, transitive packages, and removal boundary in an architecture decision.

The Vulkan probe applies this boundary directly. Its checked-in Rust ABI is generated from pinned
Khronos headers; the Linux triangle consumes only the local `mulciber-platform` crate. No third-party
Rust package enters the probe. `tools/vulkan-bindgen` is a separate Cargo workspace; its
libclang/bindgen graph is used only when regenerating that file and cannot enter Mulciber's runtime
lockfile or binary.

## Safety boundary

Native backends are necessarily unsafe. Unsafe operations stay inside backend modules and expose
validated owned handles to the rest of Mulciber. Debug builds use the strongest available API validation,
object labels, and lifetime diagnostics. Release builds retain cheap structural validation at public
API boundaries.

## Feature model

Optional GPU advancements are capability flags, not a single `low/medium/high` tier. This avoids
assuming that ray tracing, mesh shading, sparse resources, descriptor indexing, and presentation
features always advance together.

## Shader toolchain

Native probe shaders use each backend's native input—MSL for Metal and SPIR-V for Vulkan—to create a
ground-truth implementation. This does not select Mulciber's eventual authoring language.

Before choosing an authoring language or compiler, Mulciber will compile representative raster, compute,
bindless, mesh, and ray-tracing shaders through the leading candidates. The selected offline pipeline
must produce SPIR-V and MSL/metallib, expose deterministic reflection, preserve debuggable source
locations, support backend-specific escape hatches, and add no compiler dependency to the shipped
game runtime.

WGSL with Naga and Slang are candidates, not commitments. WGSL brings WebGPU semantics and familiarity
for Rust graphics developers. Slang targets a broader native feature model, but its Metal feature
coverage must be verified against Mulciber's required advanced paths. Maintaining separate production MSL
and Vulkan shader sources is a fallback, not the desired workflow.
