# API slice decision ledger

This ledger records the decisions the [API extraction and comparison plan](api-extraction-plan.md)
required the first slice to establish. Each entry states what is decided for the experimental slice,
where the deciding contract or code lives, and what deliberately stays open. Per the plan, a
decision the slice does not need stays open rather than receiving a speculative general solution;
every name remains an unstable Gate 2 experiment.

## Application and event-loop ownership

Decided. `mulciber-platform` owns a main/creating-thread-confined `Application` and `Window`; the
game calls `Application::pump_events` and receives translated lifecycle, redraw, metric, and close
events through a fallible callback whose first error the pump returns, keeping its own architecture
without a per-application error slot. Platform types are neither `Send` nor `Sync`, so
native-thread ownership is structural. Nested native dispatch (the Win32 sizing loop) may deliver
redraw inside the pump; handler errors propagate out of that nesting through the platform layer.
The platform also owns the startup wait for first drawable metrics
(`Application::wait_for_first_metrics`). Recorded in the
[experimental platform contract](api-platform-contract.md).

## Object topology

Decided for the slice. `OpenedGraphics::open` consumes a borrowed window surface target plus current
metrics and produces distinct `Device` (resource creation), `Queue` (submission), and `Surface`
(presentation) owners over one private native session, plus an observable `DeviceSelection`.
Vulkan's surface-compatible adapter selection happens inside opening without distorting Metal
initialization. The session keeps instance/adapter/queue/presentation/retirement lifetimes ordered;
explicit `shutdown` refuses to run while an acquired frame is live. Whether opening later splits
into separate public context/selection values stays open. Recorded in the
[textured-cube contract](api-cube-contract.md) and implemented in `crates/mulciber/src/graphics.rs`.

## Surface generations

Decided. `WindowRevision` is desktop-OS input; `SurfaceGeneration` is graphics-owned output that
advances on every successful replacement presentation configuration, including same-extent
replacements and Vulkan's base-swapchain abandonment fallback; suspension alone does not advance
it. Extent-dependent resources belong to exactly one generation and are rejected, then reclaimed,
when superseded. Recorded in the
[experimental graphics lifecycle contract](api-graphics-contract.md).

## Frame lifecycle

Decided. Acquisition returns a ready surface-scoped frame or a temporary-unavailability reason
(suspended, drawable absent, timed out, or reconfiguration deliberately paced). Reconfiguration for
changed metrics happens inside acquisition: a ready frame always matches the requested metrics, and
a frame whose surface information differs from the application's render targets is the one rebuild
signal, enforced by draw-time rejection. A separate reconfigured outcome was implemented first and
rejected with physical Wayland evidence — both validated native probes already reconfigure inside
their own frame machinery, and the separate outcome made trailing live resize the ergonomic default.
Every ready frame receives exactly one fallible disposition: present or explicit abandonment, with
`Drop` as best-effort deferred abandonment. Backends keep different native machinery (Metal
autorelease-scoped drawables; Vulkan acquisition fences, swapchain maintenance, or whole-generation
replacement). Recorded in the
[experimental graphics lifecycle contract](api-graphics-contract.md).

## Resource use and synchronization

Decided for the slice, deliberately narrow. The queue first exposed one resource-backed operation —
draw one indexed textured mesh with depth into generation-matched targets and present — with all
hazard translation backend-owned. A later two-pass checkpoint adds generation-bound resolved scene
color and a fixed fullscreen sampled pass. Metal uses ordered encoders; Vulkan derives the explicit
color-attachment-write to fragment-sampled-read transition behind the same safe operation.

No general render-pass or command-encoder vocabulary exists yet. The second operation establishes a
real intermediate-resource dependency but still does not constrain arbitrary pass ordering,
load/store policy, multiple draws, transient allocation, or copy/compute integration enough to
justify a broad API. Recorded in the [textured-cube contract](api-cube-contract.md) and
[two-pass postprocess contract](postprocess-contract.md).

## Capabilities and fallbacks

Decided for the slice. `DeviceRequest` carries the preferred sample count; unsupported four-sample
rendering falls back observably to one through `DeviceSelection`, which also reports the selected
backend. Required capabilities (validation availability, surface-compatible device) reject opening
with a structured error. The general optional-capability vocabulary beyond multisampling stays
open. Implemented in `crates/mulciber/src/graphics.rs`; exercised by the api-cube probe's forced
one-sample path.

## Errors and recovery

Partially decided. Nonfatal states are typed outcomes, not errors: retry-later is
`SurfaceUnavailable`, rebuild is the frame/target generation mismatch, and `Result::Err` carries a
structured `GraphicsError` for genuine failures including deferred abandonment failures surfaced by
the next fallible surface operation. Validation warnings and errors fail validation runs. Final
error categories and native diagnostic payloads remain open in the
[graphics contract](api-graphics-contract.md).

## Native reach

Deliberately open. The first slice exposes no backend-specific capability boundary; the hidden
`integration` module is probe machinery, not the answer. The recorded constraint any future reach
must satisfy: it cannot invalidate session-owned resource and presentation-retirement tracking.
This stays open until Gate 4 pressure produces a real consumer.

## Backend selection and cost

Decided. The compilation target selects the backend at `cfg(target_os)` module level — Metal on
macOS, Vulkan on Windows and Linux — with no cargo features, no runtime backend dispatch, and no
unused-backend code compiled, linked, or initialized. Single-backend build proof and measured cost
are recorded in the platform validation ledgers ([Linux](linux-validation.md),
[macOS](macos-validation.md)).

## Shader inputs

Decided for the slice. Applications ship one WGSL module compiled offline by the separately
installed `mulciber-shader` tool (pinned Naga) into target-selected SPIR-V or MSL/metallib
artifacts; ordinary builds embed the checked-in or cached artifact and never depend on the
compiler. This intentionally does not select the eventual authoring language; advanced capabilities
keep independent native paths until a single-source path has equivalent evidence. Recorded in the
[textured-cube contract](api-cube-contract.md) and the
[shader toolchain evaluation](shader-toolchain-evaluation.md).
