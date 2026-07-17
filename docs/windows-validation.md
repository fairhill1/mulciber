# Win32/Vulkan validation runbook

The Win32 probe was initially compiled and cross-checked from macOS. This runbook captures the
physical Windows evidence required for each supported hardware and driver tier.

## Recorded validation

A development tree based on revision `4c12c55` lowered the Vulkan compatibility baseline to 1.3
while continuing to request 1.4 from capable loaders. Its complete automated matrix passed on
2026-07-18 on Windows 11 Home build 22000 with an Intel UHD Graphics 620, driver 31.0.101.2115,
Vulkan device API 1.3.215, and loader/validation 1.4.350. The capability report selected the adapter
without baseline failures and reported dynamic rendering, synchronization2, maintenance4, and BC
compression, while `VK_KHR_swapchain_maintenance1` was naturally absent. The run therefore exercised
the deferred reacquisition retirement path without an override, including acquired-frame abandonment
and recovery. The full 120-frame matrix passed preferred 4x and forced 1x MSAA, BC1 and forced RGBA8,
strict pipeline-cache hits, resize, damaged-cache recovery, and cache-disabled correctness. The
ordinary clear and cube examples each passed four automated extents. The two-pass postprocess example
passed 100 rapid resize transitions at 10 ms spacing. Its base-swapchain compatibility path waited
idle before each reconfiguration while only one generation existed, then retired that generation
after creating its replacement; it therefore neither accumulated swapchains nor suppressed redraws.
All three examples handled `WM_CLOSE`, destroyed their Vulkan surface before deferred Win32 window
destruction, exited zero, and emitted no Vulkan validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-20260718-010202.zip`. This is automated single-display evidence;
it does not claim manual visual correctness, interactive lifecycle coverage, multi-display behavior,
or other Intel driver/hardware tiers.

A focused manual follow-up ran the auto-spinning two-pass postprocess cube on the same Intel tier.
Continuous drag resize remained live after the automated retirement fixes, the rendered result was
accepted by the operator, and title-bar close exited zero. The captured log contains only the selected
Vulkan backend and 4x sample count, with no validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-postprocess-visual-20260718-010637.zip`. This is single-display
visual and drag-resize evidence for the focused postprocess example; it does not cover input,
minimize/restore, maximize/restore, Alt+F4, or multi-display behavior.

A focused Win32 input follow-up used a development tree based on revision `3b03bb8` on the same
Windows 11 Home / Intel UHD 620 tier. Native strict workspace clippy and all workspace tests passed;
the platform suite included scan-code navigation/numpad distinctions, signed client coordinates,
and extended-button identity. The combined postprocess/input showcase then physically exercised
W/A/S/D and arrow rotation, Space pause/resume, R reset, primary-button drag on both axes, wheel
zoom, and drag resize. The operator accepted every control, key presses produced no default OS beep,
title-bar close exited zero, and the captured log contained only the selected Vulkan backend and 4x
sample count. Evidence: `validation-artifacts/windows-vulkan-input-visual-20260718-014428.zip`.
This focused single-display pass did not exercise key repeat, modifier transitions,
outside-window release, focus loss/reacquisition, minimize/restore, maximize/restore, Alt+F4, or
multi-display behavior.

A same-tree `-SkipInteractive` matrix rerun was attempted after that focused pass but is not counted
as passing evidence. Its clear, cube, and postprocess automated resize/close smokes completed, then
the original synchronous cross-process resize controller blocked indefinitely during the first full
probe resize smoke. Replacing that controller call with asynchronous `SetWindowPos` plus a bounded
`SendMessageTimeout` responsiveness check let a focused four-size full-probe diagnostic close and
exit zero. A clean matrix retry later stopped advancing during an ordinary 600-frame full-probe run,
before the bounded resize controller was involved, and was terminated rather than left on the
interactive desktop. The previously recorded complete Intel matrix remains the Vulkan checkpoint;
this input change claims only the native tests and focused physical/resize evidence above.

