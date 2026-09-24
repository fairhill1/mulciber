# Presentation choice and explicit frame caps

Release versions: graphics 0.13.21, platform/runtime 0.5.5.

`Surface::set_vsync(false)` requests immediate presentation, preserving available
throughput below refresh at the cost of possible tearing. `true` retains the
platform synchronized policy: Metal display sync; Windows mailbox when supported,
otherwise FIFO; Linux FIFO. Default engine policy remains synchronized. Applications
choose their own default. Call between frames, before acquisition. A live acquired
frame is a Lifecycle error. Unsupported immediate presentation returns Unsupported
without changing policy; the application must make any fallback observable.

Metal drains submitted GPU work before changing the live CAMetalLayer setting.
Vulkan validates support before changing desired policy, then rebuilds the swapchain
through ordinary acquisition/generation and retirement rules. Targets must follow
surface generations as they already do for resize. Compositor policy and variable
refresh remain native-system behavior; this API does not claim VRR discovery or
force tearing through a compositor that disallows it.

`FrameStartLimiter::set_frame_rate_limit(NonZeroU16::new(50))` schedules frame starts
at least 20 ms apart before polling fresh input. None removes the explicit cap.
It works without display feedback or native-refresh limiting and overrides the
optional refresh ceiling. Overruns never trigger catch-up bursts; OS wake lateness
reanchors the next explicit-cap deadline. The wait learns OS sleep overshoot with
a spin margin bounded at 3 ms; this avoids systematic timer-coalescing underdelivery
without spinning for the entire frame budget. A cap change discards the old deadline,
and suspension reset preserves the cap but discards display timing/deadlines.
The explicit limiter does not round caps to refresh divisors. The separate Strict presentation
policy below deliberately chooses full/half native refresh.

Use `Runtime::set_presentation_pacing_enabled(false)` with immediate presentation.
The fixed simulation step, catch-up bound and interpolation are unchanged; deltas
use elapsed time so compositor feedback cannot quantize a 50 FPS simulation onto
60 Hz. Diagnostics continue consuming native feedback. Synchronized applications
can retain existing cadence smoothing.

## Validation

Runtime unit tests cover 50 FPS independent of 60 Hz feedback, cap changes,
suspension, missing feedback, overloaded frames, and immediate simulation deltas.
The extended native API conformance run on Apple M2 passes 99 cases with Metal API
Validation enabled, including live off/on/off transitions, rejection with a live
acquired frame, abandonment and resource churn. Workspace tests and Clippy pass;
Windows x86_64 compilation covers the Vulkan implementation. Native Vulkan,
variable-refresh, multi-display and physical input-to-display latency are not
validated by these checks. Consumer pacing traces are recorded separately in the
macOS runbook; no viability gate is advanced.

## Native display timing — platform/runtime 0.5.5

The platform exposes fixed, variable and unknown display timing in `WindowMetrics`. AppKit reads
NSScreen's minimum/maximum refresh intervals and update granularity from the window's current
screen, with a metrics revision when that timing changes. Win32/Wayland/X11 return `Unknown`
until their native capability paths have evidence; nominal refresh or present mode does not prove
VRR. The range describes capability, not per-frame active VRR.

The runtime consumes this metadata from window events. Cadence smoothing now requires a known
fixed native period plus fresh presentation feedback. Variable/unknown timing retains elapsed
deltas, including below the reported range; low-frame-rate compensation remains display/driver
behavior. The limiter accepts the same timing, disabling only its implicit refresh ceiling on
variable/unknown displays and retaining explicit user caps. Native timing metadata itself never introduces a 30 FPS cap.
Fixed 60 Hz plus VSync still cannot display 50 FPS at equal intervals.

Regressions cover 35–55 FPS variable intervals, a 120 ms hitch, mode transitions, stale feedback,
slow application feedback on a fixed screen, and a 50 FPS explicit cap on variable/unknown timing.
Native fixed-refresh evidence is from the Apple M2 built-in panel (16.666 ms min/max/granularity).
Physical VRR and multi-display transitions remain unvalidated.

## Adaptive and Strict presentation — graphics 0.13.21

`Surface::set_presentation_mode` adds policies alongside the compatible boolean API:

