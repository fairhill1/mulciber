# Cross-backend contract ledger

This ledger records the game-facing requirements demonstrated by Mulciber's native probes before
those requirements become public API. It is evidence for later design work, not a proposed API and
not a promise that Metal and Vulkan should expose identical operations.

The [API extraction and comparison plan](api-extraction-plan.md) defines the narrow unstable slice now
allowed by this evidence, the decisions it must settle, and the comparisons required before that slice
is treated as a coherent supported contract. Questions below remain open until the extraction records
and implements an answer through both native backends.

The ledger uses three evidence states:

- **Established**: implemented and physically exercised with native validation where available.
- **Partial**: implemented or exercised on only part of the intended support contract.
- **Pending**: not yet demonstrated; no shared contract should be inferred from it.

Unless a row says otherwise, current Vulkan physical evidence is Windows 11 on an Nvidia RTX 3060
Ti, and current Metal evidence comes from the Apple-silicon probe described in the roadmap. Platform
and hardware breadth remain governed by the viability gates.

## Design rules supported by the probes

1. **Expose game intent, preserve native differences.** Shared types should describe resources,
   work, dependencies, and lifecycle states that games actually need. They should not reproduce a
   native API merely because both backends can be made to resemble it.
2. **Capabilities are independent facts.** Format support, sample counts, memory properties,
   timestamp support, presentation facilities, and advanced GPU features must not collapse into one
   hardware tier.
3. **Ownership includes asynchronous use.** Rust ownership of a wrapper is insufficient by itself;
   Mulciber must also know which queue, command buffer, presentation operation, or frame slot can
   still access the native object.
4. **Correct synchronization is the natural path.** Ordinary portable code should express dataflow
   and pass dependencies without spelling Vulkan stage/access masks or relying on undocumented
   Metal hazard behavior. Backend-specific synchronization control can remain an explicit advanced
   boundary.
5. **Presentation is a lifecycle, not an image factory.** Acquire, zero-sized suspension, resize,
   presentation completion, retirement, and shutdown belong to one coordinated contract.
6. **Fallbacks are observable policy.** A capability-selected fallback such as 1x rendering in
   place of 4x MSAA is part of device negotiation and diagnostics, not an invisible backend choice.
7. **Evidence precedes convenience.** Rows with pending evidence remain design questions even when
   a plausible abstraction seems obvious.

## Device and capability contract

| Game-facing need | Metal evidence | Vulkan evidence | Candidate shared invariant | Native boundary or missing evidence |
| --- | --- | --- | --- | --- |
| Enumerate usable devices and reject an unsupported baseline | The capability probe reports the default device, GPU families, memory facts, limits, and selected advanced capabilities. **Partial**: one default-device path and incomplete Metal 4 evidence. | The Win32 capability probe reports every adapter, memory heaps, queue families, workload features and limits, device extensions, surface formats, present modes, and explicit baseline failures. **Established** on the current Windows tier. Peer X11 and Wayland reports physically selected the same baseline-compatible RTX 3060 Ti on Linux with Vulkan 1.4; Wayland was native under KDE Plasma and X11 ran through XWayland. **Partial** pending native Xorg and broader Linux driver/hardware evidence. | Device selection returns structured facts plus explicit rejection reasons. Required capabilities reject startup; optional capabilities select a path. | Complete Wayland and X11 swapchain/lifecycle evidence, native Xorg coverage, Metal multi-device policy, macOS 26/Metal 4, and the Windows baseline hardware tier remain pending. |
| Negotiate features without a linear quality tier | Metal checks texture compression, sample count, storage behavior, and other selectors independently. | Vulkan checks API version, required core features, extensions, queue/present support, formats, memory types, and shared color/depth sample counts independently. | Capabilities remain independently queryable. Profiles may be convenience predicates, never the source of truth. | The eventual stability and naming of capability records remain unresolved. |
| Explain the selected execution path | Probe output records selected Metal facilities and archive behavior. | Probe output identifies the adapter, depth format, sample path, texture compression path and exact BC1 capability facts, pipeline-cache mode, and swapchain-retirement mechanism. | Selection and fallback decisions are available to application diagnostics without requiring native handles. | A stable diagnostic/event format is pending public API work. |

## Resource contract

