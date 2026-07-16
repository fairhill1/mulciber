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
`MTL_DEBUG_LAYER=1`.

On Windows, after installing a Vulkan 1.4 driver and the Khronos validation layer:

```sh
cargo run -p mulciber-vulkan-info
cargo run -q -p mulciber-vulkan-info -- --json
```

The capability probe creates a hidden Win32 surface and reports every Vulkan adapter, memory heaps,
queue families, core workload features and limits, device extensions, surface formats, present
modes, and explicit Mulciber Vulkan 1.4 baseline failures. The `--json` form emits the versioned report
used for cross-machine comparisons and adapter-tier evidence.

```sh
cargo run -p mulciber-vulkan-win32-triangle -- --frames 600
```

The probe uploads geometry and a checkerboard texture through temporary staging buffers into
device-local buffers and an RGBA8 image, then renders through GPU-written indexed-indirect drawing
with fragment texture sampling and capability-selected 4x multisampled color/depth attachments. The
scene resolves into an offscreen image that a fullscreen vignette pass samples into the swapchain;
three persistently mapped uniform frame slots provide aspect correction and time. A startup compute
dispatch writes a device-local storage buffer, the indirect draw command, and an RGBA8 storage image.
It generates the image's complete mip chain with synchronized GPU blits, verifies the base and 1x1
tail through host readback, then the fragment shader explicitly samples a generated mip. The probe
loads `vulkan-1.dll` dynamically and has no Rust package dependencies. Validation is required and
reported through `VK_EXT_debug_utils`. Colored debug-utils command regions identify the startup
compute dispatch and each frame's scene and post passes. When the selected queue exposes timestamp
bits, synchronization2 timestamp queries measure those same regions, account for counter
wraparound, and print fence-safe startup and shutdown timing summaries; zero-bit queues retain labels
and run without timing. See the
[Windows validation runbook](docs/windows-validation.md) before marking the slice complete.