| Policy | Native behavior |
| --- | --- |
| Immediate | Allow tearing; present as soon as possible. |
| Adaptive | Synchronize without letting the queue cost latency; whether a late frame tears is the adapter's answer. |
| Synchronized | Retain synchronization even when deadlines are missed. |
| HalfRefresh | Always schedule each image for two native refresh periods. |
| Strict | Keep synchronization; choose full/half refresh from measured CPU/GPU work. |

Query `supports_presentation_mode` before offering a policy. Unsupported requests return an
error without substituting another mode. `active_presentation_mode` exposes the current native
choice for diagnostics. A game may offer four choices, using Strict instead of the fixed
HalfRefresh primitive. It should explain any fallback when loading an unsupported saved choice.

Metal Adaptive starts synchronized and judges the workload exactly as Strict does (below): three
consecutive overloaded frames release display sync, and 90 fresh samples with 5% CPU and GPU
headroom restore it. Unknown native timing stays immediate. Variable-capable screens retain native synchronization, without
assuming that capability proves VRR engagement; macOS adaptive scheduling also requires the
appropriate display setting and fullscreen presentation.

Strict starts at full refresh. Three consecutive overload samples (CPU or fresh GPU work
longer than a native interval) step it down; 90 fresh GPU samples with CPU/GPU work below 95%
recover full rate. This permits an 85-90 FPS workload to recover 75 Hz, while retaining a small
hysteresis margin. Missing GPU results do not manufacture overload or recovery. Acquisition gaps
and queued half-rate frames are not workload samples. Vulkan stops measuring CPU work before
native queue submission and waits for image availability before its Strict GPU timestamp span,
so presentation backpressure is excluded from both workload measurements. The target is derived
from the native period: 75 Hz uses 37.5 FPS, not 30. It does not guarantee smooth motion when the
workload exceeds even the half-rate budget. On Metal variable-capable displays it leaves cadence
to native synchronization rather than imposing a fixed divisor. A real VRR display remains
necessary to validate that path, including window/fullscreen and below-range behavior.

Metal schedules half refresh with `presentDrawable:afterMinimumDuration:` and a small tolerance
below two periods, rounded by synchronized scanout.

Vulkan Adaptive means what relaxed FIFO means: synchronized while frames keep up with the
screen, immediate once they cannot, so a slow stretch tears instead of alternating one- and
two-refresh frames. Relaxed FIFO is taken wherever the surface lists it, so no machine already
served changes behavior.

NVIDIA's Linux driver lists it on no surface type. There the device enables
`VK_KHR_swapchain_maintenance1`, the instance `VK_KHR_surface_maintenance1`, and where the surface
reports FIFO and immediate as compatible the swapchain is created in FIFO with immediate beside
it, sized for whichever mode needs more images. Each present chains
`VkSwapchainPresentModeInfoKHR` choosing between them from the policy Metal uses (`backend/adaptive.rs`), which is Strict's
workload judgement: three consecutive frames whose CPU or GPU work exceeds the period release to
immediate, and 90 fresh samples at 95% of the period or less return to FIFO.

0.13.26 judged the interval between presents instead, releasing on any one over 1.15 periods and
recovering after 45 within 1.03. An application capping itself at the refresh rate lands on the
period with jitter either side, so a single hitch released it and ordinary jitter delayed
recovery: on an RTX 3060 Ti at 74.97 Hz with frames costing half the period it alternated every
second or so, and each immediate stretch put a stationary tear line near the bottom of the
screen, because presents capped at the refresh rate land at the same scanout phase every frame. GPU timestamps are therefore collected whenever
Adaptive is selected. The refresh period comes from `VK_EXT_present_timing`; until it is known
the swapchain presents immediately.

`VK_KHR_present_mode_fifo_latest_ready` is the last resort, used only where neither of the above
is available. It keeps every present on a vertical blank and discards the images that went stale
waiting for one, so it never queues latency, but a frame that misses one blank still waits for
the next: a workload near the refresh period alternates 1x and 2x refresh intervals, which is the
stutter the policy exists to avoid. A surface lists latest-ready whether or not the device enabled
the feature, so availability is gated on the device rather than on the surface query.