| Game-facing need | Metal evidence | Vulkan evidence | Candidate shared invariant | Native boundary or missing evidence |
| --- | --- | --- | --- | --- |
| Own a buffer until all GPU use is complete | Objective-C objects are retained and released explicitly; in-flight command buffers keep frame resources alive until completion. | Buffers own separate device memory and are destroyed after fences or orderly shutdown establish completion. | A resource is an owned device child whose destruction is deferred until every recorded GPU use is complete. Dropping the Rust value must not race the GPU. | Native object export and externally owned memory are pending escape-hatch design. |
| Select CPU/GPU placement without exposing backend heaps verbatim | Shared staging/readback/uniform buffers and private GPU resources demonstrate distinct storage modes. | Host-visible coherent staging/readback/uniform memory and device-local resource memory are selected from compatible memory types. | Public placement expresses intent such as upload, readback, frequently CPU-updated, or GPU-preferred. The backend chooses a legal native placement and reports unsupported combinations. | Explicit heaps, aliasing, residency, and budget policy require representative allocator evidence. |
| Upload immutable or infrequently changed data | Staging buffers feed private buffers and textures through blit commands. | Temporary host-visible staging buffers feed device-local vertex, index, and texture resources through transfer commands and explicit dependencies. | Upload completion is represented by owned work or an upload context; destination use cannot begin before the dependency is satisfied. | Batching, asynchronous transfer queues, and upload allocation policy remain pending. |
| Update per-frame CPU data safely | Three shared uniform buffers are reused only after their retained command buffers complete. | Three persistently mapped host-coherent uniform slots are advanced after frame-fence completion and bound through per-slot descriptors. | Mutable frame data belongs to a slot that cannot be rewritten until its previous GPU use completes. Slot reuse is a lifecycle guarantee, not an application timing convention. | Non-coherent memory and multiple queues require more evidence. |
| Read GPU results back and verify them | Buffer and padded texture readbacks are checked after command-buffer completion. | Compute storage buffers, indirect commands, storage-image base data, the mip tail, and selected BC1/RGBA8 sampled-texture payloads are copied into host-visible memory after explicit dependencies and checked exactly. | Readback exposes completion separately from mapped bytes. CPU access is legal only after completion and any backend-required visibility operation. | Row-pitch normalization, non-coherent invalidation, large asynchronous readbacks, and failure recovery remain pending. |
| Create sampled, storage, depth, multisampled, and transient textures | Private sampled/storage/depth textures, memoryless MSAA attachments, a compressed source texture, mip levels, and shader-readable scene targets are exercised. | Device-local sampled/storage/depth images, multisampled attachments, mip levels, and an offscreen shader-readable scene target are exercised. A fixed BC1 image is enabled only when the core feature and every used optimal-tiling role are present; its four blocks round-trip exactly and are sampled directly, while forced RGBA8 proves the fallback. **Established** on the current Windows tier. | A texture descriptor states dimensions, format requirements, mip/sample counts, and allowed roles. Creation either returns a resource satisfying every requested role or an actionable capability error. | Cross-backend format taxonomy, broader BC families/shapes/mips/filtering, and container/transcode policy remain pending. Metal memoryless storage is a backend optimization unless a portable transient contract proves useful. |
| Address subresources and views | Metal operations select mip levels, slices, and texture usages; render descriptors bind the relevant texture objects. | Image views and barriers identify aspect and mip ranges explicitly; generated mips transition independently. | Texture subresources are explicit wherever uploads, copies, pass attachments, or dependencies can target less than the whole resource. | The degree to which public views should expose native reinterpretation rules is unresolved. |
| Generate and consume mip chains | Metal's blit encoder generates a private mip chain after compute decompression, with exact base and tail readback. | Vulkan generates the storage-image chain with synchronized blits, verifies its tail, and explicitly samples a generated mip. | Mip generation is scheduled GPU work with declared source/destination usage and completion; later sampling depends on that work. | Filtering and format restrictions differ and must appear as capabilities or operation errors. |
| Bind resources to shaders | Metal binds buffers, textures, and samplers by native argument indices. | Vulkan descriptor set layouts, pools, sets, and updates bind uniform buffers and sampled/storage resources. | Pipeline reflection should validate a stable shader binding contract offline; runtime binding should not expose descriptor-pool or Objective-C indexing machinery in ordinary code. | The shader language, reflection format, bindless model, and native escape hatch are pending dedicated evidence. |

## Work and synchronization contract

