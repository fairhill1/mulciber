# First graphics slice: outside-in application flow

This document sketches the complete game-facing flow that the first `mulciber` graphics extraction
must support. It is intentionally non-compiling design material. Names are placeholders, and no type
in this document is a stability promise. The relationships and observable outcomes matter more than
whether the eventual API uses these exact constructors or enum names.

The flow is written before graphics types are implemented so that native Metal and Vulkan object
shapes do not become the public API by accident. It extends the already implemented experimental
[`mulciber-platform` contract](api-platform-contract.md).
The first implemented generation, acquisition, and disposition vocabulary is recorded in the
[experimental graphics lifecycle contract](api-graphics-contract.md).

The compiling `examples/clear` checkpoint deliberately implements only window creation, native
surface-compatible device selection, clear, presentation, reconfiguration, abandonment, and
shutdown. It collapses backend objects into `ClearSurface` so this document's candidate device,
queue, resource, and command topology remains open until the textured/depth implementation puts real
pressure on it.

## Application view

The first slice should let a game express this ownership and lifecycle:

```text
Application ──owns──> Window ──lends──> SurfaceTarget
                                           │
GpuContext ──selects a surface-compatible device from target + requirements
                                           │
                    ┌──────────────────────┼──────────────────────┐
                    v                      v                      v
                  Device                 Queue            Surface<'window>
                    │                      │                      │
              owns resources        submits work       lends scoped frames
```

`GpuContext` is a placeholder for the backend-global state needed before device selection. It may be
a real object, a constructor namespace, or an internal detail if a simpler safe topology survives
both backends. Vulkan needs an instance and native surface before it can prove adapter and present
queue compatibility. Metal can choose a device before creating or configuring a `CAMetalLayer`.
The public flow must support both orders without exposing a fake Vulkan instance to games or silently
choosing a Vulkan device that cannot present to the window.

## Complete candidate flow

The representative slice deliberately includes capability choice, uploads, resize-dependent
resources, temporary surface unavailability, presentation, explicit frame non-presentation, and
fallible shutdown. The ordinary example demonstrates the application path; a companion probe owns
validation-only fallback, non-presentation, and finite-run controls. Shader artifacts are
backend-selected checked-in inputs for this slice; authoring-language policy remains outside its
scope.

