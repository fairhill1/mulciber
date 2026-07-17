# API extraction and comparison plan

Mulciber now has enough native rendering and lifecycle evidence to begin an experimental API
extraction. This document defines what that means, which decisions the first slice must resolve,
and how the result will be compared with credible alternatives before it is treated as a coherent
public contract.

Experimental extraction is not a stability or support claim. Gate 1's remaining physical coverage
continues in parallel, and Gate 2 must pass before the extracted API is presented as a supported
game-facing slice. See the [viability gates](viability-gates.md) and the native evidence ledger in
[backend contracts](backend-contracts.md).

The first implementation record is the
[experimental platform and window contract](api-platform-contract.md). It extracts peer AppKit,
Win32, Wayland, and X11 application/window lifecycles into `mulciber-platform`; the full Metal and
Vulkan probes are executable consumers while Win32 physical validation and graphics extraction
continue.

The complete application-facing ownership and frame flow is sketched separately in the
[first graphics slice](api-first-slice.md). Its graphics names are intentionally non-compiling
placeholders; the object relationships and observable lifecycle outcomes are the review target.

## Objective

Extract one unified game-facing path through `mulciber-platform` and `mulciber` that can:

1. create a native window;
2. request required and optional GPU capabilities;
3. create a device and presentation surface;
4. upload a mesh and texture;
5. render the mesh with depth;
6. respond correctly to resize, temporary surface unavailability, and close; and
7. drain work and shut down cleanly.

The same application code must use Metal/AppKit on macOS and Vulkan with Win32, Wayland, or X11 on
their supported systems. Backend-specific implementations remain free to use different ownership,
synchronization, presentation, and event-loop machinery. Those differences must not force ordinary
application code to branch, but neither may the shared API promise identical native mechanisms.

This slice is deliberately smaller than the representative probes. Compute-written indirect work,
shadows, post-processing, pipeline caches, and native capability escape hatches remain evidence and
conformance cases; they do not all need to appear in the first example.

## Decisions the first slice must make

These decisions affect the safe public boundary and must be written down before or as the relevant
code lands. Candidate names and type shapes remain provisional until both backends implement them.

| Decision | The first slice must establish |
| --- | --- |
| Application and event-loop ownership | Which object must be created on the main thread, how the game receives events and requests redraw, and which operations are allowed from other threads. |
| Object topology | The ownership and creation order among application, window, surface, adapter, device, and queue, including Vulkan surface-compatible adapter selection without distorting Metal initialization. |
| Surface generations | How resize and display changes invalidate extent-dependent application resources without exposing Win32, AppKit, Wayland, or X11 messages. |
| Frame lifecycle | A scoped acquisition result for ready, temporarily unavailable, outdated/reconfigured, and fatal states, plus explicit presentation and safe non-presentation behavior. Metal drawable release and Vulkan acquired-image recovery may use different backend machinery. |
| Resource use and synchronization | The smallest game-intent vocabulary that derives the demonstrated Vulkan dependencies, remains meaningful on Metal, makes correct synchronization the ordinary path, and still permits advanced native paths. |
| Capabilities and fallbacks | How required capabilities reject a device, how optional capabilities choose a fallback, and how the selected path remains observable to application diagnostics. |
| Errors and recovery | Which structured errors mean retry later, rebuild surface resources, recreate the device, choose a fallback, or terminate. Drop remains best-effort; explicit shutdown remains fallible. |
| Native reach | A checked boundary for backend-specific capabilities or interoperation that cannot invalidate Mulciber-owned resource and presentation tracking. |
| Backend selection and cost | Supported targets build only their applicable native implementation. A game using one backend must not link or initialize another backend or pay for portability-only dispatch in its ordinary frame path. |
| Shader inputs | The first slice may accept checked-in native shader artifacts. It must not accidentally select the eventual authoring language before reflection and advanced-feature evidence settle that decision. |

The broader questions in `docs/backend-contracts.md` remain the source ledger. A decision that is not
needed by this slice stays open rather than receiving a speculative general solution.

## Extraction sequence

### 1. Write the application from the outside in