Measured on an RTX 3060 Ti (driver 615.71.09), KDE Wayland, 2560x1440 at 74.97 Hz, in the Isle of
Ran mead-hall trace with a GPU frame of about 14.8 ms: latest-ready alternated 13.34 ms and
26.68 ms intervals (16-18% of frames at two refreshes, sd 4.9-5.1 ms, 64-65 FPS); switching held
14.84 ms median intervals (sd 0.08-0.10 ms, p99 15.0-15.1 ms, 67.4-67.5 FPS). Intervals are frame
completion, not optical display timing. Vulkan validation reported no messages.

Strict/HalfRefresh require FIFO plus `VK_EXT_present_timing` relative-time scheduling, a native
refresh duration, and (for Strict) GPU timestamps. Vulkan's requested relative presentation time
is two native periods with nearest-refresh scheduling. Scheduling continues with zero timing
queries if the diagnostics queue is full, so optional telemetry cannot disable the policy. Vulkan
currently cannot distinguish active VRR from a fixed display; Strict is therefore a fixed-divisor
policy there. No nominal refresh rate is treated as VRR detection. Unavailable native support
remains unavailable in the API/UI.

### Native fixed-refresh transition evidence

Reproduce with `MTL_DEBUG_LAYER=1 target/debug/mulciber-api-conformance --pacing`.
The probe alternates a small textured workload and 22 ms of work after acquisition. On this
Apple M2 fixed 60 Hz display, the last 60 frames of each settled phase reported:

| Phase | Active policy | Median frame-completion interval | Native presentation interval |
| --- | --- | --- | --- |
| Adaptive light | Synchronized | 16.681 ms | 16.667 ms |
| Adaptive slow | Immediate | 23.812 ms | Unlocked; not a uniform scanout claim |
| Adaptive recovery | Synchronized | 16.652 ms | 16.667 ms |
| Strict light | Synchronized | 16.657 ms | 16.667 ms |
| Strict slow | HalfRefresh | 33.313 ms | 33.333 ms |
| Strict recovery | Synchronized | 16.687 ms | 16.667 ms |

The settled Strict slow phase's 60 native presentation intervals span 33.332958–33.333542 ms.
The ordinary Metal API conformance run passes 101 cases with validation enabled, including live
Adaptive/Strict selection. Unit tests cover fractional display rates, workload changes, queued
transitions, and conservative recovery. Windows/Vulkan is compile/lint checked only for these new
policies; native Vulkan, VRR, multi-display, and optical input latency are not established here.


## Strict recovery correction (0.13.24, 2026-09-21)

Strict now starts at full refresh, requires three overload samples to step down,
and recovers with five percent headroom instead of twenty percent. Regressions
cover 85 and 90 FPS workloads on a 75 Hz display, isolated hitches, sustained
overload, delayed GPU results, queued half-rate frames and native submission waits.
Vulkan ends CPU workload timing before queue submission; Strict waits for image
availability before all GPU commands so display waiting cannot inflate its
whole-frame GPU timestamp span.

Validation: Windows graphics unit tests passed (55 passed, 6 native tests ignored),
graphics all-target Clippy passed with warnings denied, and graphics all-target
Apple-silicon macOS cross-compilation passed. No window or game was launched by
the agent. Physical presentation behavior, VRR and multi-display validation remain
outstanding. The user confirmed the consuming game works on Windows / RTX 3060 Ti / 75 Hz, but reported that Strict remains irregular. This release improves the policy; it does not claim Strict pacing is fully resolved.


Release validation for 0.13.24: workspace formatting, all-target compilation,
workspace tests and doctests, and graphics all-target Clippy passed. The package
passed `cargo publish --dry-run` against published mulciber-platform 0.5.5.
`scripts/validate-windows.ps1 -SkipInteractive -InstanceOnly` passed all six
windowless Vulkan instance/device/timestamp tests, with and without validation
layers; logs are in `validation-artifacts/windows-vulkan-20260921-202518/`.
Full workspace Clippy remains blocked by the pre-existing unused
`WindowMetrics::with_display_timing` helper in Windows platform test builds.
The GUI portion of the Windows preflight was not run under the user's no-window
instruction. These checks do not establish smooth Strict presentation.
