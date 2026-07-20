# Experimental graphics lifecycle contract

This document records the first graphics decisions extracted into `mulciber`. The types and names are
unstable Gate 2 experiments, not a supported API. Native Metal and Vulkan implementations must consume
the contract before it can expand into resource and command types.

## Crate boundary

Mulciber is the overall native game-development stack. `mulciber-platform` is its desktop OS layer and
owns applications, windows, events, display facts, and borrowed native surface targets. `mulciber` is
its graphics layer and owns GPU selection, devices, queues, resources, synchronization, frame
acquisition, presentation, and graphics shutdown.

The desktop OS layer never owns a GPU object or presentation generation. The graphics layer borrows a
window's opaque surface target without gaining authority to destroy or use the native window from
another thread.

## Decisions established for the first implementation

### Target-selected backend

The supported compilation target selects Metal on Apple-silicon macOS and Vulkan on Windows or Linux.
Ordinary frame work contains no Metal-versus-Vulkan runtime dispatch, and a one-backend build does not
link or initialize the unused backend.

### Window revisions and surface generations are different

`WindowRevision` is desktop-OS input describing drawable metrics. `SurfaceGeneration` is graphics-owned
state describing the lifetime domain of presentable frames and extent-dependent graphics resources.
One does not derive mechanically from the other.

The first native surface configuration starts at generation one. A successfully created replacement
Metal configuration or Vulkan swapchain advances the generation even when its extent is unchanged.
Vulkan's base-swapchain acquired-frame abandonment fallback also advances it because that path replaces
the complete presentation generation. Suspension without graphics reconfiguration does not advance it.

### Acquisition reconfigures internally and has explicit nonfatal outcomes

The shared acquisition outcome distinguishes:

- a ready, surface-scoped frame; and
- temporary unavailability because the window is suspended, the native drawable is absent, a
  bounded acquisition timed out, or extent-driven reconfiguration is deliberately paced where the
  platform's presentation path would otherwise let a continuous resize outrun FIFO presentation.

Reconfiguration for changed metrics happens inside acquisition, so a ready frame always matches the
requested metrics. Its surface information may therefore report a newer generation than the
application's extent-dependent resources; the mismatch is the one rebuild signal, and a draw into
mismatched resources is rejected. A separate reconfigured outcome was tried first and rejected: both
validated native probes already reconfigure inside their own frame machinery, and the separate
outcome made deferring the rebuilt frame to the next redraw the ergonomic default, which physically
measured as window contents trailing a continuous Wayland resize. Under an identical scripted
350-step resize storm, folding reconfiguration into acquisition kept the same paced generation count
(210 versus 212) while presented frames rose from 544 to 1114 because no redraw is spent on a
reconfiguration round-trip.

Native result codes remain structured diagnostics but do not become the ordinary application state
machine.

### VSync pacing comes from the presentation path, not the loop

The platform pump does not throttle. An ordinary loop that presents on every redraw is VSync-bound
because the presentation path itself blocks at display rate: the Metal backend acquires drawables
from a display-synced layer (`setDisplaySyncEnabled:`), and the Vulkan backend presents through a
FIFO swapchain. This is why the examples run without explicit sleeps, and why a minimized or fully
occluded window, whose redraw delivery the platform suspends, presents no frames instead of
spinning.

### Every ready frame has one disposition

A ready frame mutably borrows its surface and cannot outlive that surface generation. Presentation and
explicit abandonment consume the frame and return a fallible result. Metal may release an abandoned
drawable at a backend-owned autorelease boundary. Vulkan uses swapchain maintenance when available and
otherwise replaces and retires the complete generation.

Dropping a frame without consuming it is not undefined behavior and must not strand native ownership.
`Drop` performs best-effort abandonment and records any failure on the borrowed surface; the surface's
next fallible operation reports that deferred failure. Explicit abandonment is the ordinary path
because it can report failure immediately.