The in-progress resource-backed cube checkpoint ran natively on 2026-07-17 on the Windows 11 / RTX
3060 Ti tier. The preferred Vulkan path selected 4x MSAA, uploaded indexed geometry and an RGBA8 sRGB
checkerboard, created a WGSL-derived native pipeline and depth/MSAA targets, abandoned one acquired
image with generation-safe target replacement, recovered, presented 240 frames, and shut down with
zero Vulkan validation messages. A forced 1x path presented another 120 frames cleanly. The cached
Vulkan artifact was validated against `vulkan1.4` by SPIRV-Tools v2026.2 and has SHA-256
`6248a2970f0d1c81c62aa5d2e785762a7a81830dacd2415eb08cd17e25c9aacc`. The complete automated
preflight then passed from the same development tree: its cube coverage repeated 120-frame 4x
abandonment/recovery and forced-1x runs, then presented 144 frames across four automated window
extents with generations 2 through 5. Evidence:
`validation-artifacts/windows-vulkan-20260717-150401.zip`. A subsequent interactive run displayed the
rotating checkerboard cube correctly through aggressive drag-resize and one minimize/restore cycle,
then closed normally. This is single-display physical visual, resize, minimize/restore, and close
evidence; it is not multi-display or broader hardware evidence.

After the user-facing examples were separated from validation-only controls, the complete automated
matrix passed again on 2026-07-17 on the same Windows 11 / RTX 3060 Ti tier. The ordinary clear and
cube examples each completed four-size automated resize and `WM_CLOSE` shutdown. The public-API
clear probe abandoned generation 2 and recovered for 120 presentations; the cube probe recovered
from abandonment for 120 preferred-4x presentations and separately completed 120 forced-1x
presentations. Evidence: `validation-artifacts/windows-vulkan-20260717-152610.zip`. This rerun is
automated evidence, not a new physical visual, multi-display, or broader hardware claim.

A clear-only Gate 2 checkpoint based on revision `2d24f8f` plus the uncommitted extraction changes
passed the automated preflight on 2026-07-17 on the same Windows 11 / RTX 3060 Ti tier. The
same-source `mulciber-clear` application selected Vulkan, abandoned one acquired image by replacing
the base swapchain generation, recovered for 10 presented frames, then passed automated resize at
four window sizes with generations 2 through 5 and 145 total clear presentations. Shutdown reported
zero Vulkan validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-20260717-140130.zip`. This is automated clear-path evidence, not
physical visual, lifecycle, multi-display, broader hardware, or Metal evidence.

A development tree based on revision `c101e08` then extracted physical surface extents,
graphics-owned generations, acquisition outcomes, and frame dispositions into `mulciber`; the Vulkan
probe consumed those types in swapchain creation, acquisition, reconfiguration, presentation, and
both non-presentation paths. The complete automated matrix passed on 2026-07-17 on the same Windows
11 / RTX 3060 Ti tier without validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-20260717-124258.zip`. This is automated Vulkan evidence, not a
new physical lifecycle, visual, multi-display, or Metal claim.

The development tree after `mulciber-platform` 0.1.0 moves Win32 application/window ownership,
thread-message dispatch, client metrics, nested live-resize redraw callbacks, and borrowed Vulkan
surface handles into the platform crate. Both Vulkan probes consume that implementation and
cross-compile and lint cleanly for `x86_64-pc-windows-msvc` from Linux.

The extracted path was physically validated on 2026-07-17 at revision `044ae86` on Windows 11 / RTX
3060 Ti. The preferred automated matrix passed with Vulkan loader/validation 1.4.350, including the
finite and automated-resize runs, native and forced acquired-frame abandonment recovery, the
pipeline-cache matrix, forced 1x MSAA, and forced RGBA8 paths. A separate traced lifecycle run covered
continuous resize including very small extents, minimize/restore, maximize/restore, and titlebar
close; the nested resize callback rendered all 2,662 attempts, with 12.927 ms average callback
spacing and 11.787 ms average across 1,912 swapchain recreations. Alt+F4 then closed a separate run.
All processes exited zero without Vulkan validation or loader messages.

During rapid edge resize, a narrow black-and-white bar could appear briefly, most visibly on the
right while shrinking from the left. Registering the class with full horizontal/vertical redraw was
physically tested, did not change the artifact, and was reverted. The Vulkan Cube demo showed the same
artifact on this Windows/driver stack, so it is recorded as a non-blocking compositor/driver-time
observation rather than an extraction regression. Multi-display behavior was not recorded.
Evidence: `validation-artifacts/windows-vulkan-20260717-120410.zip`.

