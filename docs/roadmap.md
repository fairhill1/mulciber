# Implementation roadmap

Each milestone is a runnable vertical slice. Public abstraction work follows backend evidence rather
than preceding it. Once the extraction-entry evidence is established, an unstable public slice may be
built to test its design and value; stable support claims still require the applicable viability gates.

Advancing between major milestones is governed by the [viability gates](viability-gates.md). Passing
more checkboxes is not sufficient if Mulciber is converging on a less mature `wgpu`/`winit` substitute or
cannot be learned efficiently without pre-existing ecosystem knowledge.

## 0. Capability inventory

- [x] Establish the support and dependency contracts.
- [x] Query the default Metal device without a graphics abstraction dependency.
- [x] Emit a machine-readable Metal capability report.
- [x] Emit a machine-readable Vulkan capability report with real Win32 presentation data.
- [x] Port the Vulkan capability report to X11 on Linux (physically exercised through XWayland on
  KDE Plasma with Vulkan 1.4; native Xorg hardware coverage remains pending).
- [x] Port the Vulkan capability report to Wayland on Linux (physically exercised natively on KDE
  Plasma with a compositor-discovered `wl_surface` and Vulkan 1.4).
- [x] Run the report with the macOS 26 / Metal 4 runtime and compare results (physically
  exercised on an Apple M5 running macOS 26.5.2 with runtime-detected Metal 4 symbols all
  present, against an Apple M2 running macOS 15.7.7 with all absent; recorded in the macOS
  validation runbook).
- [x] Pin the Vulkan registry, headers, loader, validation, profiles, and SPIR-V toolchain revisions.

## 1. Native presentation probes

- [ ] AppKit window with a `CAMetalLayer` and Metal triangle (implemented and physically
  exercised on an Apple M2 running macOS 15.7.7 with Apple's validation layer: continuous drag
  resize including very small sizes, minimize/restore, zoom/restore, full occlusion/reveal, and
  titlebar-close shutdown all completed cleanly with validation-clean output; a separate automated
  path acquired and abandoned one drawable before submission, then recovered for 120 submitted
  frames and clean shutdown; display-change, multi-display, differing backing scale factors,
  input, the macOS 26 / Metal 4 runtime, and broader Apple-silicon hardware evidence remain
  outstanding).
- [ ] Win32 window with a Vulkan 1.4 swapchain and triangle (physically exercised on Windows 11 and
  an Nvidia RTX 3060 Ti; the window resizes smoothly, rendering remains functional, and driving
  redraw from `WM_SIZE` improved measured callback spacing from about 27 ms to 9 ms and looked
  noticeably better; presentation-fence retirement and the forced deferred fallback are physically
  exercised, while a naturally extension-less adapter, multi-display behavior, and the GTX
  1060-class baseline remain outstanding).
- [ ] Wayland XDG-shell window with a Vulkan 1.4 swapchain and triangle (implemented and physically
  exercised on KDE Plasma/Wayland with server-side decorations, validation-clean rendering,
  responsive paced resize, minimize/restore, maximize/restore, titlebar close, and clean Vulkan/XDG
  shutdown; display-change, explicit zero-sized suspension, input, and broader compositor/hardware
  evidence remain outstanding).
- [ ] X11 window with a Vulkan 1.4 swapchain and triangle (implemented as a runtime-selected peer
  of the Wayland module with `WM_DELETE_WINDOW`, structure-notification, and
  `_NET_WM_SYNC_REQUEST` interactive-resize handling; physically exercised on KDE Plasma through
  XWayland with validation-clean unlocked 75 Hz pacing, live drag resize, minimize/restore,
  maximize/restore, window-manager close, and clean shutdown; display changes, input,
  multi-display, and native Xorg coverage remain outstanding).
- [x] Replace conventional device-idle swapchain retirement with tracked presentation completion
  using presentation fences when available and a deferred-retirement fallback.
- [x] Prove one-shot Vulkan acquired-frame non-presentation through
  `vkReleaseSwapchainImagesKHR` when swapchain maintenance is enabled and whole-generation
  replacement on the base-swapchain fallback, followed by 120 presented recovery frames and clean
  shutdown (physically exercised on native Wayland on the current Nvidia tier, with the compatibility
  path forced).

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

Vulkan evidence completed so far:

- [x] Device-local vertex and index buffers populated through host-visible staging uploads, with
  indexed drawing (physically exercised on Windows 11 / RTX 3060 Ti with validation enabled).
- [x] Device-local RGBA8 sampled texture populated through a host-visible staging upload, explicit
  layout transitions, a combined image sampler descriptor, and fragment sampling (same machine).
- [x] Optional device-local BC1 checkerboard selected from the core feature plus exact sampled and
  transfer format roles, uploaded as four compressed blocks, copied back byte-for-byte, and sampled
  directly; required BC1 and forced RGBA8 paths are physically validated at native 4x, forced 1x,
  and resize while retaining strict pipeline-cache hits (same machine).
- [x] Capability-selected, resize-dependent device-local depth attachment with explicit layout
  transitions, depth testing/writes, and swapchain-retirement ownership (same machine).
