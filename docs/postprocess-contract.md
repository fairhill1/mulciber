# Experimental two-pass postprocess contract

This checkpoint adds a second materially different rendering operation to the unstable Gate 2
graphics slice. It renders the existing indexed, textured, depth-tested cube into generation-bound
offscreen color, resolves four-sample color when selected, samples that resolved image in a
single-sample fullscreen pass, and presents the result.

The minimal `mulciber-cube` / `wgpu-cube` pair and the separate input pair remain unchanged. This
work lives in `mulciber-postprocess-cube` and `wgpu-postprocess-cube` so their application source and
line counts remain independently reviewable.

## Public checkpoint vocabulary

`Device::create_postprocess_targets` creates one `PostprocessTargets` handle containing:

- single-sample scene color usable as both a render attachment and sampled input;
- depth storage at the selected scene sample count; and
- four-sample color storage when the negotiated sample count is four.

The handle belongs to one `SurfaceInfo`. Acquisition remains the only surface reconfiguration path,
and the application recreates postprocess targets when an acquired frame reports different surface
information. Backends reclaim superseded target storage under the same completion rules as the
existing direct-to-surface targets.

`Device::create_postprocess_pipeline` loads `post_vertex` and `post_fragment` from the same offline
artifact as the scene pipeline. The post pipeline is always single-sampled and samples resolved
scene color through the shader's texture and sampler bindings.

`Queue::draw_textured_postprocessed_and_present` consumes a frame plus `PostprocessedDraw`. It
validates session identity, target generation, and finite transform data, then records the fixed
sequence:

1. clear offscreen scene color and depth;
2. draw the indexed textured mesh with the selected scene sample count;
3. resolve into single-sample scene color when multisampling is active;
4. make the resolved color readable by the fullscreen fragment stage;
5. draw a fullscreen triangle into the presentation image; and
6. present the consumed frame.

This is intentionally a narrow operation, not a general command encoder or frame graph. One fixed
two-pass consumer establishes intermediate attachment ownership and a real producer-to-consumer
dependency, but it does not yet settle arbitrary pass ordering, multiple draws, load/store
vocabulary, transient allocation, copy/compute integration, or advanced explicit synchronization.

## Native behavior

Metal places both render encoders in one command buffer. A memoryless four-sample texture resolves
into private single-sample scene color; command-encoder ordering makes that color available to the
fullscreen pass. The drawable is presented by the same retained command buffer and checked during
fallible shutdown.

Vulkan allocates scene color with color-attachment and sampled usage. Dynamic rendering writes or
resolves it, then a synchronization2 image barrier changes it from color-attachment output/write to
fragment-shader sampled/read before the fullscreen pass. The swapchain image follows its existing
acquire, color-attachment, present, and retirement path. The Vulkan implementation compiles and
lints for the Windows target, but this checkpoint has no physical Vulkan execution evidence yet.

The single WGSL source is compiled offline by Naga 30.0.0. The Vulkan module was validated for
`vulkan1.4` with the repository-pinned SPIRV-Tools v2026.2 build; source and native artifact hashes
are recorded in `vulkan-toolchain.lock.toml`.

## macOS comparison checkpoint

On 2026-07-17, both postprocess examples ran on the Apple M2 / macOS 15.7.7 machine with
`MTL_DEBUG_LAYER=1`. Both selected four samples, showed the spinning checkerboard cube through the
same desaturation/color-grade and vignette shader, closed through the titlebar, exited zero, and
emitted no Metal validation diagnostics beyond the enabled banner. Screenshots were visually
inspected; no deterministic readback comparison was performed, and resize/minimize behavior was not
recorded in this run.

Raw application-source counts, excluding the shared shader and manifests, are:

| Source | Lines |
| --- | --- |
| `mulciber-postprocess-cube` (`main.rs` + `scene.rs`) | 94 + 74 |
| `wgpu-postprocess-cube` (`main.rs` + `scene.rs`) | 562 + 81 |

The shared four-entry WGSL module is 62 lines. These counts measure application plumbing, not total
implementation cost: Mulciber's private Metal and Vulkan backend code is part of the library and must
be evaluated for maintenance and correctness separately.

## Interactive showcase composition

`mulciber-showcase-cube` and `wgpu-showcase-cube` combine the same postprocess workload with the
existing input controls: W/A/S/D and arrows rotate, primary drag orbits, scroll zooms, Space toggles
spin, and R resets. They are presentation-oriented composability examples, not replacements for the
focused graphics, input, or postprocess pairs.

The Mulciber showcase required no new public or backend API: ordered `mulciber-platform` transitions
update the application-owned orientation/zoom state, which supplies the transform to the existing
`PostprocessedDraw`. The wgpu peer implements the same composition through ordinary `winit` events
and `wgpu` resources. It also checks four-sample support and selects an equivalent one-sample scene
path when necessary.

Raw Rust application-source counts, with the shared shader excluded, are:

| Source | Lines |
| --- | --- |
| `mulciber-showcase-cube` (`main.rs` + `scene.rs`) | 206 + 80 |
| `wgpu-showcase-cube` (`main.rs` + `gpu.rs` + `scene.rs`) | 220 + 530 + 82 |

The wgpu GPU plumbing is split into `gpu.rs` for readability, not excluded from the count.
