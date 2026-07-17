# Linux Vulkan capability and presentation validation runbook

The Linux capability probe has peer X11 and Wayland paths. Both share device/report collection while
retaining native window, loader, and Vulkan surface creation in separate modules.

## Current status

- X11 and Wayland compile, lint, link, and pass their shared report tests on x86-64 Linux.
- WSLg can create both native client objects, but its Vulkan loader reports 1.3.275 and is rejected
  before Vulkan instance/surface creation because Mulciber requires Vulkan 1.4.
- Native Wayland and X11-through-XWayland Vulkan 1.4 capability results were recorded on physical
  Linux on 2026-07-16. Native Xorg and broader driver/hardware coverage remain pending.
- One-shot acquired-frame non-presentation was physically exercised on native Wayland on
  2026-07-17 through both `VK_KHR_swapchain_maintenance1` image release and the forced
  base-swapchain generation-replacement path, followed by 120 presented recovery frames.
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
- A conformant Vulkan 1.4 loader and vendor or Mesa driver.
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
