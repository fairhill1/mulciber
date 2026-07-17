# Mulciber

Mulciber is a native game platform for Vulkan and Metal.

## Why Mulciber?

Mulciber is for Rust games that want native-engine control without maintaining separate graphics and
window-system stacks for every desktop platform. It combines direct Vulkan and Metal access with
game-focused platform lifecycle, exposes recent GPU capabilities without forcing them into a WebGPU
feature model, and keeps the shipped runtime small and auditable.

This is a deliberately narrower goal than `wgpu` plus `winit`, not a claim that those projects are
bad foundations. Mulciber trades their broad portability and maturity for native API reach, coordinated
GPU/platform/runtime design, and a first-class support contract limited to modern Windows, Linux,
and Apple-silicon macOS machines.

Minimal dependencies are a means to predictable ownership, policy, and maintenance—not the reason
for the project by themselves. Read [the project vision](docs/vision.md) for the intended user,
non-goals, and the criteria Mulciber must meet to justify its existence.

The project is intentionally beginning with small native probes instead of a predesigned graphics
abstraction. Those probes will establish the real platform contracts from which `mulciber` and
`mulciber-platform` are derived.

The native evidence now permits a narrow experimental extraction, but not a stable API or first-class
support claim. The [API extraction and comparison plan](docs/api-extraction-plan.md) defines the first
unified slice, the design decisions it must settle, and the direct-native, single-backend Rust,
`wgpu`/`winit`, SDL3 GPU, Vulkano, and scoped raylib comparisons it must survive. Portability receives
no credit in the Metal-only and Vulkan-only viability evaluations.

The first extracted boundary moves peer AppKit, Win32, Wayland, and X11 application/window, event,
and drawable-metrics ownership into `mulciber-platform`; the full Metal and Vulkan probes are its
executable consumers. The Win32 extraction cross-compiles from Linux and awaits a batched physical
Windows validation run before the platform-spine milestone or a new crate release is claimed. The
[experimental platform contract](docs/api-platform-contract.md) records the decisions and remaining
peer-platform work. The [first graphics-slice flow](docs/api-first-slice.md) reviews the intended
outside-in application experience against both native call sequences without committing graphics
type names. `mulciber`'s graphics API remains empty.

## Direction

- Vulkan 1.4 on Windows and Linux.
- Metal 3 on Apple silicon as the compatibility baseline.
- Metal 4 as an SDK- and capability-gated path.
- Native Win32, AppKit, Wayland, and X11 platform implementations.
- No `wgpu`, `winit`, or Direct3D dependency.
- Modern features such as mesh shading, bindless resources, ray tracing, and sparse resources are
  independent capabilities rather than a single linear hardware tier.

See [the project vision](docs/vision.md), [viability gates](docs/viability-gates.md),
[support contract](docs/support-contract.md), [architecture decisions](docs/architecture.md),
[API extraction and comparison plan](docs/api-extraction-plan.md),
[pinned references](docs/references.md), and [implementation roadmap](docs/roadmap.md).

## Current probes

On macOS:

```sh
cargo run -p mulciber-metal-info
```

This queries Metal directly through the Objective-C runtime and has no Rust package dependencies.
Pass `--json` to emit the versioned machine-readable report used for cross-machine comparisons:

```sh
cargo run -q -p mulciber-metal-info -- --json
```

Run the native AppKit and Metal presentation/resource probe:

```sh
cargo run -p mulciber-metal-triangle
```

It uploads a capability-checked BC1 texture through a staging buffer, decompresses it into a private
mipmapped storage texture with compute, and verifies both the base level and generated mip tail
through padded GPU-to-CPU readback. Rendering uses indexed-indirect drawing, depth testing, and
memoryless 4x MSAA color/depth attachments resolved into the drawable. Compute also writes a private
storage buffer whose copied-back contents are verified. Three per-frame uniform buffers animate the
geometry and are updated only after their previous command buffers complete. All retained in-flight
command buffers are drained and released even when rendering or GPU completion fails.

Each frame encodes a reusable shadow-depth pass, the memoryless MSAA main pass resolved into a
shader-readable scene texture, and a fullscreen post-processing pass into the drawable. Major GPU
objects and encoders carry debug labels, and completed command buffers provide aggregate GPU frame
timing.

The build script invokes Xcode's `metal` and `metallib` tools and embeds the result. The running probe
loads that library directly and never invokes a shader compiler. This adds an SDK build requirement,
not a shipped package dependency.

The first run generates `target/mulciber-metal-pipelines.metalarc` from all three render pipelines and
the compute pipeline. Later runs load that device-specific Metal binary archive and create every
pipeline with `MTLPipelineOptionFailOnBinaryArchiveMiss`, proving that the serialized entries are
actually used. Pass `--binary-archive PATH` to select a different artifact or
`--rebuild-binary-archive` after changing shaders, pipeline descriptors, the OS, or the GPU.

This runtime generation path is probe machinery for producing and verifying a development artifact,
not the intended shipping cache policy. Apple's SDK recommends generating binary archives during
development and shipping them as assets; Metal maintains its own corruption-resilient application
cache for pipelines compiled at runtime.

For a finite validation run, pass `--frames N`. Enable Apple's validation layer with
`MTL_DEBUG_LAYER=1`. See the [macOS validation runbook](docs/macos-validation.md) before marking
the platform slice complete; initial physical AppKit lifecycle evidence is recorded there.

