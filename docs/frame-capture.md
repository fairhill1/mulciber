# Frame capture (unreleased)

A frame capture reads back the final color of one presented frame: exactly the image the
application rendered and the display receives, after every pass of its submission. The motivating
use is verification screenshots. A desktop screenshot tool captures whichever window has focus,
so it grabs the wrong window while someone is using the desktop; the application itself knows
which frame it rendered.

## API

```rust
impl Surface<'_> {
    pub fn request_frame_capture(&mut self) -> Result<(), GraphicsError>;
    pub fn take_frame_capture(&mut self) -> Result<Option<FrameCapture>, GraphicsError>;
}

impl FrameCapture {
    pub const fn index(&self) -> u64;   // the frame's PresentedFrame::index
    pub const fn width(&self) -> u32;
    pub const fn height(&self) -> u32;
    pub fn pixels(&self) -> &[u8];      // width * height * 4 bytes of RGBA8
    pub fn into_pixels(self) -> Vec<u8>;
}
```

```rust
graphics.surface.request_frame_capture()?;           // before acquiring the frame
if let FrameAcquire::Ready(frame) = graphics.surface.acquire(metrics)? {
    graphics.queue.render_and_present(frame, submission)?; // blocks for this frame only
}
if let Some(capture) = graphics.surface.take_frame_capture()? {
    write_png(capture.width(), capture.height(), capture.pixels());
}
```

The request applies to the next frame the surface acquires; a frame already acquired is not
captured. Capture belongs to the surface because both native mechanisms are fixed when the
presentable image is obtained (see below), and the presenting call stays the ordinary `Queue`
verb, so a capture composes with every submission shape: `render_and_present` with direct or
postprocessed output, HDR composites with or without bloom and volumetrics, foreground content
and the HUD overlay, and the older `draw_*_and_present` verbs. The clear-only `ClearSurface` does
not capture.

When the requested frame is presented, the presenting call copies the presentable image after its
last pass, submits and presents as usual, then **blocks until the GPU has finished that frame**
and converts the copy. The capture is available from `take_frame_capture` as soon as the call
returns. The wait makes the captured frame's timing unrepresentative (the Strict and Adaptive
policies see one slow frame), so this is a screenshot path, not a per-frame readback; frames
without a pending request pay nothing.

A frame that is abandoned, refused before native work, or whose submission or presentation fails
is not captured, and the request stays pending for the next acquired frame. Repeating a pending
request changes nothing. A completed capture waits until it is taken; a later capture replaces one
that was never taken. `index()` is the same zero-based presented-frame index that
`PresentedFrame::index` and GPU timing samples report.

## Pixels

`pixels()` holds `width * height` tightly packed RGBA8 pixels, rows top to bottom and pixels left
to right, where `width` and `height` are the presentable extent the frame was rendered at (its
`SurfaceInfo` extent, physical pixels).

- **Color** is the sRGB-encoded bytes the display receives, whatever the native presentable
  format's channel order: Vulkan `B8G8R8A8_*` and Metal `BGRA8Unorm_sRGB` are reordered, Vulkan
  `R8G8B8A8_*` is copied as is. A linear shader output of 0.214 reads back as 128, not 55.
- **Alpha** is 255 wherever the presentation engine composites the surface opaque, which is what
  the display shows: Vulkan's `VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR` (Mulciber's first choice) or a
  Metal layer whose `opaque` is set. Otherwise it is the stored alpha the compositor blends with.

Mulciber's swapchains are always one of these four-channel 8-bit formats, so there is no format
conversion beyond the reorder.

## Capability

`request_frame_capture` answers `Unsupported`, without recording the request, when the
presentable images cannot be read back: a Vulkan surface whose `supportedUsageFlags` lack
`VK_IMAGE_USAGE_TRANSFER_SRC_BIT`, or a presentable format other than four 8-bit channels. Metal
drawables are always readable once `framebufferOnly` is off.

## Native ownership and synchronization

**Vulkan.** Swapchain images are created with `VK_IMAGE_USAGE_TRANSFER_SRC_BIT` beside color
attachment usage wherever the surface allows it, so a request never rebuilds the swapchain or
advances the surface generation. Acquisition marks the frame when a request is pending. Before
recording, the presenting call allocates a host-visible, host-coherent transfer-destination buffer
of exactly `width * height * 4` bytes. The frame's own command buffer then ends, in place of the
usual attachment-to-present transition, with:

