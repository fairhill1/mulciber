# Implementation roadmap

Each milestone is a runnable vertical slice. Public abstraction work follows backend evidence rather
than preceding it.

Advancing between major milestones is governed by the [viability gates](viability-gates.md). Passing
more checkboxes is not sufficient if Zinc is converging on a less mature `wgpu`/`winit` substitute or
cannot be learned efficiently without pre-existing ecosystem knowledge.

## 0. Capability inventory

- [x] Establish the support and dependency contracts.
- [x] Query the default Metal device without a graphics abstraction dependency.
- [x] Emit a machine-readable Metal capability report.
- [ ] Emit equivalent Vulkan capability reports on Windows and Linux.
- [ ] Run the report with the macOS 26 / Metal 4 runtime and compare results.
- [x] Pin the Vulkan registry, headers, loader, validation, profiles, and SPIR-V toolchain revisions.

## 1. Native presentation probes

- [ ] AppKit window with a `CAMetalLayer` and Metal triangle (implemented, but physical resize,
  minimize/restore, maximize/zoom, display-change, and shutdown evidence has not been recorded).
- [ ] Win32 window with a Vulkan 1.4 swapchain and triangle (physically exercised on Windows 11 and
  an Nvidia RTX 3060 Ti; the window resizes smoothly, rendering remains functional, and driving
  redraw from `WM_SIZE` improved measured callback spacing from about 27 ms to 9 ms and looked
  noticeably better; presentation-fence retirement and the forced deferred fallback are physically
  exercised, while a naturally extension-less adapter, multi-display behavior, and the GTX
  1060-class baseline remain outstanding).
- [ ] Wayland XDG-shell window with a Vulkan 1.4 swapchain and triangle.
- [ ] X11 window with a Vulkan 1.4 swapchain and triangle.
- [x] Replace conventional device-idle swapchain retirement with tracked presentation completion
  using presentation fences when available and a deferred-retirement fallback.

Every probe must handle resize, zero-sized/minimized surfaces, VSync, acquire failure, and clean
shutdown with API validation enabled.

## 2. Representative rendering workload

Implement independently in Vulkan and Metal:

- Vertex, index, uniform, storage, and indirect buffers.
- Sampled, storage, depth, multisampled, and compressed textures.
- Uploads, readback, mip generation, and transient render targets.
- Graphics and compute pipelines with offline shader compilation.
- GPU timestamps, labels, and pipeline caching.
- One GPU-driven textured scene with depth, shadows, and post-processing.

Metal evidence completed so far:

- [x] Shared vertex and index buffers with indexed drawing.
- [x] Private sampled texture populated from a staging buffer through a blit command.
- [x] Linear filtering, repeating texture coordinates, and fragment texture sampling.
- [x] Resize-dependent private depth target with depth testing and writes.
- [x] Compute pipeline writing a private storage texture consumed by the render pipeline.
- [x] GPU-to-CPU texture readback with byte-for-byte startup validation.
- [x] Offline MSL-to-metallib build with an embedded runtime library.
- [x] Triple-buffered, CPU-updated uniforms with explicit in-flight slot reuse.
- [x] Compute-written private storage buffer with verified GPU-to-CPU readback.
- [x] Indexed-indirect drawing from a native Metal argument buffer.
- [x] Capability-checked BC1 source texture decompressed through compute.
- [x] Generated mip chain with exact 1x1 mip-tail readback validation.
- [x] Memoryless 4x MSAA color and depth attachments resolved into the drawable.
- [x] Reusable shadow depth, main MSAA, and fullscreen post-processing passes.
- [x] Debug labels and command-buffer GPU start/end timing.
- [x] Strict cold-generation and cross-process loading of a device-specific Metal binary archive.

## 3. Extract Zinc APIs

- [ ] Extract owned resource and synchronization types into `zinc-gpu`.
- [ ] Extract event, input, display, and lifecycle types into `zinc-platform`.
- [ ] Keep backend-specific capabilities reachable without leaking native object ownership.
- [ ] Establish baseline and optional capability conformance tests.

## 4. Recent GPU capabilities

Add each as an independent backend feature with a tested fallback:

- Bindless resource tables and descriptor indexing.
- Mesh shading.
- Hardware ray tracing.
- Sparse resources.
- GPU-generated command paths.
- HDR and presentation timing.
- Metal 4 command allocation, argument tables, barriers, and pipeline datasets.

## 5. Runtime

Create `zinc-runtime` only after platform and GPU lifecycle contracts stabilize. It will coordinate
the game loop, fixed and variable updates, input snapshots, frame pacing, jobs, suspension, and
device recovery.