| Game-facing need | Metal evidence | Vulkan evidence | Candidate shared invariant | Native boundary or missing evidence |
| --- | --- | --- | --- | --- |
| Record compute, render, copy, and presentation work | Command buffers contain compute, blit, shadow, main, and post-process encoders before drawable presentation. | Command buffers contain transfer/compute work or dynamic-rendering shadow, scene, and post passes before queue submission and swapchain presentation. | Work is recorded into ordered command scopes. Pass boundaries and resource roles are explicit enough for validation and backend synchronization. | Whether copy work shares the same public encoder model as render/compute work remains a design question. |
| Express dependencies between uses | Metal command-encoder ordering and resource storage modes establish the demonstrated dependencies. | Synchronization2 barriers encode stage, access, layout, and subresource transitions for uploads, compute, rendering, copies, sampling, and presentation. | Portable code declares or implies producer/consumer uses; Mulciber derives the backend dependency. Invalid or ambiguous use is rejected with the resource and conflicting roles identified. | Advanced explicit barriers may be exposed separately, but must not weaken safety of ordinary resources. Cross-queue ownership is pending. |
| Run compute and indirect GPU work | Compute writes a private storage buffer; rendering separately consumes a native indexed-indirect argument buffer. | Compute writes device-local storage, an indexed-indirect command, and a storage image; barriers make each output legal for its next consumer. | Indirect and storage outputs remain ordinary owned resources whose declared next use determines the required dependency. GPU-written commands require explicit capability and bounds validation. | Metal does not yet demonstrate a compute-written indirect command. Multi-draw, count buffers, argument/descriptor generation, and bindless GPU-driven paths belong to Gate 4. |
| Compile and create pipelines offline | MSL is built into an embedded metallib; runtime creation consumes the library. | Checked-in SPIR-V is loaded into shader modules; no runtime shader compiler is shipped. | Shipping runtime accepts precompiled backend artifacts plus deterministic reflection metadata. Shader compiler machinery remains offline. | Authoring language and cross-backend compilation pipeline remain pending evaluation. |
| Reuse pipeline compilation artifacts | A device-specific Metal binary archive is generated, serialized, reloaded cross-process, and required to produce strict archive hits. **Established** for Metal. | One raw device-specific cache is shared by compute, shadow, scene, and post pipelines. Version-one header preflight checks vendor, device, and UUID; whole-pipeline feedback proves application-cache hits; optional cache control forbids compilation in strict mode; learning serialization uses flushed sibling-temporary replacement; and strict native 4x, forced 1x, resize, corruption recovery, and no-cache correctness paths are physically exercised. **Established** on the current Windows tier. | Cache artifacts are backend-opaque, compatibility-scoped, replaceable performance data. Learning may expand them; validation can require read-only hits; rendering correctness never depends on their presence. | Metal binary archives and Vulkan pipeline caches remain distinct native artifacts with different compatibility metadata and hit mechanisms. Shipping/build-time policy, concurrent writers, driver diversity, and public cache controls remain pending. |
| Label GPU work and measure its duration | Major objects and encoders have labels; completed command buffers provide aggregate GPU frame timing. **Established** for Metal. | Colored debug-utils regions label startup `compute` and every frame's `shadow`, `scene`, and `post` work. A capability-checked eight-entry query pool uses synchronization2 top/bottom timestamps for the same regions, masks counter wraparound to `timestampValidBits`, converts ticks with `timestampPeriod`, reads results only after fence completion, and reports startup plus shutdown aggregates. Queues with zero valid timestamp bits retain labels without timing. **Established** for the current Windows tier on native 4x, forced 1x, and resize paths. | Named diagnostic scopes are always available when backend diagnostics are enabled; timing is optional capability data attached to those scopes. Timing unavailability does not disable labels or rendering. | Metal currently measures whole command buffers while Vulkan measures named command regions. A shared diagnostic model must not promise identical scope boundaries, timestamp domains, resolution, or cross-backend comparability. |

## Frame graph and attachment contract

| Game-facing need | Metal evidence | Vulkan evidence | Candidate shared invariant | Native boundary or missing evidence |
| --- | --- | --- | --- | --- |
| Render depth-tested geometry | A reusable shadow-depth pass and depth-tested main pass use private depth textures. | A capability-selected sampled depth format backs both the depth-tested main pass and a persistent 1024x1024 depth-only shadow pass. Explicit synchronization makes shadow depth writes available to the main fragment shader's filtered comparison. **Established** on the current Windows tier. | A render pass declares attachment roles, load/store behavior, clear values, and depth state. Format selection can be capability-driven when the game requests a class rather than an exact format. | More complex shadow geometry, multiple lights, depth-format diversity, and resize-dependent shadow policy remain application-level evidence gaps. |
| Select multisampling with a tested fallback | Memoryless 4x color/depth attachments resolve into the scene target. | The shared color/depth sample mask selects 4x when supported and a physically validated 1x path otherwise. | Color and depth sample counts are negotiated together. A requested optional sample count can select a documented lower-count path; a required count fails creation. | Memoryless versus ordinary transient allocation remains backend policy. More hardware tiers remain pending. |
| Compose multiple passes through intermediate textures | Shadow, main MSAA, and fullscreen post-process passes share private resources within one command buffer. | A depth-only shadow pass transitions its persistent map for scene sampling; the scene then renders into a resize-dependent offscreen target and feeds a fullscreen post-process pass into the swapchain. | Intermediate attachments are ordinary resources or frame-scoped transient resources with explicit pass-to-pass dependencies. Presentation images are not required to be directly renderable by the game. | Automatic transient allocation and aliasing are pending; the initial API should not promise a full frame graph without evidence. |