The first physical run was recorded on 2026-07-16 at commit
`1972ad486d4bbd8a76c714aca86513c60419ba2a`: Windows 11 Pro build 26200, Nvidia GeForce RTX 3060 Ti,
driver 591.86, and Vulkan loader/validation layer 1.4.350. The 600-frame finite run, resize recovery,
minimize/restore, maximize/restore, title-bar shutdown, and Alt+F4 shutdown completed without Vulkan
validation or loader warnings. Rendering resized only after the drag ended rather than continuously
during the drag, so the presentation milestone remains incomplete. Display movement was not
tested because only one display was available. This establishes initial evidence on one modern
Nvidia tier; it does not replace the required GTX 1060-class baseline or multi-display runs.

A follow-up physical run on the same machine exercised the live-resize implementation committed as
`656863a`. The triangle continued updating during the drag, and resize recovery, minimize/restore,
maximize/restore, title-bar shutdown, and Alt+F4 shutdown worked without Vulkan validation or loader
messages. Live resize was functional: the window itself resized smoothly, while the triangle's
resizing looked slightly choppy or delayed; its rendering cadence was not measured. Swapchain
recreation continued to wait for the device to become idle. Multi-display behavior was not tested
because only one display was available.

A second follow-up on 2026-07-16 replaced device-idle swapchain recreation with tracked retirement.
On the same RTX 3060 Ti, the probe selected `VK_KHR_swapchain_maintenance1`, completed an automated
600-frame run, and completed a focused manual drag-resize run without validation or loader output.
Each queued presentation now carries a presentation fence, and old swapchains are destroyed only
after all of their pending presentation fences signal. Adapters without the extension use a
deferred fallback: old swapchains remain alive until reacquiring a previously presented image from
the replacement swapchain proves the earlier presentation queue has drained. That fallback is not
yet physically exercised on an adapter lacking the extension. Final shutdown retains a
device-idle compatibility fallback only when presentation fences are unavailable. The focused
drag-resize runs felt the same or slightly better than before and were considered good enough for
now. The window resizing itself was smooth; only the triangle's resizing looked a little choppy or
delayed, and it was noticeably choppier than the Vulkan cube demo. Rendered resize cadence therefore
remains the open question and has not been measured.

A measured follow-up instrumented the live-resize path and separated callback spacing, frame-fence
wait, swapchain recreation, image acquisition, command recording/submission, and presentation. With
timer-only redraw, 558 attempts averaged 27.154 ms between callbacks; frames averaged 7.652 ms, of
which swapchain recreation averaged 7.555 ms. Reusing the format-compatible graphics pipeline
reduced recreation only modestly, showing that pipeline compilation was not the main bottleneck.
Driving redraw directly from `WM_SIZE`, with the timer retained as a fallback, reduced callback
spacing to 9.004 ms over 1,151 attempts and produced roughly 80 size-changing frames per second in
that run. The user reported that the triangle resizing looked noticeably better. Swapchain creation
still averaged 10.493 ms under the faster churn and reached 21.247 ms in the worst observed sample;
parity with the Vulkan cube demo has not been established.

The deferred-retirement compatibility path was then forced on the same machine with
`MULCIBER_VULKAN_FORCE_SWAPCHAIN_FALLBACK=1`. Its 600-frame automated run and measured manual drag-resize
run completed without validation or loader output. The manual run covered 1,352 resize attempts and
968 swapchain recreations; callback spacing averaged 9.540 ms, image acquisition averaged 0.072 ms,
and the maximum observed acquisition was 15.547 ms. This physically exercises the fallback logic
under rapid resize, but it does not replace future coverage on hardware or a driver that naturally
lacks `VK_KHR_swapchain_maintenance1`.

The native Vulkan capability report was first recorded on the same machine on 2026-07-16. It found
one adapter, selected the RTX 3060 Ti as Mulciber-baseline-compatible, decoded Nvidia driver 591.86,
reported Vulkan API 1.4.325, three memory heaps, six queue families, 261 device extensions, five
Win32 surface formats, and five present modes. Both the human-readable form and the schema-versioned
JSON form completed successfully; PowerShell parsed the JSON without repair.

