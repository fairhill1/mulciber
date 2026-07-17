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
with the Mulciber revision and dates.

### Linux, wgpu+winit, 2026-07-17, Mulciber revision `7698e26`

All runs on the pre-registered Linux machine in one native KDE Wayland session, back to back to
control compositor variance.

**Correctness.** Both implementations passed 120-frame four-sample and 60-frame forced one-sample
finite runs with zero validation output (Mulciber with its required Khronos layer; wgpu with
internal validation plus `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`) and exited zero.
Deterministic readback comparison remains an open gap as pre-registered.

**Operator drag observation (2026-07-17, revision `0eadc70`, high-polling mouse).** Both
implementations ran correctly side by side. During interactive drag-resize the `wgpu-cube` window
trailed the pointer severely while the Mulciber cube tracked it — the same FIFO-backpressure
pathology Mulciber exhibited before its backend gained paced extent-driven reconfiguration
(committed sizes outrun FIFO presentation on Wayland; see [Linux validation](linux-validation.md)).
Both sides ran the pre-registered `PresentMode::Fifo`; wgpu applications commonly avoid the
symptom by selecting mailbox/no-vsync presentation, which sidesteps rather than solves the
vsynced-path behavior. On the canonical vsynced path this is a concrete Mulciber lifecycle
advantage; a wgpu-side application-level mitigation (resize debouncing) is possible but is
application bookkeeping the Mulciber contract owns internally.

**Lifecycle (identical 350-step / 10 ms KWin storm via `comparisons/harness/run-resize-storm.sh`).**

| Measure | Mulciber `api-cube` | `wgpu-cube` |
| --- | --- | --- |
| Exit status | 0 | 0 |
| Distinct sizes committed (generations / configures) | 211 | 109 |
| Presented frames over the whole run | 694 | 437 |

Mulciber presented ~1.6x the frames while tracking ~2x the distinct sizes under the same
server-side walk. An earlier same-day run of the Mulciber side alone (recorded in
[Linux validation](linux-validation.md)) presented 1114 frames, so absolute counts vary
meaningfully between sessions; the paired same-session run above is the comparative datum, and
future storm results should always be recorded as same-session pairs.

**Cost (default release profiles, cold builds, no compiler cache).**

| Measure | Mulciber `examples/cube` | `wgpu-cube` |
| --- | --- | --- |
| Cold release build | 1.2 s | 28.3 s |
| Binary as built | 644,200 B | 12,248,944 B |
| Binary stripped | 499,504 B | 8,755,352 B |
| Unique crates in `cargo tree` | 4 | 144 |
| `target/` after one release build | 16 MiB | 607 MiB |

**Application size (shared 33-line WGSL module counted once, listed separately).**

| Source | Lines |
| --- | --- |
| Mulciber `examples/cube` (`main.rs` + `scene.rs`) | 129 + 74 |
| Mulciber `probes/api-cube` (adds `--frames`/abandonment/fallback controls) | 186 (+ shared `scene.rs`) |
| `wgpu-cube` (`main.rs` + `scene.rs`) | 527 + 82 |

`wgpu-cube` includes the `--frames`/`--force-one-sample` controls (roughly forty lines), so its
fair line comparison sits between the Mulciber example and probe; on either basis the wgpu side
is two to four times larger, with the difference concentrated in device/surface plumbing,
pipeline and bind-group declaration, and resize/acquire-outcome handling that Mulciber owns
behind its contract.

**Not yet recorded.** CPU frame-time distributions, process memory, macOS (Metal 3, 60 Hz) and
Windows repeats, failure-diagnosis judging (task 5), and operator visual confirmation. Each will
be appended under this protocol.

**Blind model ergonomics review (2026-07-17, revision `2b90fc4`).** A no-context Claude Fable 5
instance was given both application sources unlabeled and asked which is more ergonomic. It chose
the Mulciber example "and it's not close," crediting the four resource one-liners against roughly
250 lines of wgpu descriptor/bind-group/pipeline declaration, the single draw-and-present
operation making MSAA store-op and resolve mistakes unrepresentable, the declare-intent
`DeviceRequest` with an observable selection report, and the two-arm acquisition match against
wgpu's six-variant surface state machine "every wgpu app reimplements slightly wrong." It
identified four Mulciber frictions, all platform-shape rather than graphics-shape: error plumbing
out of the `pump_events` closure (it proposed a `Result`-returning event callback), the
copy-pasted initial-metrics wait, the manual render-target rebuild line (retained deliberately as
the application's one point of generation awareness), and `ClearColor`'s fallible constructor on
literals. Attribution per the operator: most of Mulciber was developed with a different vendor's
model (ChatGPT), while the reviewer shares a model family with the current assistant that
authored the acquisition reshape and this comparison — so family-correlated taste is a partial
threat for the newest API surface specifically, and this counts as one blind model review, not
independent confirmation. The pump-error and initial-metrics items are accepted as
platform-layer API work; the review's line-count attribution matches the recorded
application-size table.

**Disposition of the blind-review frictions (2026-07-17, uncommitted tree on `ccfc4d7`).** Three
of the four frictions were fixed in the platform and graphics layers rather than per application:
`pump_events` now takes a fallible handler (`FnMut(WindowEvent) -> Result<(), E>` for any error
type convertible from `PlatformError`) and returns the first handler error after native dispatch
completes — deleting the error-slot/IIFE idiom from every binary, including across Win32's nested
sizing loop; `Application::wait_for_first_metrics` owns the startup metrics wait previously
copy-pasted into five binaries; and `ClearColor::opaque` is a const constructor that turns an
invalid literal into a compile-time failure, removing the `.expect` on constants. The fourth —
the manual render-target rebuild — is retained deliberately as recorded above: it is the
application's single point of generation awareness, and its forget-it failure mode is a precise
draw-time validation error rather than corruption; recorded here as an open tension instead of a
fix. After the reshape, `examples/cube` `main.rs` is 90 lines (from 129) with `scene.rs`
unchanged; the pre-registered size table above keeps its original revision's figures. The changed
pump contract was revalidated the same day: Linux Wayland and XWayland finite runs (conformance
13/13, 120-frame cube, clear abandonment/recovery) under the Khronos layer, and macOS Metal runs
over SSH (conformance 12/12 per the recorded Metal baseline, 120-frame cube, clear
abandonment/recovery, `metal-triangle` 120-frame abandonment run) under `MTL_DEBUG_LAYER=1`, all
exiting zero with no validation output; Win32 and macOS additionally compile and lint clean from
Linux via `--target`. Physical interactive lifecycle evidence for the new pump error path is not
claimed.