- [x] Three persistently mapped, host-coherent uniform-buffer frame slots with matching descriptor
  sets, aspect-correct transforms, and shader-visible elapsed time (same machine).
- [x] Compute-written device-local storage buffer with explicit compute-to-copy and copy-to-host
  barriers plus exact GPU-to-CPU readback verification (same machine).
- [x] Compute-written device-local indexed-indirect command synchronized for transfer and
  indirect-command reads, verified through exact readback, and consumed by
  `vkCmdDrawIndexedIndirect` (same machine).
- [x] Compute-written device-local RGBA8 storage image with capability checks, exact texel
  readback, explicit compute-to-copy-to-fragment transitions, and fragment sampling (same machine).
- [x] Complete 8x8-to-1x1 storage-image mip chain generated through synchronized nearest blits,
  with exact 1x1 tail readback and explicit generated-mip fragment sampling (same machine).
- [x] Capability-selected 4x multisampled transient color/depth attachments resolved into the
  swapchain, with a physically validated 1x fallback and resize-retirement ownership (same machine).
- [x] Resize-dependent offscreen scene color target consumed by a second dynamic-rendering fullscreen
  vignette pass, with explicit color-write-to-sampled-read synchronization (same machine).
- [x] Capability-checked timestamp query pool with valid-bit wrap handling, fence-safe startup
  compute and per-frame shadow/scene/post timing, plus colored debug-utils labels for the same
  command regions (same machine).
- [x] Persistent 1024x1024 sampled depth map rendered by a depth-only light-space pass, synchronized
  for filtered fragment sampling by the main scene, and measured as its own diagnostic region (same
  machine).
- [x] Device-specific raw Vulkan pipeline cache shared by compute, shadow, native/forced-1x scene,
  and post pipelines, with header preflight, per-pipeline application-hit feedback, optional
  compile prohibition, atomic learning-mode persistence, strict read-only cross-process proof,
  corruption recovery, and a physically validated no-cache correctness control (same machine).

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

## 3. Extract and test the first Mulciber API slice

Follow the [API extraction and comparison plan](api-extraction-plan.md). Gate 1's remaining physical
coverage continues in parallel; this milestone creates an explicitly unstable experiment, not a
first-class support claim.

- [x] Define the experimental-extraction threshold, unresolved design decisions, comparison targets,
  tasks, measurements, single-backend scoring, and stop conditions.
- [x] Write the complete application-facing flow before committing to public type names, covering
  window creation, capability requests, device/surface creation, rendering, resize/suspension, frame
  completion, and fallible shutdown.
- [ ] Decide and record main-thread/event-loop ownership, object topology, surface generations, frame
  presentation/non-presentation, resource-use vocabulary, error recovery, and safe native reach.
- [x] Extract the minimal event, window, display, and lifecycle spine into `mulciber-platform`, backed
  by peer AppKit, Win32, Wayland, and X11 modules.
- [ ] Extract owned device, queue, buffer, texture, pipeline, command, synchronization, and
  presentation types into `mulciber` with Metal and Vulkan implementations.
- [ ] Build the same textured depth-tested resize-aware example through both backends without
  ordinary backend branches or application `unsafe`.
- [ ] Establish baseline, optional-capability, invalid-usage, surface-generation, frame-abandonment,
  and shutdown conformance tests.
- [ ] Prove that a Metal-only and Vulkan-only build neither links nor initializes the unused backend
  and does not add portability-only dispatch to the ordinary frame path.
- [ ] Implement and preserve the pinned direct-native, practical single-backend Rust, `wgpu`/`winit`,
  SDL3 GPU, Vulkano, and scoped raylib comparisons.
- [ ] Record the Gate 2 pass, redesign, narrow, or stop decision from correctness, ergonomics,
  learnability, control, cost, performance, and single-backend results.
- [ ] Keep backend-specific capabilities reachable without leaking native object ownership or
  bypassing presentation-retirement tracking.

Initial extraction progress: AppKit, Win32, Wayland, and X11 application/window/event paths, logical
and physical sizing, window metric revisions, lifecycle/redraw events, and borrowed opaque graphics
targets now live in `mulciber-platform` and drive the full Metal and Vulkan probes. AppKit supplies
backing scale; Linux and the initial Win32 extraction intentionally report `1.0` pending
scale/display-change evidence. Win32 cross-compiles and lints from Linux, including synchronous
redraw delivery inside its nested sizing loop. The extracted path passed the automated Windows matrix
and physical live-resize/lifecycle validation on Windows 11 / RTX 3060 Ti at revision `044ae86`,
completing the initial peer platform-spine evidence without broadening the support claim. See the
[experimental platform contract](api-platform-contract.md).

Do not stabilize names merely because both backends compile. Stable claims wait for Gate 1 completion
and a successful Gate 2 decision.

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

Create `mulciber-runtime` only after platform and GPU lifecycle contracts stabilize. It will coordinate
the game loop, fixed and variable updates, input snapshots, frame pacing, jobs, suspension, and
device recovery.
