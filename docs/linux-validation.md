# Linux Vulkan capability and presentation validation runbook

The Linux capability probe has peer X11 and Wayland paths. Both share device/report collection while
retaining native window, loader, and Vulkan surface creation in separate modules.

## Current status

- X11 and Wayland compile, lint, link, and pass their shared report tests on x86-64 Linux.
- WSLg previously created both native client objects and reported Vulkan 1.3.275, but the former
  Vulkan 1.4 gate rejected it before instance/surface creation. The new feature-checked 1.3 baseline
  requires a WSLg rerun before any execution claim is made.
- Native Wayland and X11-through-XWayland Vulkan 1.4 capability results were recorded on physical
  Linux on 2026-07-16. Native Xorg and broader driver/hardware coverage remain pending.
- One-shot acquired-frame non-presentation was physically exercised on native Wayland on
  2026-07-17 through both `VK_KHR_swapchain_maintenance1` image release and the forced
  base-swapchain generation-replacement path, followed by 120 presented recovery frames.
- Physical human input, pointer-capture, and playable game-slice evidence on native Wayland and
  X11 through XWayland was recorded on 2026-07-20 at committed `3075d0e`; modifier-transition,
  trackpad-unit, repeat-cadence, non-KDE-compositor, and native Xorg coverage remain pending.
- The capability report's Wayland path creates an unconfigured `wl_surface` only for Vulkan queries.
- The Vulkan triangle probe consumes runtime-selected peer Wayland and X11 modules from
  `mulciber-platform`, behind a `--platform` flag with `WAYLAND_DISPLAY`/`DISPLAY` autodetection.
  The Wayland module creates a real XDG-shell toplevel, requests server-side decorations, presents
  through `VK_KHR_wayland_surface`, coalesces configure events, and paces resize swapchain commits. Its
  event pump takes socket input through the libwayland `wl_display_prepare_read` /
  `wl_display_read_events` protocol; see the presentation-stall correction below for why a
  blocking `wl_display_dispatch` is incorrect on this connection.
