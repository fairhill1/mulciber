# Linux Vulkan capability and presentation validation runbook

The Linux capability probe has peer X11 and Wayland paths. Both share device/report collection while
retaining native window, loader, and Vulkan surface creation in separate modules.

## Current status

- X11 and Wayland compile, lint, link, and pass their shared report tests on x86-64 Linux.
- WSLg can create both native client objects, but its Vulkan loader reports 1.3.275 and is rejected
  before Vulkan instance/surface creation because Mulciber requires Vulkan 1.4.
- Native Wayland and X11-through-XWayland Vulkan 1.4 capability results were recorded on physical
  Linux on 2026-07-16. Native Xorg and broader driver/hardware coverage remain pending.
- The capability report's Wayland path creates an unconfigured `wl_surface` only for Vulkan queries.
- The peer Vulkan triangle probe has runtime-selected Wayland and X11 platform modules behind a
  `--platform` flag with `WAYLAND_DISPLAY`/`DISPLAY` autodetection. The Wayland module creates a
  real XDG-shell toplevel, requests server-side decorations, presents through
  `VK_KHR_wayland_surface`, coalesces configure events, and paces resize swapchain commits.
  Initial physical presentation and lifecycle evidence was recorded on the same KDE Plasma system
  before the runtime-dispatch refactor that added the X11 peer; a Wayland re-run through the
  refactored dispatch path is pending because KDE suspends FIFO presentation entirely on an
  idle or locked session, which repeatedly prevented remote completion. The physically validated
  pre-refactor revision stalls identically under the same idle conditions (including against a
  nested `kwin_wayland --virtual` instance), so the stall is compositor frame starvation rather
  than a dispatch-refactor regression.
- The X11 triangle module creates a real Xlib toplevel with `WM_DELETE_WINDOW` registration and
  structure-notification tracking, waits for the initial `MapNotify`, and presents through
  `VK_KHR_xlib_surface`. Initial automated presentation evidence was recorded through XWayland on
  the same system; physical lifecycle interaction and native Xorg coverage remain pending.

The capability results complete the two capability-inventory ports. The Wayland presentation item
remains incomplete pending display-change, explicit zero-sized suspension, input, and broader
compositor/hardware evidence. The X11 presentation item remains incomplete pending physical
resize/minimize/close interaction, unlocked-session frame pacing, display changes, input, native
Xorg, and broader hardware evidence.

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
interactive frame pacing. No window was physically resized, minimized, or closed; shutdown came
from the `--frames` limit. Physical lifecycle interaction, unlocked pacing, display changes,
input, multi-display, native Xorg, and broader hardware/driver coverage all remain outstanding.

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
```

Confirm that server-side chrome and the rendered workload are visible. Physically exercise drag
resize, minimize/restore, maximize/restore, and titlebar close. Preserve the full trace and every
validation message. A passing run must remain responsive during resize, recreate all
extent-dependent attachments, continue rendering after restore, and drain both GPU and presentation
ownership during shutdown. These checks do not substitute for display-change, explicit zero-sized
suspension, input, multi-display, or broader compositor/hardware coverage.

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
initial Wayland presentation evidence above establishes only the explicitly listed KDE Plasma run,
and the initial X11 presentation evidence establishes only automated locked-session XWayland runs;
physical X11 lifecycle interaction, native Xorg, and the remaining Wayland cases are still
required.