The first representative Vulkan resource slice was then exercised on the same machine. The probe
uploaded interleaved positions/colors and 16-bit indices into owned host-visible coherent Vulkan
buffers, bound both resources, and rendered through `vkCmdDrawIndexed`. A 600-frame run completed
without validation or loader output. This establishes buffer allocation, memory-type selection,
mapping/upload, vertex-input declaration, binding, indexed drawing, and orderly resource teardown;
it does not yet establish device-local staging uploads or the remaining representative workload.

The geometry path was subsequently moved into device-local vertex and index buffers. Two temporary
host-visible coherent staging buffers are mapped and populated at startup, copied with
`vkCmdCopyBuffer2`, and synchronized with explicit transfer-write to vertex/index-read buffer
barriers before a fenced upload submission completes. A 600-frame run on the same machine reported
the device-local staging path and completed without validation or loader output. This establishes
the first real Vulkan upload path; readback and reusable upload scheduling remain outstanding.

The sampled-texture slice was then validated on 2026-07-16. A 4x4 RGBA8 sRGB checkerboard is copied
from a host-visible staging buffer into a device-local optimal-tiled image with explicit
`UNDEFINED` to `TRANSFER_DST_OPTIMAL` and `TRANSFER_DST_OPTIMAL` to
`SHADER_READ_ONLY_OPTIMAL` synchronization2 transitions. The renderer creates an image view,
nearest-repeat sampler, combined image sampler descriptor, and descriptor-aware pipeline layout;
the fragment shader samples the image while drawing the indexed triangle. The complete
noninteractive Windows gate ran 600 frames on the RTX 3060 Ti without validation or loader
messages. Evidence: `validation-artifacts/windows-vulkan-20260716-104453.zip`.

The depth slice was validated on 2026-07-16. The renderer queries optimal-tiled depth-attachment
format support, selected `D32_SFLOAT` on the RTX 3060 Ti, and creates a device-local depth image and
view at each swapchain extent. Dynamic rendering clears the attachment after an explicit
`UNDEFINED` to `DEPTH_ATTACHMENT_OPTIMAL` transition, with depth testing and writes enabled in the
pipeline. Depth resources retire with their corresponding swapchains so resize does not destroy an
in-flight attachment. The validation gate now automatically resizes the window through 640x360,
1200x700, 320x240, and 960x540 before closing it with `WM_CLOSE`; that smoke test and the 600-frame
finite run completed without validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-20260716-105528.zip`.

The uniform-buffer slice was validated on 2026-07-16. Three host-visible coherent buffers remain
persistently mapped for the renderer lifetime, and three descriptor sets pair the shared sampled
texture with one frame-local uniform buffer apiece. After frame-fence completion, the CPU writes an
80-byte std140-compatible block containing an aspect-correct transform and elapsed time, binds the
matching descriptor set, submits, and advances the frame slot. The vertex shader preserves triangle
proportions across window shapes and the fragment shader uses time for a subtle color pulse. The
600-frame run and automated four-extent resize smoke completed without validation or loader
messages. Evidence: `validation-artifacts/windows-vulkan-20260716-110153.zip`.

The compute storage/readback slice was validated on 2026-07-16. An offline-compiled compute shader
dispatches 64 invocations into a device-local storage buffer through its own storage descriptor and
compute pipeline; adapter selection requires the chosen queue family to support graphics, compute,
and presentation. A synchronization2 buffer barrier makes shader writes visible to transfer, the
buffer is copied into host-visible coherent memory, and a second barrier makes the transfer writes
visible to the host. After fenced completion, the probe maps the readback allocation and compares
all 64 `u32` values against the deterministic expected sequence, failing startup on any mismatch.
Both the 600-frame run and automated resize smoke reported exact readback and completed without
validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-20260716-111034.zip`.