```rust,ignore
use mulciber::{
    BufferDescriptor, BufferUse, Capability, Color, DepthTarget, DeviceRequest, FrameAcquire,
    GpuContext, GraphicsPipelineDescriptor, Multisampling, RenderPassDescriptor, SurfaceDescriptor,
    TextureDescriptor, TextureUse,
};
use mulciber_platform::{
    Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent, WindowMetrics,
};

fn run() -> Result<(), GameError> {
    // Platform objects remain main-thread-owned. The game owns the loop.
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber first slice",
        LogicalSize::new(1280, 720),
    ))?;

    // Required capabilities reject the device. Preferred capabilities select an observable
    // fallback rather than silently changing the workload.
    let request = DeviceRequest::new()
        .require(Capability::BaselineGraphics)
        .prefer(Multisampling::samples(4));

    let gpu = GpuContext::new()?;
    let selection = gpu.select_for_surface(window.surface_target(), &request)?;
    eprintln!("selected GPU path: {}", selection.report());
    let selected_samples = selection
        .selected(Multisampling::samples(4))
        .unwrap_or(Multisampling::samples(1));
    let (device, queue, mut surface) = selection.open(SurfaceDescriptor::default())?;

    // Resource descriptors state game intent. Backend placement and native synchronization remain
    // backend policy unless the game opts into a later, explicit advanced path.
    let vertex_buffer = device.create_buffer_with_data(
        BufferDescriptor::new("triangle vertices", BufferUse::Vertex),
        triangle_vertices_as_bytes(),
        &queue,
    )?;
    let texture = device.create_texture_with_data(
        TextureDescriptor::new("albedo", TextureUse::Sampled),
        texture_source(),
        &queue,
    )?;

    let mut surface_info = surface.info();
    let mut depth = DepthTarget::new(&device, surface_info.extent(), selected_samples)?;
    let mut pipeline_format = surface_info.color_format();
    let mut pipeline = device.create_graphics_pipeline(GraphicsPipelineDescriptor {
        color_format: pipeline_format,
        depth_format: depth.format(),
        samples: selected_samples,
        shaders: checked_in_shader_artifacts(),
        ..GraphicsPipelineDescriptor::default()
    })?;

    let mut redraw = None::<WindowMetrics>;
    let mut running = true;
    while running {
        let status = application.pump_events(&window, |event| match event {
            WindowEvent::RedrawRequested(metrics)
            | WindowEvent::RenderingResumed(metrics) => redraw = Some(metrics),
            WindowEvent::RenderingSuspended => redraw = None,
            WindowEvent::CloseRequested => running = false,
            _ => {}
        })?;
        if status == PumpStatus::Exit || !running {
            break;
        }

        let Some(window_metrics) = redraw.take() else {
            continue;
        };

        // WindowRevision is platform-owned input. SurfaceGeneration is graphics-owned output: it
        // may change because of native acquisition results even without another window revision.
        // Reconfiguration happens inside acquisition, so a ready frame always matches the
        // requested metrics and reports the generation dependent resources must match.
        let mut frame = match surface.acquire(window_metrics)? {
            FrameAcquire::Ready(frame) => frame,
            FrameAcquire::Unavailable(reason) => {
                record_temporary_surface_state(reason);
                continue;
            }
        };

        if frame.generation() != surface_info.generation() {
            surface_info = frame.surface_info();
            depth = DepthTarget::new(&device, surface_info.extent(), selected_samples)?;
            if pipeline_format != surface_info.color_format() {
                pipeline_format = surface_info.color_format();
                pipeline = device.create_graphics_pipeline(GraphicsPipelineDescriptor {
                    color_format: pipeline_format,
                    depth_format: depth.format(),
                    samples: selected_samples,
                    shaders: checked_in_shader_artifacts(),
                    ..GraphicsPipelineDescriptor::default()
                })?;
            }
        }

        let mut commands = device.create_command_encoder("main frame")?;
        {
            let mut pass = commands.begin_render_pass(RenderPassDescriptor {
                color: frame.color_attachment(Color::BLACK),
                depth: depth.clear_attachment(1.0),
            })?;
            pass.set_pipeline(&pipeline)?;
            pass.set_vertex_buffer(0, &vertex_buffer)?;
            pass.set_texture(0, &texture)?;
            pass.draw(0..3, 0..1)?;
        }

        match finish_game_frame(&mut frame, commands) {
            Ok(encoded) => queue.submit_and_present(encoded, frame)?,
            Err(error) => {
                // This must be an explicit, safe operation. Metal may release an unsubmitted
                // drawable at a backend-owned autorelease boundary. Vulkan either releases an
                // unused image through swapchain maintenance or replaces its surface generation.
                frame.abandon()?;
                return Err(error);
            }
        }
    }

    // Drop remains best-effort. The ordinary path explicitly drains presentation and GPU work and
    // can report failure before native resources disappear.
    surface.shutdown()?;
    device.shutdown()?;
    Ok(())
}
```

This is intentionally more explicit than the implemented clear-only checkpoint. Convenience constructors
may shorten resource upload or default pipeline setup later, but the complete ownership and recovery
path must remain explainable without hidden global state.

## Native review