1. a barrier from `COLOR_ATTACHMENT_OPTIMAL` (color attachment output, color attachment writes;
   the last pass is the scene, the MSAA resolve, the postprocess draw or the overlay) to
   `TRANSFER_SRC_OPTIMAL` (copy stage, transfer reads);
2. one `vkCmdCopyImageToBuffer2` of the whole image with zero row length and image height, so rows
   are tightly packed;
3. one dependency holding the transition from `TRANSFER_SRC_OPTIMAL` to `PRESENT_SRC_KHR` after
   the copy, and a memory barrier from the copy's transfer writes to host reads.

The submission signals the render-finished semaphore that presentation waits on, as every frame
does. After `vkQueuePresentKHR`, the call waits on that frame slot's fence, which the next
acquisition of the slot would otherwise wait on, maps and copies the bytes out, and destroys the
buffer. A submitted copy is waited for even when presentation fails, because its buffer cannot be
released earlier. Storage left by a frame whose recording or submission failed never reached the
GPU and is released by the next capture or at teardown.

**Metal.** `CAMetalLayer.framebufferOnly` stays `YES` except while a capture is pending:
`request_frame_capture` sets it to `NO`, so drawables vended from then on are readable, and the
completed capture sets it back. Acquisition marks the frame when a request is pending. After the
frame's last render encoder ends and before `presentDrawable:`, the presenting call encodes one
blit, `copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:`,
from the drawable texture into a new shared-storage buffer with a `width * 4` byte row, in the
frame's own command buffer. After commit it calls `waitUntilCompleted`, reads the buffer contents
when the status is completed, and releases the buffer. Metal tracks the drawable hazard between
the render pass and the blit. A drawable that still reports `isFramebufferOnly` (one the layer
vended before the property change took effect) is not captured, and the request stays pending.
**This path has not run on a Mac.** It passes `cargo clippy` for `aarch64-apple-darwin`, but no
Metal device has captured a frame yet.

## Validation boundary

Unit tests cover BGRA reordering, RGBA pass-through, opaque alpha and the capture size check.

`mulciber-frame-capture` is the native probe. It reuses two checked-in artifacts with both
Vulkan and Metal builds: the material scene's bindingless HUD shader, which draws vertex colors,
and the float-texture probe's HDR composite, which clamps scene color into the presentable target.
Each captured frame is a four-quadrant scene of red, green, blue and orange (`(1, 0.214, 0)`
linear, alpha one half), compared pixel by pixel against the expected encoded bytes, skipping only
pixels within 1.5 pixels of a quadrant edge and allowing one step of sRGB rounding:

- a frame presented without a request, a frame acquired before its request, and an abandoned
  frame acquired under the pending request are not captured, and the request then captures the
  next presented frame;
- the direct path (material records into the presentable target, resolved from four samples by
  default or rendered at one sample with `--force-one-sample`);
- the HDR path, with a red radiance of 4.0 that the composite clamps to 255. The reused composite
  maps clip-space y up onto texture v up, so it presents the scene upside down; the probe expects
  the flipped scene, because that is what the display shows;
- the HDR path with a white overlay record over the bottom-right quadrant, which is drawn in
  presentable space and is not flipped;
- the capture's extent against the frame's, its index against the count of presented frames, and
  that a taken capture is not reported again.

On 2026-10-08, Linux / NVIDIA GeForce RTX 3060 Ti (driver 615.71.09) with Khronos validation
1.4.363 and `vulkan-validation` enabled, all five captures per run matched over nine presented
frames with no validation messages, at four samples and at one, on two surfaces: KDE Wayland,
whose swapchain was `VK_FORMAT_R8G8B8A8_SRGB`, and X11 through XWayland
(`WAYLAND_DISPLAY=` unset), whose swapchain was `VK_FORMAT_B8G8R8A8_SRGB`. The formats were read
with a temporary diagnostic that is not checked in. All four runs also passed with the layer's
synchronization validation (`VK_KHRONOS_VALIDATION_VALIDATE_SYNC=true`,
`VK_KHRONOS_VALIDATION_SYNCVAL_FULL_VALIDATION=true`). Both surfaces composite opaque, so the
stored half alpha always read back as 255; the non-opaque alpha path has unit evidence only. Metal
was not run, and no viability gate is advanced.

```sh
cargo run -p mulciber-frame-capture                        # Linux/Windows; Vulkan validation
cargo run -p mulciber-frame-capture -- --force-one-sample
WAYLAND_DISPLAY= cargo run -p mulciber-frame-capture       # Linux: X11 through XWayland
MTL_DEBUG_LAYER=1 cargo run -p mulciber-frame-capture      # macOS
```