The indexed-indirect slice was validated on 2026-07-16. The compute shader also writes a native
20-byte `VkDrawIndexedIndirectCommand` into a device-local storage/indirect buffer. A synchronization2
barrier makes that shader write visible both to transfer readback and indirect-command consumption;
startup compares all five command fields exactly, and rendering consumes the same allocation through
`vkCmdDrawIndexedIndirect`. The 600-frame run and automated four-extent resize smoke completed on the
RTX 3060 Ti without validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-20260716-111925.zip`.

The storage-image slice was validated on 2026-07-16. The renderer capability-checks optimal-tiled
`R8G8B8A8_UNORM`, creates a device-local 8x8 image with storage, transfer-source, and sampled usage,
and exposes it to the startup compute shader through a storage-image descriptor. The 8x8 workgroup
writes an exact magenta/cyan texel pattern alongside the existing storage-buffer and indirect-command
outputs. Explicit synchronization2 transitions make the image writable by compute, readable by copy,
then readable by the fragment shader; all 256 copied-back bytes are checked before rendering samples
the same image. The 600-frame run and automated four-extent resize smoke completed on the RTX 3060 Ti
without validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-20260716-112831.zip`.

The mip-generation slice was validated on 2026-07-16. The compute image now owns four mip levels
covering 8x8, 4x4, 2x2, and 1x1, with separate base-level storage and full-chain sampled views. After
compute writes mip 0, three nearest-filter `vkCmdBlitImage2` operations generate the remaining levels;
each destination subresource transitions independently from undefined to transfer destination and
then transfer source before feeding the next blit. The readback copy verifies all 256 base bytes and
the exact magenta 1x1 tail, after which the complete chain transitions to shader-read-only and the
fragment shader explicitly samples mip 1. The 600-frame run and automated four-extent resize smoke
completed on the RTX 3060 Ti without validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-20260716-113651.zip`.

The multisampling slice was validated on 2026-07-16. Adapter selection intersects framebuffer color
and depth sample-count support, choosing 4x when available and otherwise retaining a 1x path. At 4x,
each swapchain generation owns transient device-local multisampled color and depth images; dynamic
rendering clears and renders into them, resolves color into the acquired single-sample swapchain
image with average resolve, and discards transient attachment contents. These images retire with the
swapchain and its presentation-completion tracking. The validation gate completed the native 4x
600-frame run, a forced 1x 600-frame fallback run, and the four-extent 4x resize smoke without
validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-20260716-114427.zip`.

The offscreen/post-processing slice was validated on 2026-07-16. The renderer capability-checks a
linear-filterable `R8G8B8A8_UNORM` color-attachment format and creates a single-sample offscreen image
at every swapchain extent. The scene pass renders directly into it on the 1x path or resolves 4x color
into it, then an explicit color-attachment-write to fragment-sampled-read transition feeds a dedicated
linear-clamp descriptor and fullscreen-triangle pipeline. A second dynamic-rendering scope applies a
vignette into the acquired swapchain image. Offscreen targets and both format-dependent pipelines
participate in tracked swapchain retirement. Native 4x and forced 1x 600-frame runs plus the four-size
resize smoke completed without validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-20260716-115422.zip`.

The GPU-instrumentation slice was validated on 2026-07-16. Adapter selection records the selected
queue family's `timestampValidBits` and the device's nanoseconds-per-tick period. A six-entry
timestamp query pool measures the startup compute dispatch plus every frame's complete scene and
post command regions with synchronization2 top/bottom writes; result conversion masks counter
wraparound to the advertised bit width. Query results are read only after the frame fence signals,
and shutdown prints aggregate scene/post averages. A queue family with zero valid timestamp bits
skips query creation while preserving `VK_EXT_debug_utils` labels named `compute`, `scene`, and
`post`. On the RTX 3060 Ti, the validation gate measured 0.005 ms startup compute, 0.062 ms average
4x scene and 0.021 ms post, then 0.028 ms average forced-1x scene and 0.029 ms post. Native 4x and
forced 1x 600-frame runs plus the four-size resize smoke completed without validation or loader
messages. Evidence: `validation-artifacts/windows-vulkan-20260716-120435.zip`.

The shadow-pass slice was validated on 2026-07-16. Depth format selection now requires both
optimal-tiled depth-attachment and sampled-image support. The renderer creates a persistent
single-sample 1024x1024 depth image, depth-only dynamic-rendering pipeline, and sampled
descriptor. Each frame clears and renders an offset light-space triangle projection through the
same GPU-written indexed-indirect command as the scene, then explicitly transitions depth writes to
fragment sampled reads. The main fragment shader performs a biased 3x3 depth comparison, while an
independent `shadow` debug label and timestamp pair feed the shutdown timing summary. The query pool
expands from six to eight entries to cover that fourth region. The validation
gate measured 0.020 ms average shadow time on both native 4x and forced 1x paths; their scene/post
averages were 0.073/0.025 ms and 0.026/0.027 ms respectively. Both 600-frame runs and the four-size
resize smoke completed without validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-20260716-121826.zip`.