| Application step | Direct Metal/AppKit consequence | Direct Vulkan consequence | Required shared outcome |
| --- | --- | --- | --- |
| Create context and target | Connect to AppKit, create a view, later attach a configured `CAMetalLayer`. | Create `VkInstance`, create the platform `VkSurfaceKHR`, then query physical-device and queue presentation support. | Surface-compatible selection is correct without application backend branches. |
| Select required and optional capabilities | Query device families, selectors, formats, sample counts, and resource options. | Query API version, features, extensions, formats, memory, queue families, and surface support. | Required facts reject startup; optional facts produce an inspectable selection report. |
| Create resources and upload | Choose storage modes and encode/copy through Metal buffers, textures, and command buffers. | Choose memory types, stage data, encode copies, and establish the required layout/access dependencies. | Safe resource ownership and game-intent usage produce validation-clean native work. |
| Acquire a frame | Obtain a drawable from `nextDrawable`; temporary absence is normal. | Acquire a swapchain image with synchronization; outdated or suboptimal state may force recreation. | Acquisition reconfigures internally and distinguishes ready, temporarily unavailable, and fatal outcomes; a ready frame reports the generation dependent state must match. |
| Rebuild extent-dependent state | Drawable size follows backing pixels; the game recreates depth and other targets. | Swapchain extent/format/generation and application attachments may all change. | A graphics-owned generation tells the game exactly when its dependent state is stale. |
| Submit and present | Encode presentation on a command buffer and retain it through completion. | Join acquire, render completion, queue submission, presentation, and later presentation retirement. | The ordinary operation consumes both encoded work and the scoped frame without exposing backend synchronization tokens. |
| Abandon a frame | The validated probe releases one drawable at its autorelease boundary and later recovers. | The validated one-shot path acquires with a fence and no reusable presentation semaphore. Swapchain maintenance releases the unused image directly; the base path replaces and retires the complete swapchain generation. Both recover through later presentation. | Non-presentation is explicit and fallible. It may invalidate the surface generation even when platform metrics did not change, and the game does not receive a native release primitive. |
| Shut down | Wait and inspect retained command buffers before releasing resources. | Wait for rendering and presentation ownership before destroying dependent objects. | Explicit shutdown drains the stronger native ownership condition and reports failure. |

The shape is intentionally not a Metal command encoder renamed into a portable namespace, nor a
Vulkan swapchain exposed through friendlier enums. It coordinates the lifecycle that the game must
observe and leaves native mechanisms inside their backends.

## Decisions this sketch makes

- A native presentation target participates in device selection; it is not attached after choosing
  an arbitrary adapter.
- Required and preferred capabilities are separate, and the selected fallback is observable.
- Platform window revisions and graphics surface generations are different facts.
- A presentable frame is scoped and consumed by presentation or an explicit non-presentation path.
- Submission and presentation are coordinated in the ordinary path so applications do not manually
  join Metal completion handlers or Vulkan acquire/render/present primitives.
- Application-owned extent-dependent resources are rebuilt from graphics surface information, not
  raw platform messages.
- Normal shutdown is explicit and fallible; `Drop` is cleanup insurance rather than proof that GPU
  and presentation work completed correctly.
- A selected backend is compiled directly for its target. The ordinary frame path has no runtime
  Metal-versus-Vulkan dispatch.

## Decisions deliberately left open

- Whether `GpuContext`, selection, and opening remain separate public objects or collapse into one
  constructor after the Vulkan and Metal implementations are compared.
- Whether uploads are resource constructors, an explicit upload batch, or ordinary transfer command
  encoding. The first implementation must measure the bookkeeping and synchronization consequences.
- The final resource-use vocabulary and how much backend hazard control belongs in the safe advanced
  boundary.
- Resolved: surface reconfiguration does not return a separate acquisition outcome. Acquisition
  reconfigures internally and a ready frame carries the changed generation; the frame-versus-resource
  generation mismatch is the game's one unambiguous rebuild signal, enforced by draw-time rejection.
  See the [API slice decision ledger](api-slice-decisions.md).
- Whether explicit non-presentation remains named `abandon` or is expressed by another consuming
  operation. The established outcome is that it safely consumes the frame, may advance the graphics
  surface generation, and reports a failure if native recovery cannot complete.
- Whether full occlusion is a surface-unavailable fact, a pacing hint, or only platform visibility
  state. It must not silently stop unrelated game simulation.
- Stabilizing the provisional error categories, device-loss recovery, multi-window routing, and a
  broader owning runtime loop.
- Shader authoring, reflection, binding generation, transient allocation, aliasing, and a full frame
  graph. None is required to validate this first slice.

## Acceptance test for the implementation

The representative compiling example must use the same application source on Metal/AppKit and Vulkan
with Win32, Wayland, or X11. It must contain no ordinary application `unsafe`, report its chosen 4x or
1x sample path, render a textured depth-tested scene, rebuild correctly across lifecycle changes,
handle temporary surface unavailability, and complete fallible shutdown without native validation
output. A companion public-API probe must explicitly exercise frame non-presentation and forced
fallback without adding backend branches to either application path.

Metal-only and Vulkan-only builds are scored separately with portability receiving no credit. This
flow survives only if it removes meaningful unsafe ownership or lifecycle/synchronization burden
without hiding native capabilities, linking the unused backend, or imposing an unexplained material
performance regression.
