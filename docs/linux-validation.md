# Linux Vulkan capability validation runbook

The Linux capability probe has peer X11 and Wayland paths. Both share device/report collection while
retaining native window, loader, and Vulkan surface creation in separate modules.

## Current status

- X11 and Wayland compile, lint, link, and pass their shared report tests on x86-64 Ubuntu under
  WSL.
- WSLg can create both native client objects, but its Vulkan loader reports 1.3.275 and is rejected
  before Vulkan instance/surface creation because Mulciber requires Vulkan 1.4.
- No physical Linux Vulkan 1.4 capability result has been recorded yet.
- The Wayland path creates an unconfigured `wl_surface` only for Vulkan capability queries. It does
  not establish XDG-shell window, resize, input, presentation, or lifecycle behavior.

These implementation checks are not physical backend evidence and do not complete either roadmap
item.

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
capability surface intentionally has no XDG-shell role; the future Wayland presentation probe must
exercise XDG lifecycle separately.

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

Capability-report evidence does not establish swapchain rendering, resize, minimize, display
changes, frame pacing, input, or shutdown. Those remain requirements of the X11 and Wayland
presentation probes.