The Vulkan pipeline-cache slice was validated on 2026-07-16. One device-specific raw cache backed
the startup compute, shadow, scene, and post pipelines. The native 4x cold run produced valid misses
and atomically stored 48,457 bytes; forced 1x learning and fresh strict 4x/1x processes then reported
valid application-cache hits for every requested pipeline while compile-required control was
enabled. Strict four-size resize preserved those hits, and artifact hashes before and after all
strict runs matched. Truncated and vendor-mismatched copies were rejected by header preflight and
replaced after clean learning runs. A one-byte opaque-payload mutation caused a detected
`scene-4x` miss while the other pipelines hit; learning recovered safely and replaced the copy with
a 56,980-byte artifact. A missing strict artifact failed before pipeline creation, and a subsequent
120-frame `--disable-pipeline-cache` run established unchanged rendering correctness with valid
non-hit feedback. The complete cache matrix emitted no validation or loader messages. Evidence:
`validation-artifacts/windows-vulkan-20260716-125230.zip`.

The Vulkan BC1 slice was validated on 2026-07-16. The RTX 3060 Ti reported core
`textureCompressionBC` support and optimal-tiling features `0x0001d401`, satisfying the sampled,
transfer-destination, and transfer-source roles used by the fixed 8x8 image. Required BC1 mode
uploaded four blocks, copied all 32 encoded bytes back exactly, and directly sampled the image for
600 frames on native 4x and forced 1x. Fresh strict 4x/1x processes and the four-size resize smoke
retained application-cache hits for compute, shadow, scene, and post. Forced RGBA8 mode copied all
256 expanded bytes back exactly and passed the same 600-frame 4x/1x and resize paths without adding
a pipeline variant. Captured BC1 and RGBA8 windows showed the same expected repeated checkerboard
modulation across the triangle. No validation or loader messages were emitted. Evidence:
`validation-artifacts/windows-vulkan-20260716-130403.zip`.

## Setup

Install a current vendor driver exposing Vulkan 1.3 or newer with dynamic rendering and
synchronization2, Rust 1.97, and a Vulkan SDK containing
`VK_LAYER_KHRONOS_validation` and `vulkaninfo`. No SDK library is needed to build Mulciber because the
probe loads `vulkan-1.dll` at runtime.

Record the machine and driver before running:

```powershell
vulkaninfo
rustc --version --verbose
```

The full `vulkaninfo` form is intentional: older SDK releases do not recognize `--summary`, and the
full report preserves features and properties needed for later capability comparisons.

## Preferred evidence run

