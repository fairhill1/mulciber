# Vulkan immutable mesh memory

Mulciber 0.13.9 prefers device-local storage for immutable mesh vertices, indices and indirect
arguments. This is backend policy; the public mesh/mesh-parts API, per-frame buffers and Metal
storage are unchanged.

## Placement and ownership

The coalescing mesh arena keeps its 64 MiB blocks (or a larger power of two for an oversized mesh).
It selects directly mapped local memory when a host-visible coherent type covers the largest local
heap, accommodating UMA/full BAR without selecting a small discrete-GPU BAR as the main mesh heap.
Otherwise, it uses device-local blocks and staged uploads. Unsupported placement and allocation
exhaustion select host-visible coherent storage with a diagnostic; device loss, validation errors
and other native failures propagate. `MULCIBER_VULKAN_MESH_MEMORY=host` forces the host path for
validation/comparison. Normal selection needs no environment variable.

Staged mesh creation retains packed CPU bytes until queue submission. The next rendered frame copies
new ranges before consuming draws, with transfer-write to vertex/index/indirect-read barriers on the
same graphics queue. Each frame slot owns its coherent staging buffer until its fence completes.
Small buffers are reused; buffers larger than 4 MiB are released on the next completed-slot reuse.
At most 12 MiB of small staging capacity remains cached across three slots. Large queued/in-flight
uploads can temporarily exceed this bound. No per-mesh CPU mirror remains after submission.

Abandonment and failed recording/submission preserve pending upload bytes. Dropping an unsubmitted
mesh cancels its copy. Once queue submission succeeds, CPU bytes are released even if presentation
subsequently fails; the submitted staging buffer remains owned by that slot. Explicit destruction
and lazy reclamation still drain all frame slots before returning mesh regions. Shutdown drains GPU
work before freeing staging and mesh storage. No new queue, shader or public buffer API is added.

## Timing and validation

Frame-ring conformance exposed out-of-order GPU feedback after abandonment skipped a slot. The
collector now drains every older pending timestamp block in submission order when acquisition
completes a slot's fence: earlier submissions on that graphics queue have also completed. This adds
no fence wait and does not rotate frame slots during unsuccessful acquisition polls.

On the RTX 3060 Ti / Nvidia 610.57.04 / KDE tier, all 96 conformance cases pass with Khronos
validation on native Wayland, forced-host Wayland, and X11 through XWayland. Cases cover initial
abandonment, mixed-width mesh parts, material/shadow/instance draws, drop churn, oversized uploads
followed by small-mesh slot reuse, frame-buffer growth, ordered timings and fallible shutdown. The
stale postprocess error expectation now derives from the public uniform-size limit.

Unit evidence covers small BAR versus full mapped heaps, bounded staging retention, preservation of
device/validation failures and existing packing/coalescing behavior. Workspace format, check,
Clippy and tests pass; library cross-checks pass for Windows MSVC and Apple-silicon macOS.

These checks do not establish automatic UMA/full-BAR selection on hardware, actual allocation-
exhaustion recovery, human resize/minimize/restore, multi-display, native Xorg or deterministic
visual readback. Forced host placement validates fallback execution, not memory-pressure recovery.
Broader platform/viability claims remain unchanged.

## Game evidence

The motivating Isle of Rán prototype used three alternating pairs on one RTX 3060 Ti at 2560×1440,
100% scale, 4× MSAA, approximately 75 Hz FIFO, with identical settings/shaders and a verified outdoor
route. Host meshes averaged 67.37 FPS versus 72.34 FPS for local meshes (+7.4%); GPU time fell from
10.94 to 9.71 ms (−11.3%). Wall p99 stayed about 26.7 ms. This prototype retained staging mirrors;
those numbers motivate the change rather than measure the final bounded-staging implementation.

A follow-up pair using the final 0.13.9 implementation measured 67.78 FPS with forced host storage
and 74.71 FPS with default local storage (+10.2%). GPU time fell from 10.93 to 9.73 ms (−11.1%);
wall p99 was 26.68 versus 13.88 ms. This single pair confirms the final implementation retains the
benefit, but does not establish a repeatable p99 improvement. Both completed all 1800 ticks with
31 identical actor checkpoints, matching player route and streaming totals, and over 99.8% correlated
GPU coverage. The game capture instrumentation and settings were the same in both modes.

The route is `--newgame --trace --trace-walk --at 1484,-2820 --look 0`, after 300 warm-up ticks and
five streaming-quiet seconds. It travels about 231 m across 78×71 m and exercises terrain/grass
streaming. Valid runs complete 1800 simulation ticks, match actor/player checkpoints and provide
correlated GPU timings. Earlier attempts at an indoor location were discarded.

Prototype archives: `validation-artifacts/isle-mesh-memory-2026-09-08/`.
Release validation and follow-up captures: `validation-artifacts/mesh-memory-release-2026-09-08/`.
