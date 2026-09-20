# Presentation choice and explicit frame caps

Release versions: graphics 0.13.20 and runtime 0.5.4.

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
There is no automatic 60-to-30 downgrade and no rounding to refresh divisors.

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