Create a non-compiling design example showing the complete desired application flow. Keep all
backend names out of the ordinary path. Annotate where capability choice, resize invalidation,
temporary suspension, presentation, and fallible shutdown are observable.

Review the example against both native call sequences before creating public types. If a type exists
only because Vulkan or Metal has one, restate it in game-facing terms or keep it behind the backend.

### 2. Establish the platform and surface spine

Extract only the window/event-loop ownership required to create a surface, deliver resize and close,
represent temporary unavailability, and shut down. Implement peer Win32, AppKit, Wayland, and X11
modules behind the same application-facing contract. Symmetric source organization does not require
identical native behavior.

### 3. Establish the resource and command spine

Extract device selection, owned buffers and textures, uploads, a graphics pipeline, a depth target,
command encoding, submission, and presentation. Use explicit application intent and backend-owned
hazard translation; do not expose raw Vulkan barriers as the portable path or assume Metal can erase
all synchronization decisions.

### 4. Add conformance and failure tests

Run the same baseline and optional-capability cases through Metal and Vulkan. Include invalid resource
usage, unsupported required capability, optional 4x-to-1x multisampling, surface suspension/recreation,
frame non-presentation, and fallible shutdown as soon as their contracts exist. A backend-specific
test may prove different machinery while asserting the same game-facing outcome.

### 5. Run the comparison suite

Implement the applicable tasks below against pinned current releases of each comparison target. Keep
the source, build instructions, environment, validation output, and raw measurements. Record a written
Gate 2 decision even if the result is to redesign or stop.

### 6. Stabilize only what survived comparison

Remove accidental generality, document the ownership and frame-flow mental model, and retain unstable
markers for unresolved areas. Stable naming or compatibility promises wait until Gates 1 and 2 pass;
advanced feature differentiation remains Gate 4.

## Comparison targets

No single comparison answers whether Mulciber is worthwhile. Each target has a declared role so an
unfavorable result cannot be dismissed after the fact as an unfair comparison.

| Target | Role in the evaluation | Applicable questions |
| --- | --- | --- |
| Existing direct native probes | Ground-truth Metal/AppKit and Vulkan/native-platform behavior. | What ownership, synchronization, lifecycle, control, and performance cost did extraction add or remove? |
| Practical single-backend Rust stack | Idiomatic Rust bindings and ordinary window integration, pinned before implementation; initially `ash` plus a window stack for Vulkan and a maintained Metal/AppKit binding stack on macOS. | Would a Metal-only or Vulkan-only game choose Mulciber when portability receives no credit? |
| `wgpu` plus `winit` | Established safe portable Rust graphics and windowing baseline. | Is Mulciber's ordinary path comparably coherent and learnable, and does native reach buy something material? |
| SDL3 GPU and SDL3 platform APIs | Integrated modern GPU, window, input, and lifecycle baseline. | Does Mulciber's Rust ownership, capability model, validation, and native-feature reach justify its complexity? |
| Vulkano plus a window stack | Safe Vulkan-only resource and synchronization baseline. | Is Mulciber competitive for a Vulkan-only user while adding a stronger coordinated platform contract? |
| raylib through a pinned maintained Rust binding, or its canonical C API when binding quality would dominate the result | Convenience and cold-start reference, not a native feature or low-level performance peer. | Is Mulciber's simplest path unnecessarily difficult for the control it exposes? |

Full engines such as Bevy, Godot, or Unity are not Gate 2 graphics/platform peers because they own
substantially more game architecture than Mulciber intends to own. They may become relevant to the
Gate 5 dogfood evaluation, but they are not required for the first comparison suite. Additional thin
graphics libraries are added only when they represent a materially different design point; a long
competitor list is not a substitute for executing the core comparisons well.

At execution time, record the exact release, source revision, features, dependencies, shader tools,
and platform integration used for every target. The reasonable best-practice path is required; do
not intentionally choose raw FFI or an obsolete example to make Mulciber look simpler.

## Comparison tasks

The tasks build on one another but are scored separately:

1. **Clear:** create a window, clear it to a chosen color, remain VSync-paced, and close cleanly.
2. **Representative draw:** upload a mesh and texture, create a depth target and graphics pipeline,
   and draw a proportion-correct animated scene.