### Presentation feedback is drained, identified, and honest about absence

The surface owner exposes `take_present_feedback`, a non-blocking drain of native presentation
completions observed since the previous drain. Each sample identifies its frame by a zero-based
per-session presented index and carries the display time when the native system reported one;
`None` records presentation handling without a display time, which physically occurs while a window
is coming on screen or occluded. Undrained samples are kept in a bounded queue so ignoring feedback
costs fixed memory and no per-frame work. A backend without native feedback answers `Unsupported`
on every drain, so estimation fallbacks are an observable application decision rather than a silent
library substitution. Metal implements feedback through drawable presented handlers registered
before presentation. Vulkan implements it through the `VK_KHR_present_id2` +
`VK_KHR_calibrated_timestamps` + `VK_EXT_present_timing` chain where the adapter and surface
support it, chaining a per-present id and one present-stage timing request and draining completed
reports after every present; a tier without the chain answers `Unsupported`. Vulkan display times
arrive in a swapchain-scoped time domain whose epoch is not the process clock on the surveyed
tier, so each swapchain's times are re-anchored to the drain instant of its first completed
report — intervals between reported times are native-exact within one swapchain and are never
paired across recreations, while absolute placement carries at most one drain latency of bias.
Physical evidence lives in the [Linux runbook](linux-validation.md).

### The clear checkpoint keeps topology private

The first compiling graphics application uses `ClearSurface`, a scoped `ClearFrame`, and a normalized
linear `ClearColor`. `ClearSurface` temporarily owns the target-selected device, queue, command, and
presentation machinery as one private native unit. This proves creation, generation changes,
acquisition, one rendering operation, presentation, abandonment, and shutdown without prematurely
creating general resource or command types.

This collapsed owner is evidence, not the final answer to the open context/device/queue topology.
The representative textured/depth slice must expose uploads, resources, pipelines, and more than one
operation; that pressure will determine which objects deserve independent public ownership.

### Shutdown is explicit

Surface shutdown drains presentation ownership before device shutdown drains remaining GPU work. Both
operations are fallible. `Drop` remains best-effort cleanup for partial construction and unwinding, not
evidence of successful shutdown.

## Current code boundary

The initial checked-in code contains only the shared extent, generation, surface-information,
unavailability, acquisition, and disposition vocabulary. Both native probes consume these facts. At
revision `931b0dc`, the integrated Vulkan path passed the Windows matrix and the integrated Metal path
passed native archive rebuild/reuse, acquired-frame abandonment, and physical resize/lifecycle
validation on an Apple M2. This is cross-backend evidence for the experimental vocabulary, not a
stability claim.

The next checkpoint adds the same-source `examples/clear` application and target-selected native
implementations. On Vulkan, a dedicated acquisition fence makes both presentation and abandonment
legal choices after acquisition; the base-swapchain abandonment path replaces the whole generation,
and old swapchains use the validated deferred-reacquisition retirement rule. On Metal, a scoped
autorelease pool owns the drawable, a labeled command buffer performs an sRGB clear, and the surface
retains that buffer through completion. The Windows automated preflight passed this path on the RTX
3060 Ti tier on 2026-07-17. The same source also abandoned one Metal drawable, recovered for 120
presented frames, and shut down with Metal API Validation enabled on the Apple M2 tier. No general
device, resource, encoder, or pipeline types have been added.

## Still deliberately open

- Whether context, selection, and device opening are separate public values or one constructor flow.
- Rich native diagnostic payloads and physical device-loss/out-of-memory recovery evidence; the
  provisional recovery-oriented error categories are now extracted.
- Upload, resource-use, command-encoding, binding, and shader-artifact vocabulary.
- Safe native capability reach and interoperation.

The clear checkpoint now has validation-enabled finite and physical smoke evidence on Metal plus the
automated Vulkan evidence above. The next implementation step is the representative textured
depth-tested slice.