- The X11 triangle module creates a real Xlib toplevel with `WM_DELETE_WINDOW` registration,
  structure-notification tracking, and a `None` background pixmap (so interactive resize does not
  flash the server's solid background), waits for the initial `MapNotify`, and presents through
  `VK_KHR_xlib_surface`. Presentation, unlocked pacing, and physical lifecycle evidence through
  XWayland is recorded below; native Xorg coverage remains pending.

The capability results complete the two capability-inventory ports. The Wayland presentation item
remains incomplete pending display-change, explicit zero-sized suspension, input, and broader
compositor/hardware evidence. The X11 presentation item remains incomplete pending native Xorg,
display-change, input, multi-display, and broader hardware evidence.

### Platform extraction smoke evidence

On 2026-07-17, an uncommitted development tree based on `e573d68` moved the triangle probe's native
Wayland and X11 window/event implementations into `mulciber-platform` and left Vulkan surface
creation in a narrow probe adapter over borrowed native handles. On the native KDE Wayland session
and RTX 3060 Ti tier described below, these finite runs each presented 120 frames and exited zero
with no validation warning/error callbacks:

```sh
target/debug/mulciber-vulkan-triangle --platform wayland --frames 120
target/debug/mulciber-vulkan-triangle --platform x11 --frames 120
```

The first run used native Wayland; the second used XWayland from the same session. This confirms
construction, native event pumping, Vulkan surface creation, presentation, and orderly shutdown
through the extracted boundary. It is automated finite-run evidence, not a repeat of physical
resize, minimize/restore, close, display-change, input, or visual-correctness coverage.

### Extracted graphics slice resize evidence

On 2026-07-17, an uncommitted development tree based on `09d2477` exercised the extracted
same-source cube slice on the native KDE Wayland session and RTX 3060 Ti tier described below,
the slice's first Linux execution record. An interactive drag-resize of `examples/cube` first
reproduced two defects: retained per-generation render targets grew until `vkAllocateMemory`
failed with -2 while validation reported one leaked `VkImage` from that failed image's partial
construction, and unpaced per-configure swapchain recreation let FIFO presentation backlog make
the window trail input by multiple seconds.

After the fixes, on the same session with continuous server-side resizes driven through KWin's
scripting interface:

- `mulciber-api-cube --frames 120 --abandon-acquired-frame-once` and
  `mulciber-api-cube --frames 120 --force-one-sample` each exited zero on native Wayland with no
  validation output, covering 4x selection with abandonment recovery and the observable 1x
  fallback.
- With stale-generation render-target reclamation, a 110-step scripted resize walked surface
  generations 2 through 111 across extents from 457x315 to roughly 1600x1000, presented 1201
  frames, exited zero with no validation output, and whole-GPU memory samples stayed within
  596-708 MiB across the storm instead of growing per generation.
- With extent-driven reconfiguration pacing, a 350-step scripted resize at 10 ms intervals
  produced 212 reconfigurations at the 16 ms Wayland pace, presented 544 frames, and exited zero
  with no validation output. A subsequent interactive drag-resize of `examples/cube` tracked the
  pointer without the earlier trailing.

On the same day and session, an uncommitted tree based on `286fcfb` folded reconfiguration into
acquisition (removing the separate reconfigured outcome). Under the identical 350-step / 10 ms
KWin-scripted storm, `mulciber-api-cube` walked 210 paced surface generations, presented 1114
frames — up from 544 under the separate-outcome shape, because no redraw is spent on a
reconfiguration round-trip — and exited zero with no validation output. Finite reruns of
`--frames 90 --abandon-acquired-frame-once` (generation replacement recovered through the
frame/target mismatch rebuild), `--frames 60 --force-one-sample`, and the api-clear abandonment
run also exited zero.

This is automated, single-machine, single-display native Wayland evidence plus one interactive
drag-resize smoke. Extracted-slice X11, native Xorg, minimize/restore, display-change,
multi-display, and broader hardware coverage remain unrecorded.

### Linux input and pointer-capture evidence

On 2026-07-20, an agent-driven session on the same native KDE Plasma / Nvidia machine exercised
the first Wayland and X11 input implementations (uncommitted tree based on `e894fd4`), which add
seat/keyboard/pointer translation, xkb-derived modifier masks, client-side key repeat, and the
pointer-capture backends described in the [input contract](input-contract.md):

- **X11 through XWayland, full input pipeline (automated, XTEST-driven)**:
  `mulciber-input-cube` ran with `WAYLAND_DISPLAY` unset while `xdotool` synthesized W/A/S/D,
  Space, and R key transitions, a primary-button drag, wheel scrolls in both directions, a `C`
  keypress that engaged pointer capture (confirmed by the example's `cursor mode: Captured`
  output), relative motion while captured, and an Escape release. With the window at a known
  geometry, `xdotool getmouselocation` read exactly the content-area center after each relative
  move while captured — direct evidence of the grab-plus-warp delta path — and moved freely again
  after Escape. A KWin-scripted window close then exited zero.
- **External-destroy resilience**: `xdotool windowclose` issues a raw `XDestroyWindow` from
  another client. This initially aborted the process through Xlib's fatal `BadWindow` handler when
  `Window::drop` re-destroyed the dead handle; `DestroyNotify` now marks the window destroyed so
  drop skips it, and the same sequence exits zero through the ordinary close path.
- **X11 UTF-8 titles**: the interactive run surfaced mojibake in the title bar (the em dash's
  UTF-8 bytes read as Latin-1) because only the legacy `WM_NAME` was set through `XStoreName`;
  the module now also sets `_NET_WM_NAME` as `UTF8_STRING`, and the title renders correctly.
- **Native Wayland lifecycle with input listeners live**: `mulciber-input-cube` ran on the native
  Wayland session with the seat bound at version five and keyboard/pointer listeners registered
  (KWin delivers the xkb keymap and capabilities during construction roundtrips), rendered
  normally, and exited zero through a KWin-scripted close.
- **Native Wayland capture protocol (automated probe)**: a scratchpad probe drove
  `set_cursor_mode` directly against live KWin: engage (constraint lock, relative pointer, cursor
  hide), thirty pumped frames, an idempotent second request, release (cursor-shape restore),
  re-engage, and drop-while-captured, all without a compositor protocol error and exiting zero.

KWin advertised `zwp_pointer_constraints_v1`, `zwp_relative_pointer_manager_v1`, and
`wp_cursor_shape_manager_v1`; compositors lacking any of the three report capture as unsupported
and that path is untested against a real compositor. This is automated single-machine evidence:
physical (human) keyboard/mouse interaction on Wayland and X11, key-repeat cadence observation,
modifier and focus-transition coverage, native Xorg, and non-KDE compositors remain unrecorded.

The Wayland keymap path adds `libxkbcommon` as a Linux link-time dependency beside
`libwayland-client`/`libX11`/`libXext`, used solely to resolve modifier-mask bit positions from
the compositor-supplied keymap; the single-backend `ldd` list recorded above predates it.

### Physical Linux input, capture, and game-slice evidence

On 2026-07-20, the operator physically exercised committed revision `3075d0e` (clean tree equal to
`origin/main`) on the same physical machine: CachyOS, Linux 7.1.3, KDE Plasma 6.7.3 in a native
Wayland session, Nvidia RTX 3060 Ti (vulkaninfo `driverVersion` 610.43.3.0, device API 1.4.341).
Four interactive sessions ran the ordinary examples — whose Vulkan backend requires and enables the
Khronos validation layer and fails shutdown on any validation message — once each on native Wayland
and on X11 through the session's XWayland with `WAYLAND_DISPLAY` unset. All four processes selected
Vulkan with four samples and exited zero with no validation output.

- **`mulciber-input-cube` on native Wayland** (server-side decorations confirmed): per-tap W/A/S/D
  and arrow rotation; held-key rotation through the client-synthesized repeat path; Space
  pause/resume without repeat retriggering; R reset; primary-button drag orbit; wheel zoom; C
  capture with a hidden cursor and relative look the pointer could not escape; Escape release
  restoring the arrow cursor; Alt-Tab away while captured releasing cleanly; a held key cleared by
  focus loss with no stuck rotation on return; minimize/restore; maximize/restore; live drag
  resize including very small sizes; titlebar close. The console log records repeated
  capture/release cycles and ends in the captured state, so window teardown from active capture was
  also exercised.
- **`mulciber-input-cube` on X11**: the same checklist through detectable auto-repeat and the
  confined-grab/warp-to-center capture path, with no observed look drift, stutter, or visible
  cursor reappearance while captured; closed through Alt-F4, covering the second close path
  interactively.
- **`mulciber-game-slice` on native Wayland** — the first physically played Linux run of Forge Run:
  movement with per-axis obstacle collision and camera follow, one sentry hit, all eight crystals
  collected through the win transition, an R reset, and a second partial run, all corroborated by
  the console log. The suspension sequence (hold a movement key, minimize, release while minimized,
  wait, restore) produced no catch-up jump and no stuck movement; Alt-Tab with a held key cleared
  it; drag resize and maximize/restore ran mid-game; titlebar close. Exit reported
  `presentation feedback: unsupported on this backend`, the expected explicit Vulkan gap from the
  [Gate 4 pacing plan](gate4-pacing-plan.md).
- **`mulciber-game-slice` on X11**: movement, collection (the log records two crystals, an R reset,
  then three more), one sentry hit, the same suspension and focus-loss checks, mid-game resize, and
  Alt-F4 close. The eight-crystal win transition was exercised on the Wayland session only.

This retires the physical-human-verification caveat on the KDE tier for both Linux input paths.
Still unexercised: modifier-key transitions, precise trackpad scroll units (a coarse wheel was
used), key-repeat cadence measured against the configured rate, the X11 win transition, display
changes, multi-display, non-KDE compositors, native Xorg, and other hardware/driver tiers.

### Presentation-feedback availability survey (Gate 4)

On 2026-07-20, the KDE Plasma 6.7.3 Wayland session and Nvidia RTX 3060 Ti tier described above
(vulkaninfo `driverVersion` 610.43.3.0, device API 1.4.341) were surveyed for the native
presentation-feedback mechanisms named in the [Gate 4 pacing plan](gate4-pacing-plan.md). This
records advertised availability only; no feedback path has been exercised on this tier.

- The Nvidia device extension list includes `VK_KHR_present_id` and `VK_KHR_present_wait`
  (revision 1, plus their `VK_KHR_present_id2`/`VK_KHR_present_wait2` successors),
  `VK_KHR_incremental_present` (revision 2), and `VK_EXT_present_timing` (revision 3) — the last
  reporting actual presentation timestamps, the closest Vulkan analog to Metal's presented
  handlers. `VK_GOOGLE_display_timing` is absent. vulkaninfo reports the single Nvidia adapter and
  no other ICD.
- KWin advertises `wp_presentation` version 2 with a `CLOCK_MONOTONIC` presentation clock
  (`wayland-info`), so compositor-side presentation-time feedback exists independently of the
  Vulkan extensions.
- XWayland lists the X11 `Present` extension, the XPresent path the plan names.

In contrast to the surveyed Windows Intel UHD 620 tier, which exposes none of these, every
candidate feedback source the pacing plan names is advertised on this tier. Which of them actually
delivers accurate identified presentation times through the Wayland and XWayland WSI paths is
behavior evidence for the plan's probe-first step; advertisement is not exercised-path evidence.

### Native present-timing probe evidence

Later on 2026-07-20, an uncommitted tree based on `e0b0c0e` extended the Vulkan triangle probe
with the `VK_EXT_present_timing` feedback path: the device enables
`VK_KHR_present_id2`/`VK_KHR_calibrated_timestamps`/`VK_EXT_present_timing` with the `presentId2`
and `presentTiming` features when the surface reports support, creates the swapchain with the
present-id-2 and present-timing flags, sizes the timing queue, selects a swapchain time domain
(preferring `CLOCK_MONOTONIC`), chains a per-present id and one-stage timing request, and drains
completed reports after every present into the pacing record beside the retained CPU
present-return estimation. Tiers without the chain keep the estimation-only report with the
observable reason, and `MULCIBER_VULKAN_FORCE_PRESENT_TIMING_FALLBACK=1` forces that path.

Automated finite runs on the machine above (KDE Plasma 6.7.3, 74.971 Hz display), all with
validation enabled, exit code zero, and no validation output:

- **Native Wayland, 300 frames**: the surface offered the first-pixel-out stage (not
  first-pixel-visible) and a non-monotonic device-selected time domain; the driver reported
  `refreshDuration` as zero, which the probe treats as unknown. 296 presents carried native times
  (4 untimed at the shutdown tail). Native intervals: min 13.289 ms, p50 13.338 ms, p99 13.388 ms,
  max 13.415 ms — tightly on the 13.339 ms vsync grid — while the same run's CPU present-return
  intervals spread 0.139–15.190 ms.
- **Native Wayland, 300 frames with the pre-registered 40 ms load spike over frames 100..130**:
  native spike intervals reported p50 40.016 ms, exactly three refresh periods, demonstrating
  vsync-quantized degradation; the CPU estimation reported noisy 40.31–40.55 ms returns. Steady
  native intervals kept 0 missed against the measured median.
- **X11 through XWayland, 300 frames**: the extension chain works but the surface offers only the
  queue-operations-end stage, whose times track present-return closely (p50 13.027 ms with the
  same 0.2–31 ms spread as the returns) — display-side feedback is not available through this WSI
  path on this tier.
- **Abandonment interplay**: `--frames 120 --abandon-acquired-frame-once` on both the
  `vkReleaseSwapchainImagesKHR` path and the forced whole-generation-replacement fallback (which
  resets the per-swapchain present-id sequence through swapchain recreation) completed
  validation-clean with 116 of 120 presents timed.
- **Forced estimation fallback**: `MULCIBER_VULKAN_FORCE_PRESENT_TIMING_FALLBACK=1` produced the
  labeled estimation-only report and exited zero.
- `--pacing-csv` now emits per-frame native offset/interval columns beside the return columns.

A physical interactive Wayland session (drag resize, minimize/restore, titlebar close) then
exposed a defect the finite runs could not: a nonsense interval of roughly 21,000 seconds,
because native times from different swapchains were paired across a live-resize recreation, and
each swapchain's time domain carries its own epoch. Intervals and CSV columns are now
generation-scoped — a swapchain recreation breaks a pair exactly like an untimed frame does. The
re-run interactive smoke (600 presents through continuous drag resize and titlebar close)
reported 491 timed presents with native intervals min 13.067 ms, p50 13.339 ms, max 13.566 ms
and 0 missed against the now-reported 13.338 ms refresh duration; the 109 untimed presents
belong to swapchains retired mid-resize. The driver reports `refreshDuration` as zero on the
first swapchain but real values after recreation, so a zero report is treated as unknown.

This is the first exercised Vulkan native presentation-feedback evidence in the project, single
machine and single display. The Wayland `wp_presentation` protocol path, Windows tiers, and pacing
policy remain open per the [Gate 4 pacing plan](gate4-pacing-plan.md); the extraction into
`Surface::take_present_feedback` is recorded next.

### Extracted present-feedback evidence

Later on 2026-07-20, the probe-proven feedback path above was extracted into the `mulciber`
crate: the Vulkan backend enables the same extension chain when the adapter and surface support
it, creates swapchains with the present-id-2/present-timing flags, chains a per-present id and
one-stage timing request at both present sites, and drains completed reports into the bounded
queue behind `Surface::take_present_feedback`, which previously answered `Unsupported` on every
Vulkan drain. Native times arrive in a swapchain-scoped time domain whose epoch is not the
process clock on this tier, so each swapchain's times are re-anchored to the drain instant of its
first completed report: intervals stay native-exact within one swapchain, absolute placement
carries at most one drain latency of bias, and times are never paired across recreations.
Unsupported tiers keep the explicit `Unsupported` answer with the selection reason recorded
internally.

Automated runs on the machine above, all with validation enforced at shutdown and exit code zero:

- `mulciber-api-cube --frames 120 --abandon-acquired-frame-once`, native Wayland: 117 presented
  frames reported, 0 without a display time, estimated cadence 13.338 ms, intervals min 13.300 /
  median 13.338 / p95 13.349 / max 13.379 ms, 0 missed — native vsync-grid intervals flowing
  through the public API into `mulciber-runtime::PacingDiagnostics`, spanning the
  abandonment-driven swapchain recreation without an epoch artifact.
- The same probe on X11 through XWayland: 120 frames reported, intervals min 0.086 / median
  13.323 / max 30.070 ms with 11 missed — the queue-operations-end-only stage from the probe
  survey tracks present-return rather than the display and is passed through faithfully.
- A KWin-scripted ten-second continuous resize storm over the probe: 2,545 presents across 201
  swapchain generations, 1,934 reported with display times, largest interval 547 ms from real
  recreation stalls with no cross-epoch artifact, validation-clean exit.
- `mulciber-api-clear --frames 120 --abandon-acquired-frame-once` and the nineteen-case
  `mulciber-api-conformance` suite passed with the timing chain active.
- `mulciber-game-slice` on native Wayland with a KWin-scripted close: 1,938 presented frames
  reported, 0 without a display time, cadence 13.338 ms, intervals 13.280–13.399 ms with 1
  missed — replacing the `presentation feedback: unsupported on this backend` line from the
  physical sessions at `3075d0e`.

These are automated static-window and scripted-resize runs; no new physical interactive claims
are made. Pacing policy and the remaining platform surveys stay open per the
[Gate 4 pacing plan](gate4-pacing-plan.md).

### Custom-material vocabulary evidence

On 2026-07-20, the custom-material checkpoint (see the
[material slice plan](material-slice-plan.md) and [material contract](material-contract.md))
was validated on the same machine and session. `mulciber-material-scene` — two
application-authored WGSL modules, two application-declared vertex layouts (position/normal/
uv/glow at stride 36 and position/uv at stride 20), per-frame application-packed uniform bytes
of 144 and 80 bytes, and a material sampling two textures — ran with the validation layer
enforced at shutdown, all exiting zero with no validation output:

- Native Wayland, roughly nine seconds of frames, closed by a KWin script: Vulkan backend, four
  samples.
- A KWin-scripted resize storm driving 200 geometry changes at 50 ms intervals, then closing the
  window: continuous swapchain and render-target replacement with material content and
  material descriptor pools, clean exit.
- X11 through XWayland (`WAYLAND_DISPLAY` unset), roughly nine seconds of frames, KWin-scripted
  close.

`mulciber-api-conformance` grew twelve material cases — five creation-time
declaration-versus-artifact rejections naming the offending slot, location, entry point, or
stride; three draw-time record rejections (uniform byte length, texture count, mesh/pipeline
layout mismatch); direct and postprocessed material presentations; explicit destruction plus
drop reclamation of the new pipeline kind; and a mixed-session rejection naming the material
pipeline handle — and passed all thirty-one cases on both native Wayland and X11 through
XWayland, exit zero, no validation output. These are automated runs; the visual appearance of
the material scene and the Metal implementation await operator confirmation and the next M2
session respectively.

### Mesh index-width and sampler-mode evidence

On 2026-07-20, `Device::create_mesh_with_layout` gained `MeshIndices` (16- or 32-bit), with the
index type stored per mesh and passed to every native index-buffer bind. The conformance probe's
shared material mesh now uploads 32-bit indices, so every material draw, presentation, and
reclamation case runs the u32 path, and a new creation-time case asserts an out-of-range u32
index is rejected. All thirty-two cases (thirty-one plus this driver's Vulkan-only
superseded-generation branch) passed on this machine's KDE Plasma session on both native Wayland
and X11 through XWayland, exit zero, no validation output. The Metal path compiles under the
cross-host `aarch64-apple-darwin` type check but awaits the next M2 session.

Later the same day, material sampler slots gained declared per-slot filter (`Nearest`/`Linear`)
and address (`Repeat`/`ClampToEdge`) modes, replacing the single crate-owned linear repeat
sampler with one pipeline-owned native sampler per declared slot on both backends. The direct
material presentation case now submits a second record through a nearest/clamp pipeline in the
same frame, asserting both sampler modes reach the native samplers. All thirty-three cases passed
on both native Wayland and X11 through XWayland, exit zero, no validation output, and the
material-scene example ran validation-clean on native Wayland (Vulkan, four samples). Nearest
filtering and clamping are asserted by execution, not visually verified; the Metal sampler path
again compiles cross-host and awaits the next M2 session.

Still later on 2026-07-20, material pipelines gained declared blend and depth modes from the
fixed set recorded in the [decision ledger](api-slice-decisions.md). The direct material
presentation frame now submits four records — the opaque test-write baseline, the nearest/clamp
sampler pipeline, an alpha-to-coverage cutout pipeline with depth off, and a
premultiplied-translucent pipeline with a read-only depth test — so every blend and depth mode
reaches native pipeline state in one submission. All thirty-five cases (thirty-four plus this
driver's Vulkan-only superseded-generation branch) passed on both native Wayland and X11 through
XWayland, exit zero, no validation output, and the material-scene example (updated to declare
the opaque baseline explicitly) again ran validation-clean on native Wayland (Vulkan, four
samples). Blending, coverage, and depth behavior are asserted by execution and validation
cleanliness, not visually verified; the Metal path compiles under the cross-host
`aarch64-apple-darwin` type check and awaits the next M2 session.

### Conformance probe evidence

`mulciber-api-conformance` passed all thirteen asserted cases on the native Wayland session and
RTX 3060 Ti tier on 2026-07-17: four creation-time invalid-usage rejections, draw-time non-finite
transform rejection, explicit abandonment, superseded-generation target rejection followed by a
rebuilt-target presentation (this driver's abandonment path replaced the generation, so the stale
branch executed rather than the stable-generation branch), fallible shutdown, observable forced
one-sample reopening, mixed-session handle rejection, one-sample presentation, and a second
fallible shutdown. Exit code zero with no validation output.

Later on 2026-07-17, an uncommitted tree based on `ccfc4d7` reshaped the platform pump contract
(fallible event handler returning the first application error, platform-owned
`wait_for_first_metrics`) and added the const `ClearColor::opaque` constructor. On the same
machine and session, the conformance probe repeated all thirteen cases, and `mulciber-api-cube
--frames 120` passed on both native Wayland and XWayland (`DISPLAY` path) with
`mulciber-api-clear --frames 60 --abandon-acquired-frame-once` recovering through abandonment,
all exiting zero with no validation output. These are finite static-window runs; no new
interactive lifecycle evidence is claimed for the reshaped pump error path.

### Single-backend build evidence

At revision `7d25d1f` on the x86-64 CachyOS machine below (i5-12400F, 12 threads, Rust 1.97.0,
default release profile), the Linux build of `examples/cube` was measured as the Vulkan-only
single-backend data point:

- `cargo tree` shows `mulciber` depending only on `mulciber-platform` and `mulciber-platform`
  depending on nothing; the example adds `glam` as its own math choice. No graphics, windowing,
  binding, or shader crate appears in the tree.
- `cargo clean` followed by `cargo build --release -p mulciber-cube` completed in 1.2 seconds of
  wall clock with no compiler cache or rustc wrapper configured.
- The produced binary is 644,200 bytes as built and 499,504 bytes stripped.
- `ldd` lists only libc/libm/libgcc, the dynamic loader, and the Linux platform peers
  (`libwayland-client`, `libX11`/`libXext`/`libxcb` with their transitive helpers). Both Linux
  platform paths are present by design as runtime-selected peers. `libvulkan` is not link-time
  required; the backend loads `libvulkan.so.1` at graphics open, so a Vulkan-less system fails
  with a structured error rather than at process start.
- The binary contains zero Metal, Objective-C, or AppKit symbols or strings
  (`objc_msgSend`, `CAMetalLayer`, `MTLDevice`, `metallib` all absent); the Metal backend module
  is excluded at `cfg(target_os)` level, so it is not compiled, linked, initialized, or reachable.
- Backend dispatch is compile-time module aliasing (`crates/mulciber/src/backend/mod.rs`); the
  ordinary frame path contains no backend-selection branch, table, or trait object.

The Metal-only mirror of this record belongs to the macOS runbook.

## Recorded evidence

Revision `d5a50a490063b99d04d5dcfc4282c39f883b1bbe` was exercised on a physical
x86-64 CachyOS system running Linux 7.1.3, KDE Plasma in a native Wayland session, an Nvidia RTX
3060 Ti with proprietary driver 610.43.03, Vulkan loader 1.4.350, device API 1.4.341, and Rust 1.97.
The working tree was clean and equal to `origin/main` before capture.

The native Wayland report selected the single adapter as baseline compatible, reported six queue
families, three memory heaps, 277 device extensions, 28 surface formats, and mailbox,
FIFO-latest-ready, FIFO, and immediate presentation modes. The explicit X11 report ran against
XWayland 24.1.13, selected the same adapter, and reported two surface formats plus FIFO, immediate,
and FIFO-latest-ready modes. Both JSON reports parsed without repair, contained no baseline failures,
and agreed with the human-readable reports and full `vulkaninfo` inventory.

The ignored validation archive is
`validation-artifacts/linux-vulkan-20260716-160107.tar.gz` with SHA-256
`8e1e8dc9d099536b0819b31717dc46de825271e182c81d8e7fbc30854a852d68`. It contains
the complete environment, preflight, capability, display-server, and Vulkan inventory logs. The
Khronos validation layer was not installed when that capability archive was captured. It was
installed before the later presentation work described below.

### Initial Wayland presentation evidence

The presentation implementation was exercised from an uncommitted working tree based on revision
`d5a50a490063b99d04d5dcfc4282c39f883b1bbe`; it is therefore development evidence, not a clean
revision archive. On the physical KDE Wayland session described above, the XDG toplevel visibly
rendered the full compute, shadow, 4x-MSAA scene, post-process, and BC1 workload with server-side
window chrome. The user physically confirmed minimize/restore, maximize/restore, titlebar close, and
responsive corner drag-resize. Vulkan validation reported no warning or error messages, and shutdown
drained rendering and tracked presentation work before destroying Vulkan and XDG objects.

The resize investigation first reproduced severe whole-window lag despite CPU frame work averaging
roughly 3--8 ms. Recreating a swapchain for every XDG configure supplied fresh images and bypassed
FIFO acquisition backpressure, queuing obsolete surface commits faster than the 74.971 Hz display
could consume them. The final path retains only the newest queued configure and limits Wayland
resize commits to one frame start per 16 ms. Its physically accepted trace rendered and recreated
198 generations, averaged 16.522 ms between callbacks (17.054 ms maximum), and averaged 9.374 ms per
resize frame (12.536 ms maximum), including 9.121 ms average swapchain recreation. This trace does
not establish other compositor, refresh-rate, display, GPU, or driver behavior.

### Wayland presentation-stall correction

Automated finite-frame Wayland runs of the triangle probe reproducibly froze after roughly five
presented frames on the KDE Plasma system above, while interactive input (resize, activation)
released a handful of frames at a time. This was previously attributed to KDE suspending FIFO
presentation for idle or locked sessions; that conclusion was wrong and is retracted. A
`WAYLAND_DEBUG` protocol capture showed the first five commits leaving at vblank pacing and all
client-side protocol activity stopping afterwards, `vkcube --wsi wayland --present_mode 2`
sustained 75 Hz on the same unlocked desktop under identical protocol machinery
(`wp_fifo_v1` barriers plus `wp_linux_drm_syncobj_v1` explicit sync), and disabling the
validation layer, the debug messenger, surface/swapchain maintenance1, server-side decorations,
and FIFO (via mailbox) individually did not affect the freeze.

The actual defect was in the probe's event pump: it polled the display descriptor and then called
the blocking `wl_display_dispatch` whenever the socket was readable. The NVIDIA driver runs its
own Wayland reader thread on the same connection, so the driver thread could consume the readable
data between the probe's `poll` and its dispatch, leaving `wl_display_dispatch` asleep until the
compositor happened to send another event — which input events did, explaining the
interaction-released frames. The pre-refactor revision stalled identically because it contained
the same pump. The pump now takes socket input through the libwayland thread-safe
`wl_display_prepare_read`/`wl_display_read_events`/`wl_display_cancel_read` protocol and only
ever dispatches already-queued events.

With the corrected pump and the validation layer enabled, on the same system (KWin 6.7.3,
XWayland 24.1.13, Nvidia driver 610.43.03, Rust 1.97.0, working tree based on
`8e62d02b537593eafd365c0d598780542f7538cf`): a 600-frame Wayland run completed in 8.3 s at the
74.971 Hz display rate with exit code zero, a 60-frame `--require-pipeline-cache-hits` run
completed with all four pipelines reporting application-cache hits, and a physically exercised
session rendered 1975 frames through drag resize, minimize/restore, maximize/restore, and
titlebar close, capturing 158 resize-trace samples (record + submit average 0.179 ms, image
acquisition average 0.005 ms) with no validation messages and a drained shutdown. This re-run
retires the pending Wayland dispatch-path regression check from the earlier revision of this
runbook.

### Acquired-frame non-presentation evidence

On 2026-07-17, a development tree based on revision
`86dbb462e32f311a4cef7e6c8fbe6b663235412d` added a deterministic acquired-frame
non-presentation path. It ran on the same physical native-Wayland RTX 3060 Ti system described
above with the Khronos validation layer enabled:

```sh
target/debug/mulciber-vulkan-triangle \
  --frames 120 --abandon-acquired-frame-once
MULCIBER_VULKAN_FORCE_SWAPCHAIN_FALLBACK=1 \
  target/debug/mulciber-vulkan-triangle \
  --frames 120 --abandon-acquired-frame-once
```

The default run acquired image zero with a dedicated fence and no image-available semaphore,
submitted no command buffer, queued no presentation, and returned the untouched image through
`vkReleaseSwapchainImagesKHR`. It then presented 120 later frames. The forced fallback run performed
the same unused acquisition without the maintenance extension, created a replacement swapchain with
the abandoned generation as `oldSwapchain`, retired that complete generation, and then presented
120 later frames. Both runs exercised the full startup workload, loaded application-cache hits for
all four pipelines, reported recovery after a later presentation, drained rendering and presentation
ownership, emitted no validation warning or error, and exited zero.

This establishes the two one-shot Vulkan recovery mechanisms on one Nvidia/Wayland tier. It is
development-tree evidence without a validation archive, and it does not establish repeated
non-presentation, non-Nvidia drivers, Windows behavior, abandonment during resize or suspension, or
a naturally maintenance-less adapter.

### Initial X11 presentation evidence

The X11 presentation implementation was exercised from an uncommitted working tree based on
revision `f332e15a4b5875b3f71004aeaf3cdf00245b1041`; it is therefore development evidence, not a
clean revision archive. The environment matched the Wayland presentation runs above (CachyOS,
Linux 7.1.3, KDE Plasma Wayland session, XWayland 24.1.13, Nvidia RTX 3060 Ti with driver
610.43.03, Rust 1.97.0), with the X11 path reaching the compositor through XWayland via
`--platform x11`.

Two automated finite runs completed with exit code zero and no Vulkan validation warnings or
errors: a 300-frame learning-mode run and a 60-frame `--require-pipeline-cache-hits` run in which
all four pipelines reported read-only application-cache hits. Both runs exercised the full
workload — BC1 direct sampling with exact readback, compute-written storage/indirect/image
resources, mip generation, 4x MSAA, shadow and post passes, GPU timestamps — and the
`VK_KHR_swapchain_maintenance1` presentation-fence retirement path, then drained rendering and
presentation work before destroying Vulkan and Xlib objects.

Both runs executed while the desktop session was locked (`loginctl` reported `LockedHint=yes`), so
the compositor consumed frames at a heavily throttled pace and the non-blocking acquire path
returned `VK_NOT_READY` for most iterations. This establishes that compositor-suspended
presentation retries without deadlock or validation noise, but it establishes nothing about
interactive frame pacing.

### X11 unlocked pacing and physical lifecycle evidence

On the same system with the session unlocked (KWin 6.7.3, XWayland 24.1.13, Nvidia driver
610.43.03, working tree based on `8e62d02b537593eafd365c0d598780542f7538cf`), a 600-frame
`--platform x11` run completed in 8.1 s at the 74.971 Hz display rate with exit code zero and no
validation messages. Physically exercised sessions confirmed drag resize, minimize/restore,
maximize/restore, and window-manager close through `WM_DELETE_WINDOW`, with resize traces
recording 187 swapchain generations (recreation average 7.7 ms) and drained shutdowns.

Physical interaction exposed two X11 window-system defects that automated runs cannot see. First,
the `XCreateSimpleWindow` solid background made the server clear the window to black on every
interactive resize step before the next frame arrived; the window now uses a `None` background
pixmap. Second, the window content froze at its old size for the whole drag and snapped on
release, because KWin only live-updates X11 windows during interactive resize for clients that
implement the `_NET_WM_SYNC_REQUEST` protocol; the module now creates an XSync counter, registers
the protocol, and reports each sync value on the pump following the frame that answered it. With
the counter gating each resize step, the Wayland-motivated 16 ms resize-commit pacing only
widened the stale-content window and is disabled on X11. The physically accepted final trace
rendered all 725 attempted resize frames across 716 swapchain generations, averaging 5.4 ms per
resize frame including 5.2 ms of swapchain recreation, and the user compared drag-resize behavior
favorably against `vkcube --wsi xlib` on the same desktop. Native Xorg, display changes, input,
multi-display, and broader hardware/driver coverage remain outstanding.

## Machine requirements

- Physical x86-64 Linux installation.
- A conformant Vulkan 1.3-or-newer loader and vendor or Mesa driver with dynamic rendering and
  synchronization2.
- `vulkaninfo` and the Khronos validation layer for the later presentation probes.
- Xlib client libraries for the X11 path.
- `libwayland-client` and a reachable compositor for the Wayland path.
- Rust 1.97 or the repository-pinned compatible toolchain.

Record the environment before running:

```sh
uname -a
rustc --version --verbose
vulkaninfo
printf 'XDG_SESSION_TYPE=%s\nDISPLAY=%s\nWAYLAND_DISPLAY=%s\n' \
  "$XDG_SESSION_TYPE" "$DISPLAY" "$WAYLAND_DISPLAY"
```

Preserve the complete `vulkaninfo` output. A summary alone can omit queue, format, feature, and
presentation facts needed for later comparisons.

## Structural preflight

From the repository root:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Both Linux native modules must compile and link in the same build. Passing this preflight does not
prove that either display server or Vulkan surface path works on the machine.

## X11 capability run

Run from an X11 session or an environment whose `DISPLAY` reaches an X server:

```sh
cargo run -p mulciber-vulkan-info -- --platform x11
cargo run -q -p mulciber-vulkan-info -- --platform x11 --json \
  | tee mulciber-vulkan-x11.json
jq -e '
  .schema_version == 1 and
  .backend == "vulkan" and
  .platform == "linux-x11" and
  (.adapters | length) > 0
' mulciber-vulkan-x11.json
```

The run must create a real `VK_KHR_xlib_surface`, enumerate surface support for every queue family,
and report formats, FIFO presentation, extents, usage flags, and explicit baseline failures.

## Wayland capability run

Run from a Wayland session with valid `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY`:

```sh
cargo run -p mulciber-vulkan-info -- --platform wayland
cargo run -q -p mulciber-vulkan-info -- --platform wayland --json \
  | tee mulciber-vulkan-wayland.json
jq -e '
  .schema_version == 1 and
  .backend == "vulkan" and
  .platform == "linux-wayland" and
  (.adapters | length) > 0
' mulciber-vulkan-wayland.json
```

The run must discover `wl_compositor`, create a live `wl_surface`, create a real
`VK_KHR_wayland_surface`, and report the same device and surface facts as the X11 path. This hidden
capability surface intentionally has no XDG-shell role; the triangle probe exercises XDG lifecycle
separately.

## Wayland presentation run

Run from a native Wayland session after installing the Khronos validation layer:

```sh
cargo run -p mulciber-vulkan-triangle
MULCIBER_VULKAN_RESIZE_TRACE=1 cargo run -p mulciber-vulkan-triangle
cargo run -p mulciber-vulkan-triangle -- \
  --abandon-acquired-frame-once --frames 120
MULCIBER_VULKAN_FORCE_SWAPCHAIN_FALLBACK=1 \
  cargo run -p mulciber-vulkan-triangle -- \
  --abandon-acquired-frame-once --frames 120
```

Confirm that server-side chrome and the rendered workload are visible. Physically exercise drag
resize, minimize/restore, maximize/restore, and titlebar close. Preserve the full trace and every
validation message. A passing run must remain responsive during resize, recreate all
extent-dependent attachments, continue rendering after restore, and drain both GPU and presentation
ownership during shutdown. These checks do not substitute for display-change, explicit zero-sized
suspension, input, multi-display, or broader compositor/hardware coverage.
The two finite non-presentation runs must each report exactly one untouched acquired image, later
presentation recovery, 120 submitted frames, and clean shutdown. Record whether the native extension
or forced generation-replacement path ran.

## X11 presentation run

Run with a reachable X server after installing the Khronos validation layer. From a Wayland
session, `DISPLAY` reaches XWayland; record that the run is XWayland-provided rather than native
Xorg:

```sh
cargo run -p mulciber-vulkan-triangle -- --platform x11
MULCIBER_VULKAN_RESIZE_TRACE=1 cargo run -p mulciber-vulkan-triangle -- --platform x11
```

Confirm that the window-manager chrome and the rendered workload are visible and that the session
is unlocked while pacing is measured. Physically exercise drag resize, minimize/restore,
maximize/restore, and window-manager close (which must arrive as `WM_DELETE_WINDOW`). Preserve the
full trace and every validation message. A passing run must remain responsive during resize,
recreate all extent-dependent attachments, continue rendering after restore, and drain both GPU
and presentation ownership during shutdown. Xlib routes fatal connection errors through its
process-exiting default handlers instead of recoverable returns; record any such exit verbatim.
These checks do not substitute for native Xorg, display-change, input, multi-display, or broader
compositor/hardware coverage.

## Success criteria

For each native path:

- The process exits successfully without panic, native display error, or Vulkan error.
- JSON parses without repair and identifies the requested platform.
- At least one adapter is reported, even if none satisfies Mulciber's baseline.
- Every adapter contains queue-family presentation facts and nonempty surface formats.
- FIFO presentation is present for any adapter reported as baseline compatible.
- The selected adapter and every rejection reason agree with the raw capability facts.

Do not treat WSL, XWayland-only execution, compilation, or successful JSON parsing as evidence for a
different native path. Record whether X11 is native or provided through XWayland, and run the
Wayland path explicitly rather than inferring it from the desktop session.

## Evidence to preserve

- Distribution, kernel, desktop environment, compositor/X server, and session type.
- GPU, driver, Vulkan loader, and Vulkan API versions.
- Full `vulkaninfo` output.
- Human-readable and JSON Mulciber reports for each exercised path.
- Exact Git revision and working-tree status.
- Any native display or Vulkan error verbatim.

Capability-report evidence alone does not establish swapchain rendering or lifecycle behavior. The
presentation evidence above establishes only the explicitly listed KDE Plasma runs: physical
Wayland and XWayland lifecycle interaction, unlocked pacing on both paths, and locked-session
retry behavior on X11. Native Xorg, display changes, input, multi-display, and other
compositor/driver/hardware combinations are still required.
