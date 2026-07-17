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

### Acquisition has explicit nonfatal outcomes

The shared acquisition outcome distinguishes:

- a ready, surface-scoped frame;
- temporary unavailability because the window is suspended, the native drawable is absent, or a
  bounded acquisition timed out; and
- successful graphics reconfiguration that requires application-owned extent-dependent resources to
  be rebuilt before acquiring again.

Native result codes remain structured diagnostics but do not become the ordinary application state
machine.

### Every ready frame has one disposition

A ready frame mutably borrows its surface and cannot outlive that surface generation. Presentation and
explicit abandonment consume the frame and return a fallible result. Metal may release an abandoned
drawable at a backend-owned autorelease boundary. Vulkan uses swapchain maintenance when available and
otherwise replaces and retires the complete generation.

Dropping a frame without consuming it is not undefined behavior and must not strand native ownership.
`Drop` performs best-effort abandonment and records any failure on the borrowed surface; the surface's
next fallible operation reports that deferred failure. Explicit abandonment is the ordinary path
because it can report failure immediately.

### Shutdown is explicit

Surface shutdown drains presentation ownership before device shutdown drains remaining GPU work. Both
operations are fallible. `Drop` remains best-effort cleanup for partial construction and unwinding, not
evidence of successful shutdown.

## First code boundary

The initial checked-in code contains only the shared extent, generation, surface-information,
unavailability, acquisition, and disposition vocabulary. Both validated native probes must use these
facts before `mulciber` gains device, resource, encoder, or pipeline types. This keeps the first public
surface small enough to delete or redesign if the native integrations expose a mismatch.

## Still deliberately open

- Whether context, selection, and device opening are separate public values or one constructor flow.
- Final error categories and native diagnostic payloads.
- Whether reconfiguration remains a separate acquisition outcome or a ready frame carrying changed
  surface information.
- Upload, resource-use, command-encoding, binding, and shader-artifact vocabulary.
- Safe native capability reach and interoperation.

The next implementation step is a validation-clean clear-only application through both native
backends, followed by the representative textured depth-tested slice.
