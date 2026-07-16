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
`ZINC_VULKAN_FORCE_SWAPCHAIN_FALLBACK=1`. Its 600-frame automated run and measured manual drag-resize
run completed without validation or loader output. The manual run covered 1,352 resize attempts and
968 swapchain recreations; callback spacing averaged 9.540 ms, image acquisition averaged 0.072 ms,
and the maximum observed acquisition was 15.547 ms. This physically exercises the fallback logic
under rapid resize, but it does not replace future coverage on hardware or a driver that naturally
lacks `VK_KHR_swapchain_maintenance1`.

## Setup

Install a current vendor driver exposing Vulkan 1.4, Rust 1.97, and a Vulkan SDK containing
`VK_LAYER_KHRONOS_validation` and `vulkaninfo`. No SDK library is needed to build Zinc because the
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

The script records the OS, GPU and driver, Git revision/status, Rust version, full Vulkan report,
Cargo test output, and a 600-frame validation run. It then guides two interactive runs: lifecycle
testing closed through the title bar, followed by an Alt+F4 shutdown test.

Every native command must exit successfully, the probe treats every validation warning/error as a
failure, and the script checks the captured logs again. The result is written to a timestamped ZIP
under `validation-artifacts/`. Use `-SkipInteractive` only for an automated preflight that will not be
accepted as complete physical validation. `-Frames N` and `-OutputRoot PATH` override their defaults.

## Manual fallback

From the repository root:

```powershell
$env:VK_LOADER_DEBUG = "error,warn"
cargo test -p zinc-vulkan-win32-triangle
cargo run -p zinc-vulkan-win32-triangle -- --frames 600
```

Success means exit code zero, a colored triangle was visible, and neither the Zinc validation
callback nor the loader printed a warning or error. Preserve the full output with the capability
report for the machine.

For an opt-in live-resize timing summary, set `ZINC_VULKAN_RESIZE_TRACE=1` before launching without a
frame limit. Drag-resize, close the window, and preserve the printed callback, recreation, acquire,
submit, and present timings.

To exercise the compatibility path on a driver that supports presentation fences, set
`ZINC_VULKAN_FORCE_SWAPCHAIN_FALLBACK=1`. The probe will skip
`VK_KHR_swapchain_maintenance1`, print `Swapchain retirement: deferred reacquisition fallback`, and
keep retired swapchains alive until reacquisition proves queued presentation has completed. This is
a diagnostic override, not a normal runtime recommendation.

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