## Presentation and platform lifecycle contract

| Game-facing need | Metal evidence | Vulkan evidence | Candidate shared invariant | Native boundary or missing evidence |
| --- | --- | --- | --- | --- |
| Acquire a presentable image without owning it indefinitely | `nextDrawable` returns a drawable whose texture remains valid through command-buffer presentation. A validation-layer run also acquired one drawable, accessed its texture, intentionally encoded and submitted nothing, drained the iteration's autorelease pool, then submitted 120 later frames and shut down cleanly. **Established** for one-shot submission abandonment on the current M2 tier. | Swapchain acquisition returns an image index synchronized by an acquire primitive; presentation releases the acquisition. A native-Wayland validation run also acquired one image through a dedicated fence, submitted no work, and presented nothing. With swapchain maintenance, `vkReleaseSwapchainImagesKHR` returned the untouched image; the forced base-swapchain path replaced and retired its complete generation. Both paths then presented 120 frames and shut down without validation messages. **Established** for one-shot non-presentation on the current Linux/Nvidia tier. | A presentable frame image is a scoped capability obtained from a surface. It cannot outlive the surface generation and is consumed by presentation or an explicit safe non-presentation path. | Metal releases an abandoned drawable at a backend-owned autorelease boundary. Vulkan releases the image directly only with swapchain maintenance; its fallback invalidates the whole presentation generation. These remain different native recovery mechanisms, not a portable fence promise. |
| Handle zero-sized or temporarily unavailable surfaces | A missing drawable skips the frame; resize-dependent resources follow drawable extent. **Partial** pending physical lifecycle evidence. | Zero client extent suspends rendering without creating a zero-sized swapchain; out-of-date acquisition triggers recreation. **Established** on the current Windows tier. Wayland minimize/restore remained functional on KDE Plasma. **Partial** because that run did not prove an explicit zero-sized configure. | Surface unavailability is a normal nonfatal state. Frame acquisition can report suspended/unavailable without fabricating a texture or treating minimization as device failure. | AppKit minimize/restore, an explicit Wayland zero-sized/suspended path, and broader compositor behavior remain pending. |
| Recreate resize-dependent resources | Metal recreates depth, memoryless MSAA, and scene-color textures when drawable extent changes. | Vulkan creates a new swapchain using the old one, recreates views and extent-dependent attachments, and retires the previous resource generation. The peer Wayland path uses the same renderer and physically recreated 198 generations during a responsive validation-clean drag test. | Resize creates a new surface generation. Resources tied to the old generation remain alive until all rendering and presentation access is complete. | Applications need a clear signal for rebuilding their own extent-dependent resources without observing backend-specific resize messages. |
| Know when presentation-owned resources may be destroyed | Retained command buffers establish GPU completion, while the drawable/presentation lifecycle remains owned by Metal. **Partial** pending broader shutdown/lifecycle evidence. | Presentation fences are used when swapchain-maintenance support exists; otherwise reacquisition history defers old swapchain and semaphore destruction. Both paths have current-machine evidence. | Rendering completion and presentation-engine completion are distinct. Surface retirement must track the stronger condition before destroying presentation resources. | The fallback is backend machinery, not a portable fence promise. Naturally extension-less hardware evidence remains pending. |
| Preserve useful frame cadence during native resize loops | AppKit lifecycle behavior has not yet been physically recorded. **Pending**. | Win32 redraw is driven during live resize. Wayland coalesces queued XDG configure events and paces resize swapchain commits so recreation cannot bypass FIFO backpressure; the final physical run averaged 16.522 ms between 198 responsive resize frames on a 74.971 Hz display. **Partial** pending broader display, compositor, and hardware evidence. | The platform layer must keep event delivery and rendering coordination functional during native modal/nested resize behavior. | Cadence policy is platform-specific and should feed the eventual runtime rather than leak native messages into the GPU API. |

## Failure and shutdown contract