From the repository root, run:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\validate-windows.ps1
```

The script records the OS, GPU and driver, Git revision/status, Rust version, the native Mulciber JSON
capability report, full `vulkaninfo`, and Cargo test output. Its runtime matrix rebuilds a pipeline
cache during the normal 600-frame run, expands it on forced 1x, requires read-only cross-process hits
on native 4x and forced 1x, performs the resize smoke in strict mode, verifies that strict runs do not
change the artifact, recovers copies with truncated, incompatible, and payload-damaged data, and
runs once with caching disabled. It exercises acquired-frame non-presentation through both the native
swapchain-maintenance path and the forced base-swapchain generation-replacement path. It also forces
RGBA8 through native 4x, forced 1x, and resize so a BC-capable adapter cannot hide fallback
regressions. It then guides two interactive runs: lifecycle testing closed through the title bar,
followed by an Alt+F4 shutdown test.

The automated matrix builds the ordinary same-source clear, cube, and postprocess-cube examples. It
drives clear and cube through the four-size resize smoke, then drives postprocess through 25 cycles of
those four extents at 10 ms spacing to exercise compatibility retirement pressure before closing each
through `WM_CLOSE`. Separate public-API clear and cube probes own finite execution, acquired-frame
abandonment followed by presented recovery, and the cube's forced 1x path so validation controls do
not leak into user-facing examples.

Every native command must exit successfully, the probe treats every validation warning/error as a
failure, and the script checks the captured logs again. The result is written to a timestamped ZIP
under `validation-artifacts/`. Use `-SkipInteractive` only for an automated preflight that will not be
accepted as complete physical validation. `-Frames N` and `-OutputRoot PATH` override their defaults.

## Manual fallback

From the repository root:

```powershell
$env:VK_LOADER_DEBUG = "error,warn"
cargo run -q -p mulciber-vulkan-info -- --json
cargo test -p mulciber-vulkan-triangle
cargo run -p mulciber-vulkan-triangle -- --frames 600
cargo run -p mulciber-vulkan-triangle -- --abandon-acquired-frame-once --frames 120
$env:MULCIBER_VULKAN_FORCE_SWAPCHAIN_FALLBACK = "1"
cargo run -p mulciber-vulkan-triangle -- --abandon-acquired-frame-once --frames 120
Remove-Item Env:MULCIBER_VULKAN_FORCE_SWAPCHAIN_FALLBACK
```

Success means exit code zero, a colored triangle was visible, and neither the Mulciber validation
callback nor the loader printed a warning or error. Preserve the full output with the capability
report for the machine.

Each acquired-frame non-presentation run must report exactly one untouched acquired image, recovery
after a later presentation, 120 submitted frames, and clean shutdown. The normal run uses
`vkReleaseSwapchainImagesKHR` when maintenance is supported; the forced run must replace and retire
the complete abandoned swapchain generation. The preferred validation script runs and archives both
commands automatically.

For an opt-in live-resize timing summary, set `MULCIBER_VULKAN_RESIZE_TRACE=1` before launching without a
frame limit. Drag-resize, close the window, and preserve the printed callback, recreation, acquire,
submit, and present timings.

To exercise the compatibility path on a driver that supports presentation fences, set
`MULCIBER_VULKAN_FORCE_SWAPCHAIN_FALLBACK=1`. The probe will skip
`VK_KHR_swapchain_maintenance1`, print `Swapchain retirement: deferred reacquisition fallback`, and
keep retired swapchains alive until reacquisition proves queued presentation has completed. This is
a diagnostic override, not a normal runtime recommendation.

Set `MULCIBER_VULKAN_FORCE_MSAA_1X=1` to bypass supported 4x multisampling and exercise the 1x color
and depth path. The automated validation script runs this finite fallback check in addition to the
adapter-selected path.

Set `MULCIBER_VULKAN_TEXTURE_MODE` to `auto`, `bc1`, or `rgba8` to select the sampled-texture policy.
Auto prefers BC1 only when the core compression feature and every sampled/transfer role used by the
probe are supported. Required BC1 fails with the missing feature names; RGBA8 deliberately bypasses
compression. The validation script always forces and exercises RGBA8, and it validates required BC1
when launched with `MULCIBER_VULKAN_TEXTURE_MODE=bc1`.

## Interactive lifecycle pass

Run without `--frames`, then:

1. Resize continuously, including very small sizes.
2. Minimize for several seconds and restore.
3. Maximize and restore.
4. If multiple displays are available, move the window between them.
5. Close with both the title-bar button and Alt+F4 on separate runs.

The triangle must remain stable, resize without stale frames, resume after minimize, remain
VSync-limited, and shut down without validation output. Record apparent low frame rate, delay, or
other live-resize pacing issues even when the functional live-resize check passes.

## Evidence to return

- Prefer the timestamped ZIP generated by `scripts/validate-windows.ps1`.
- Otherwise, include the full `vulkaninfo` report and GPU/driver identification.
- Whether the adapter is the GTX 1060-class baseline or another test tier.
- Console output from the finite run.
- Any validation message verbatim, plus the action that triggered it.