3. **Lifecycle:** handle continuous resize, minimize/suspension and restore, display change where
   hardware permits, and titlebar close without invalid ownership or stale rendering.
4. **Optional fallback:** request 4x multisampling as optional, visibly report the selection, and run
   the 1x fallback when forced or required by the device.
5. **Failure diagnosis:** submit one intentionally invalid resource or synchronization request and
   judge whether the resulting compile-time or runtime diagnostic identifies the violated contract
   and likely correction.
6. **Native differentiation:** implement the Gate 4 feature and its fallback. If a comparison target
   cannot express it, record that limitation rather than silently substituting a different feature.
7. **Integrated runtime:** later extend the same slice with input snapshots, fixed and variable
   updates, frame pacing, fullscreen/display changes, suspension, and device recovery for Gate 5.

raylib participates directly in Clear and Lifecycle and may render a visually equivalent scene for
the usability comparison. It is not scored as though its higher-level model exposed the same resource,
synchronization, or recent native-feature controls.

## Measurement protocol

### Hard correctness requirements

- Render the intended output and pass the same deterministic readback checks where the API permits.
- Complete the applicable physical lifecycle actions.
- Produce no native validation warning or error.
- Use no `unsafe` in ordinary Mulciber application code.
- Preserve explicit access to chosen capabilities and fallbacks.
- Shut down without leaking application-owned work or abandoning required cleanup.

A result that fails correctness is not rescued by lower line count or faster timing.

### Ergonomics and learnability

Record:

- time to the first correct run for an unfamiliar human and current coding models using only the
  checked-out materials allowed by Gate 3;
- application lines and declarations, reported with generated code and shader source separated;
- the number of application-visible concepts needed to explain ownership and frame flow;
- application-owned synchronization, resize, presentation, and shutdown bookkeeping;
- compiler and runtime diagnostic quality; and
- documentation or backend-source lookups required to finish each task.

Lines of code are evidence, not the score by themselves. Hidden framework policy and verbose but
necessary capability declarations must be discussed rather than flattened into one number.

### Control, cost, and performance

Record on the same machine and workload:

- CPU frame and command-encoding time distributions, GPU scope timings where comparable, frame
  cadence, and resize behavior;
- process memory, application-controlled allocations, cold pipeline behavior, executable size,
  dependency tree, and clean build time;
- native commands or captures needed to explain material GPU differences;
- capabilities or diagnostics inaccessible without an escape hatch; and
- source and binary artifacts included when only one backend is selected.

The workload, frame count, warmup, validation configuration, display mode, power state, compiler
profile, shaders, assets, and statistical summary must be fixed before examining comparative results.
Workload-specific tolerances are written into the benchmark record before the final runs. GPU timing
from different timestamp domains is not treated as directly comparable without calibration.

### Single-backend scoring

Evaluate Metal and Vulkan separately. For this score:

- award no benefit for sharing source with another backend;
- build and inspect only the selected target and confirm the unused backend is not linked or
  initialized;
- compare against the reasonable direct/native Rust stack for that backend;
- require a material reduction in unsafe application code or lifecycle/synchronization burden;
- require equivalent native validation cleanliness and no unexplained material frame-behavior or
  performance regression; and
- require ordinary access to the backend capabilities demonstrated by the task, either through the
  shared contract or a safe, bounded native extension.

Mulciber need not be universally preferable to direct native APIs for arbitrary experiments. It must
be preferable for at least the serious-game workload and lifecycle contract it claims to support.

## Gate records and artifacts

Comparison implementations and harnesses are source and belong in the repository. Large logs,
captures, screenshots, and machine inventories belong under `validation-artifacts/` and remain
ignored. Each decision record must contain:

- Mulciber and competitor revisions;
- exact task source and build commands;
- hardware, OS, driver, displays, compiler, and validation configuration;
- raw and summarized measurements;
- unavailable or non-equivalent functionality;
- single-backend results with portability excluded;
- known threats to a fair comparison; and
- a pass, redesign, narrow, or stop decision tied to the applicable viability gate.

Changing a comparison target, task, threshold, or scoring rule after results exist requires a written
reason and preserves the previous result. This prevents benchmark selection from becoming a moving
goalpost.