| Game-facing need | Metal evidence | Vulkan evidence | Candidate shared invariant | Native boundary or missing evidence |
| --- | --- | --- | --- | --- |
| Receive actionable creation and capability failures | Missing selectors, objects, required formats/features, pipeline creation, archive operations, and command-buffer failures become contextual probe errors. | Missing layers/extensions/features, unsupported formats/memory/sample paths, Vulkan result failures, and validation messages become contextual probe errors. | Errors identify the failed operation, relevant resource or capability, and whether the game can choose a fallback. Native codes remain available as structured diagnostics. | Stable error categories must be derived from more failure-path evidence; avoid one undifferentiated string error in the public API. |
| Drain asynchronous work on ordinary shutdown | Every retained in-flight Metal command buffer is waited, checked, timed when possible, and released even after an earlier completion failure. | Frame work and tracked presentation operations are waited before owned resources are destroyed; the base swapchain path retains an orderly-idle fallback at final shutdown. | Shutdown is an explicit fallible lifecycle operation that attempts to drain all owned work and reports the first or aggregated failure without abandoning remaining cleanup. Drop remains best-effort. | Device loss, out-of-memory, process teardown, and partial-construction cleanup need dedicated evidence. |
| Keep validation part of the support claim | Metal supports native validation-layer runs and attaches labels to major objects. | The probe requires Khronos validation, counts warning/error callbacks, and records reproducible Windows evidence plus initial native Wayland presentation evidence with zero warning/error callbacks. | Debug configurations enable the strongest native validation and route messages through Mulciber diagnostics. A backend is not first-class solely because it renders. | Broader Linux and AppKit lifecycle evidence, driver diversity, and release-build structural checks remain pending. |

## Questions the extraction must resolve before support

The first experimental graphics vocabulary now represents physical surface extents, graphics-owned
surface generations, unavailable/reconfigured acquisition outcomes, and presented/abandoned frame
dispositions in `mulciber`. The Metal and Vulkan probes consume these shared facts without sharing
native objects or mechanisms. The Windows Vulkan validation matrix passed after integration at
development revision `c101e08` plus the working changes, including native and forced acquired-frame
abandonment recovery. The Metal path cross-host type-checks and lints; native macOS validation remains
required before this becomes cross-backend physical evidence.

These questions must be resolved before the affected portion of the API is treated as a supported
contract. Experimental types may test a candidate answer under the extraction plan; their existence
is not a resolution by itself.

1. What is the smallest resource-usage vocabulary that can derive the demonstrated Vulkan barriers
   without hiding meaningful Metal behavior or blocking advanced native paths?
2. Are uploads and readbacks methods on resources, operations on an explicit transfer context, or a
   more general command-encoding facility?
3. Which placement requests are stable game intent, and which allocation choices must remain backend
   policy until heap, residency, aliasing, and memory-pressure probes exist?
4. Should frame slots be visible to the game, or should a frame token carry the guaranteed-safe
   per-frame allocation context implicitly?
5. How does a game declare an optional fallback such as 4x-to-1x MSAA while still making the chosen
   path visible to diagnostics and pipeline creation?
6. Which surface-generation changes invalidate application resources, and how are those changes
   delivered without coupling the GPU crate to native window messages?
7. What safe escape hatch can expose native capability without allowing application code to violate
   Mulciber's ownership and presentation-retirement tracking?
8. Which error categories have genuinely distinct recovery actions: retry later, rebuild the surface,
   choose a fallback, recreate the device, or terminate?
9. Can Metal command-buffer timing and Vulkan named-region timing share a small diagnostic-scope
   vocabulary without implying identical boundaries, resolution, or cross-backend comparability?

## Evidence still blocking Gate 1

- Physical AppKit resize, minimize/restore, maximize/zoom, display-change, and shutdown evidence.
- Complete Wayland XDG-shell evidence for display changes, explicit zero-sized suspension, input,
  and broader compositors/hardware; the KDE Plasma presentation/resize/lifecycle path is
  physically established through the runtime dispatch layer. X11 presentation is physically
  established through XWayland with live sync-gated interactive resize and unlocked pacing;
  display changes, input, multi-display, and native Xorg coverage remain pending.
- Windows baseline hardware and broader driver coverage, including a naturally
  swapchain-maintenance-less adapter.
- Device-loss, out-of-memory, memory-pressure, suspend/resume, and recovery evidence on every claimed
  first-class platform.

This ledger should be updated when evidence changes. A completed probe checkbox does not automatically
create a public abstraction; it narrows the remaining design space by establishing another native
contract that Mulciber must preserve.
