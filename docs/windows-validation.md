# Win32/Vulkan validation runbook

The Win32 probe was initially compiled and cross-checked from macOS. This runbook captures the
physical Windows evidence required for each supported hardware and driver tier.

## Recorded validation

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

## Setup

Install a current vendor driver exposing Vulkan 1.4, Rust 1.97, and a Vulkan SDK containing
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
capability report, full `vulkaninfo`, Cargo test output, a normal 600-frame validation run, and a
forced 1x multisampling-fallback run. It then guides two interactive runs: lifecycle testing closed
through the title bar, followed by an Alt+F4 shutdown test.

Every native command must exit successfully, the probe treats every validation warning/error as a
failure, and the script checks the captured logs again. The result is written to a timestamped ZIP
under `validation-artifacts/`. Use `-SkipInteractive` only for an automated preflight that will not be
accepted as complete physical validation. `-Frames N` and `-OutputRoot PATH` override their defaults.

## Manual fallback

From the repository root:

```powershell
$env:VK_LOADER_DEBUG = "error,warn"
cargo run -q -p mulciber-vulkan-info -- --json
cargo test -p mulciber-vulkan-win32-triangle
cargo run -p mulciber-vulkan-win32-triangle -- --frames 600
```

Success means exit code zero, a colored triangle was visible, and neither the Mulciber validation
callback nor the loader printed a warning or error. Preserve the full output with the capability
report for the machine.

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
