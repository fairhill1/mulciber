# Gate 2 comparison record

This document pre-registers the first Gate 2 comparison required by the
[API extraction and comparison plan](api-extraction-plan.md) and the
[viability gates](viability-gates.md). The protocol below was committed before any comparative
result was recorded; per the plan, changing a target, task, threshold, or scoring rule after
results exist requires a written reason here and preserves the previous result.

## Targets and pinned revisions

The first executed comparison is `wgpu` + `winit`, the central established safe portable Rust
baseline. SDL3 GPU, Vulkano, the practical single-backend stacks, and scoped raylib follow with
this same protocol once the harness is proven against one target; executing the core comparison
well precedes a long competitor list.

| Side | Source | Pinned revision |
| --- | --- | --- |
| Mulciber | this repository, `examples/cube` and `probes/api-cube` | the revision recorded with each result below |
| wgpu | crates.io `wgpu` | `=30.0.0` |
| winit | crates.io `winit` | `=0.30.13` (latest stable line; `0.31.0-beta.2` excluded as a prerelease) |
| shared math | crates.io `glam` | `=0.33.2` (same on both sides) |
| wgpu-side helpers | crates.io `bytemuck`, `pollster` | resolved in `comparisons/Cargo.lock`, which is the authoritative pin for the whole comparison tree |

The comparison implementation lives in `comparisons/wgpu-cube` inside a separate cargo workspace
so the main workspace's dependency story remains its own measurement. It is written as an
ordinary best-practice `wgpu`+`winit` application: `winit 0.30` `ApplicationHandler`, FIFO
presentation, reconfigure on `Resized` and on outdated/lost acquisition, MSAA resolve into the
surface texture, and no unsafe code. It shares the exact WGSL module
(`examples/cube/src/cube.wgsl`, via `include_str!`) and restates the same scene data with its own
vertex type. Both implementations print the same observability lines (`surface configured` /
`surface generation N configured`, `presented N textured cube frame(s)`) and accept the same
`--frames N` and `--force-one-sample` flags.

## Tasks under comparison

From the plan's task list, this record covers:

1. **Clear** — subsumed by the cube scene's cleared background; both remain FIFO-paced.
2. **Representative draw** — indexed textured cube with depth, three-buffer scene data, one WGSL
   module, perspective-correct animation.
3. **Lifecycle** — the standard KWin resize storm (350 server-side geometry steps at 10 ms,
   `comparisons/harness/resize-storm.js`, identical walk to the record in
   [Linux validation](linux-validation.md)), plus interactive drag-resize, minimize/restore, and
   titlebar close.
4. **Optional fallback** — preferred four-sample MSAA with the observable forced one-sample path.
5. **Failure diagnosis** — one intentionally invalid resource request per side; the diagnostic is
   judged on whether it identifies the violated contract and a likely correction.

Task 6 (native differentiation) is Gate 4 scope; task 7 (integrated runtime) is Gate 5 scope.

## Fixed measurement configuration

- **Machines.** Linux: x86-64 CachyOS desktop, i5-12400F (12 threads), RTX 3060 Ti
  (proprietary driver 610.43.03), KDE Plasma native Wayland, single 75 Hz display, Rust 1.97.0.
  macOS: Apple M2 MacBook Air (8 cores), macOS 15.7.7, Metal 3 tier, built-in 60 Hz display,
  Rust 1.97.0. Windows: RTX 3060 Ti tier, Windows 11 — deferred until the machine is next booted.
- **Build profiles.** Behavior and validation runs use the `dev` profile; size and build-time
  measurements use the default `release` profile of each workspace. Cold build means
  `cargo clean` of the implementation's workspace followed by one timed build of the example
  binary, no compiler cache or rustc wrapper.
- **Frame counts.** Finite validation runs use 120 frames (and 60 for the forced one-sample
  rerun); storm runs are bounded by the 350-step script closing the window, with whole-run counts
  reported and no warmup exclusion.
- **Validation.** Mulciber requires the Khronos validation layer (Vulkan) or runs under
  `MTL_DEBUG_LAYER=1` (Metal) and fails runs on any warning or error. wgpu correctness runs use
  its always-on internal validation with uncaptured errors fatal, plus
  `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation` on Vulkan for layer parity; any validation
  output fails the run.
- **Lifecycle metric.** For each storm run: exit status, count of surface
  configurations/generations, and presented-frame count from the run log. Higher presented frames
  under the same storm on the same display means less presentation stall; visual trailing
  judgments additionally require an interactive drag observation.
- **Ergonomics metrics.** Application lines (via `wc -l` on the example's `src`, shader counted
  separately, `Cargo.toml` and generated artifacts excluded), count of application-visible
  concepts needed for ownership and frame flow, and application-owned resize/synchronization/
  shutdown bookkeeping, discussed rather than flattened into one number.
- **Cost metrics.** Cold build wall clock, as-built and stripped binary size, `cargo tree`
  direct and transitive dependency counts, and clean `target/` size after one release build.

## Known threats to fairness

- The Mulciber cube was developed against this exact scene; wgpu-cube was ported from it. Both
  therefore encode the same requirements, but Mulciber's API was shaped partly by this workload.
- The author of both implementations is the Mulciber author. The wgpu side follows current
  upstream-documented patterns (`ApplicationHandler`, surface reconfigure on resize, MSAA
  resolve targets) to keep it a reasonable best-practice implementation, and it is preserved in
  the repository for independent review.
- wgpu 30 is used through its Rust API only; no WebGPU/browser considerations are measured.
- The Linux lifecycle storm exercises one compositor (KWin Wayland) on one display; the plan's
  broader display-change and multi-display coverage is not part of this record.
- Readback-based image comparison is not yet part of this record: the Mulciber slice does not
  expose a readback path, so cross-implementation correctness relies on validation-clean runs
  plus human visual comparison of the same scene. This is recorded as a gap rather than waived.

## Results

Results are recorded only below this line, after the protocol above was committed, each tagged
with the Mulciber revision and dates. No results yet.