Pass `--abandon-acquired-frame-once` with a finite run to acquire one drawable, access its
texture, and intentionally submit and present nothing for it. The per-frame autorelease pool is
then drained, and the run fails unless later rendering submits successfully. This makes the
otherwise exceptional frame-abandonment path observable without weakening normal validation.

On Windows, after installing a Vulkan 1.4 driver and the Khronos validation layer:

```sh
cargo run -p mulciber-vulkan-info
cargo run -q -p mulciber-vulkan-info -- --json
```

The capability probe creates a hidden Win32 surface and reports every Vulkan adapter, memory heaps,
queue families, core workload features and limits, device extensions, surface formats, present
modes, and explicit Mulciber Vulkan 1.4 baseline failures. The `--json` form emits the versioned report
used for cross-machine comparisons and adapter-tier evidence.

On Linux, the same report has explicit X11 and Wayland paths:

```sh
cargo run -p mulciber-vulkan-info -- --platform x11
cargo run -q -p mulciber-vulkan-info -- --platform x11 --json
cargo run -p mulciber-vulkan-info -- --platform wayland
cargo run -q -p mulciber-vulkan-info -- --platform wayland --json
```

The X11 path creates a hidden Xlib window. The Wayland capability path discovers `wl_compositor`
and creates an unconfigured `wl_surface`, which is sufficient for Vulkan surface capability queries.
Both report paths are implemented and physically exercised on a Vulkan 1.4 Nvidia system: Wayland
ran natively under KDE Plasma and X11 ran through XWayland. The separate triangle probe consumes
peer Wayland and X11 modules from `mulciber-platform`, selected at runtime: an XDG-shell Wayland
window with server-side decorations and paced resize commits, and an Xlib window with
`WM_DELETE_WINDOW` handling,
structure-notification resize tracking, and `_NET_WM_SYNC_REQUEST` sync-gated interactive resize.
Physical presentation, pacing, and lifecycle evidence for both paths is recorded in the
[Linux validation runbook](docs/linux-validation.md). Native Xorg coverage, display changes,
input, and broader Linux hardware/driver evidence remain pending.

```sh
cargo run -p mulciber-vulkan-triangle -- --frames 600
cargo run -p mulciber-vulkan-triangle -- --platform x11 --frames 600
```

Without `--platform`, the Linux triangle probe selects Wayland when `WAYLAND_DISPLAY` is set and
X11 when only `DISPLAY` is set.

Exercise the acquired-but-unsubmitted frame path separately:

```sh
cargo run -p mulciber-vulkan-triangle -- \
  --abandon-acquired-frame-once --frames 120
```

On adapters with `VK_KHR_swapchain_maintenance1`, the probe acquires through a dedicated fence and
returns the untouched image with `vkReleaseSwapchainImagesKHR`. The base-swapchain compatibility path
retires the acquired image's complete swapchain generation instead. The run fails unless a later
frame is submitted, presented, and shutdown completes without validation messages. Set
`MULCIBER_VULKAN_FORCE_SWAPCHAIN_FALLBACK=1` to exercise the compatibility path on a maintenance-
capable adapter.

The probe uploads geometry and a deterministic 8x8 checkerboard through temporary staging buffers
into device-local buffers and either a directly sampled BC1 image or an RGBA8 fallback image. It
copies the selected texture back and verifies every encoded or expanded byte before rendering through
GPU-written indexed-indirect drawing with fragment texture sampling and capability-selected 4x
multisampled color/depth attachments. The
scene resolves into an offscreen image that a fullscreen vignette pass samples into the swapchain;
before that scene pass, a fixed-resolution depth-only pass renders an offset light-space projection
into a sampled shadow map, and the fragment shader applies a filtered depth comparison. Three
persistently mapped uniform frame slots provide aspect correction and time. A startup compute
dispatch writes a device-local storage buffer, the indirect draw command, and an RGBA8 storage image.
It generates the image's complete mip chain with synchronized GPU blits, verifies the base and 1x1
tail through host readback, then the fragment shader explicitly samples a generated mip. The probe
loads the platform Vulkan loader dynamically (`vulkan-1.dll` on Windows or `libvulkan.so.1` on
Linux); its Linux window integration depends only on the local `mulciber-platform` crate. Validation
is required and reported through `VK_EXT_debug_utils`. Colored debug-utils command regions identify
the startup compute dispatch and each frame's shadow, scene, and post passes. When the selected queue exposes
timestamp bits, synchronization2 timestamp queries measure those same regions, account for counter
wraparound, and print fence-safe startup and shutdown timing summaries; zero-bit queues retain labels
and run without timing.

Every compute and graphics pipeline uses one device-specific Vulkan pipeline cache by default. The
probe validates the raw cache header against the selected adapter, reports whole-pipeline creation
feedback, and atomically persists learning-mode updates under `target`. Use `--pipeline-cache PATH`
to select an artifact, `--rebuild-pipeline-cache` for a cold learning run,
`--require-pipeline-cache-hits` for read-only cross-process hit proof, or
`--disable-pipeline-cache` for the correctness control. Strict mode also forbids compilation when
the adapter exposes pipeline creation cache control. See the
[Windows validation runbook](docs/windows-validation.md) or
[Linux validation runbook](docs/linux-validation.md) before marking a platform slice complete.

Texture selection is independent from the Vulkan baseline. Unset
`MULCIBER_VULKAN_TEXTURE_MODE` (or set it to `auto`) to prefer BC1 only when the core feature and
sampled/transfer format roles are all available. Set it to `bc1` for an actionable required-mode
failure or `rgba8` to force the physically validated fallback.
