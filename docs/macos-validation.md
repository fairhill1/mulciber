# macOS AppKit/Metal validation runbook

## Presentation feedback checkpoint

On 2026-07-19, an uncommitted tree based on `edfb792` added presentation-feedback instrumentation
to `mulciber-metal-triangle` and exercised it under `MTL_DEBUG_LAYER=1` on the Apple M2 /
macOS 15.7.7 machine (single built-in 60 Hz display). The probe registers a presented handler on
every presented drawable (a captureless Clang-ABI global block), reads `presentedTime` and
`drawableID` inside the handler, correlates callbacks to submissions by drawable ID, and reports
distributions; `--pacing-csv PATH` writes the per-frame samples and `--load-spike START:COUNT:MILLIS`
injects a fixed CPU stall for the pre-registered load-spike scenario in the
[Gate 4 pacing plan](gate4-pacing-plan.md).

- A steady 300-frame run received a presented callback for all 300 presents (0 unmatched, 0
  pending at exit). 290 callbacks carried a nonzero `presentedTime`; the missing 10 were the first
  frames of the run, before the window was fully on screen. Presented intervals sat on the vsync
  grid at 16.667 ms from min through p99 with 0 missed intervals, matching the queried
  `maximumFramesPerSecond` of 60. Commit-to-present latency was 47.3 to 49.0 ms, about three
  refresh intervals, reflecting this probe's acquire-then-render loop running as far ahead as the
  three-drawable pool allows.
- A 300-frame run with `--load-spike 120:30:40` (a 40 ms stall before each of 30 frames) kept
  every non-spike interval at 16.667 ms while the 30 spike intervals quantized to exactly 2x and
  3x refresh (p50 33.333 ms, max 50.000 ms), never landing between vsync edges. The latency
  minimum dropped to 16.4 ms as the stall drained the drawable queue. One callback arrived out of
  order and was correlated correctly by drawable ID.
- A 120-frame `--abandon-acquired-frame-once` run recovered as before, and the instrumentation
  attributed the abandonment to exactly one 50.000 ms missed interval.

All three runs exited zero with no validation output beyond the enabled banner. This establishes
that per-frame presented-time feedback, refresh cadence, and vsync-quantized degradation are
observable through native Metal on this machine, the Metal half of the pacing plan's probe-first
step. It is single-display, fixed-60 Hz evidence only: ProMotion or external displays, display
changes mid-run, occlusion and resume, and the latency behavior of a paced (rather than
free-running) loop remain unmeasured, and the Vulkan availability survey is untouched.

## Recovery-oriented error checkpoint

On 2026-07-18, an uncommitted tree based on `01a0770` ran `mulciber-api-conformance` with
`MTL_DEBUG_LAYER=1` on the Apple M2 / macOS 15.7.7 machine. All eighteen conformance cases passed.
Every deliberately invalid operation exercised on this backend produced its asserted
`GraphicsErrorKind` as well as the expected contextual diagnostic; the render, abandonment,
resource-reclamation, instancing, fallback, mixed-session, and shutdown cases also completed. Metal
printed no diagnostic beyond its validation-enabled banner. This finite run is error-contract and
presentation evidence, not a new visual, resize, display-change, or interactive lifecycle claim.

## GPU instancing checkpoint

On 2026-07-18, an uncommitted tree based on `15e6aa2` ran `mulciber-instanced-scene` with
`MTL_DEBUG_LAYER=1` on the Apple M2 / macOS 15.7.7 machine. It selected Metal and four samples. A
visually inspected screenshot showed the animated 100-object cube/pyramid field grouped into four
native instance batches, using both checkerboards with depth and the expected final
grade/vignette. Metal emitted no diagnostic beyond the validation-enabled banner. The process was
deliberately interrupted after the visual check, so this is not a close, resize, minimize, or
broader lifecycle pass.

The equivalent `wgpu-instanced-scene` selected wgpu's Metal backend and four samples and showed the
same workload and effect. That peer run did not enable Metal API Validation, and the screenshots
were visually inspected rather than compared through deterministic readback.

The corrected `mulciber-api-conformance` then presented direct and postprocessed two-instance cases,
explicitly reclaimed the new instanced pipeline resource kind, and passed all eighteen assertions
under Metal API Validation with no diagnostic beyond the banner. See the
[GPU instancing contract](instancing-contract.md) for the API boundary, native behavior, source
counts, and remaining physical Vulkan gap.

## Multi-object scene checkpoint

On 2026-07-18, an uncommitted tree based on `a00bb52` ran `mulciber-scene` with
`MTL_DEBUG_LAYER=1` on the Apple M2 / macOS 15.7.7 machine. It selected Metal and four samples. A
visually inspected screenshot showed the animated 100-object cube/pyramid field using both
checkerboards with depth and the expected final grade/vignette. Metal emitted no diagnostic beyond
the validation-enabled banner. The process was deliberately interrupted after the visual check, so
this is not a close, resize, minimize, or broader lifecycle pass.

The same tree then ran `mulciber-api-conformance` under Metal API Validation. Its new direct and
postprocessed two-object cases presented successfully after explicit resource reclamation, and all
sixteen asserted Metal cases passed with no diagnostic beyond the banner. The visually equivalent
`wgpu-scene` peer later selected Metal and four samples on the same machine, but that run did not
enable Metal API Validation. See the [multi-object scene contract](scene-contract.md) for the API,
line counts, and remaining Vulkan gap.

## Two-pass postprocess checkpoint

The separate `mulciber-postprocess-cube` renders the existing textured/depth-tested scene into
resolved offscreen color, samples that image in a fullscreen grade/vignette pass, and presents. Run
it with:

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-postprocess-cube
```

On 2026-07-17, an uncommitted tree based on `ce3cd3c` ran this example on the Apple M2 / macOS
15.7.7 machine. It selected Metal and four samples. An unobstructed screenshot showed the spinning
checkerboard cube with the expected desaturation/color grade and darker window corners, establishing
that the resolved scene texture reached the fullscreen pass with the intended orientation. The
window closed through its titlebar, the process exited zero, and Metal emitted only its
validation-enabled banner.

The equivalent `wgpu-postprocess-cube` then ran on the same machine with `MTL_DEBUG_LAYER=1`, selected
wgpu's Metal backend and four samples, and showed the same shader effect and upright sampling. It
also closed normally with exit code zero and no Metal validation diagnostics beyond the banner. The
screenshots were visually inspected rather than compared through deterministic readback. This run
did not record resize, minimize/restore, abandonment, forced one-sample behavior, other hardware, or
Vulkan execution.

The combined `mulciber-showcase-cube` and `wgpu-showcase-cube` were then launched separately under
the same validation layer. Both selected four samples, rendered the postprocessed interactive scene,
accepted their equivalent keyboard, drag, scroll, spin-toggle, and reset controls to the operator's
satisfaction, closed through the titlebar, and exited zero with no diagnostics beyond the enabled
banner. This establishes the ordinary showcase controls, composition, rendering, and shutdown; it
does not independently establish outside-window release, focus invalidation, key repeat, every
scroll unit, minimize/restore, or resize behavior.

## Input-transition checkpoint

The AppKit-first input experiment delivers physical keys, aggregate modifiers, logical-coordinate
pointer motion and buttons, precise/coarse scroll, and focus transitions through the existing
`mulciber-platform` pump. The separate input cube consumes those transitions directly while the
minimal graphics-only cube stays unchanged; this is lower-level platform evidence, not the future
runtime snapshot API.

Run the interactive cube with Metal validation enabled:

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-input-cube
```

In one captured session, verify W/A/S/D and arrow-key rotation including key repeat, Space
spin toggle, R reset, primary-button drag, release after dragging outside the content area, trackpad
or wheel zoom, and focus loss/reacquisition. Then repeat continuous resize, minimize/restore, full
occlusion/reveal, and titlebar close. Record which scroll hardware was used and whether any key
produced an AppKit alert sound or failed to reach the example. Successful rendering and input
behavior must accompany exit code zero and no Metal validation output beyond the startup banner.

Implementation and unit-test evidence alone must not be recorded as physical input coverage. Text or
IME input, gestures, pressure, gamepads, relative-pointer capture, multi-display behavior, and input
on Win32/Wayland/X11 remain outside this checkpoint.

On 2026-07-17, an uncommitted development tree based on `6eccf2e` ran the new input cube repeatedly
under `MTL_DEBUG_LAYER=1` while the operator physically reviewed it. Every process closed through the
titlebar with exit code zero and Metal emitted only its validation-enabled banner. The first pass
established that translated keys and pointer controls reached the application, but exposed two real
defects: AppKit produced its fallback alert sound for handled keys because the ordinary `NSView` was
not a first responder, and autonomous model spin fought direct manipulation. A custom AppKit content
view now accepts and consumes the already-translated physical key events; the operator confirmed the
alert sound was gone. The input example now starts stationary and Space toggles its independent
automatic spin.

Two later passes exposed an application-math error rather than a platform-coordinate error. Euler
pitch composed inside the model's yaw made vertical drag orientation-dependent; a sign-only change
could make one face appear inverted while another barely moved. Both input comparison examples now
pre-multiply normalized quaternion increments around fixed screen axes. The final pass corrected the
top-left Y sign, and the operator confirmed vertical drag felt correct. This iterative record proves
the final key-responder and vertical-drag paths plus validation-clean rendering and shutdown. It does
not by itself claim that every checklist action above—especially outside-window button release,
focus invalidation, key repeat, minimize/restore, or every scroll unit—was independently observed.
The `wgpu-input-cube` peer compiled and linted on the same Mac. Later the same day the operator ran it
and reported that it worked correctly, establishing a basic interactive smoke but not the complete
input checklist above.

## Render-target reclamation evidence

Revision `884c9d2` introduced stale-generation render-target reclamation into the Metal textured
session after native KDE Wayland resize evidence exposed unbounded per-generation retention (see
the [Linux validation runbook](linux-validation.md)). On 2026-07-17, that revision was pulled onto
the Apple M2 / macOS 15.7.7 machine over SSH: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` all passed,
compiling and linting the changed Metal session natively. With `MTL_DEBUG_LAYER=1`,
`mulciber-api-cube --frames 120 --abandon-acquired-frame-once` selected the 4x path, abandoned one
drawable, recovered, and presented 120 frames; `--frames 120 --force-one-sample` presented 120
frames on the 1x path. Both exited zero with only the validation-enabled banner.

These static-window runs never advanced the surface generation, so they left the Metal reclamation
branch unexercised. Later on 2026-07-17 the operator interactively resized the cube slice on this
machine at `286fcfb` and reported correct behavior with no misrendering or failure, which is the
first physical exercise of Metal generation advancement with reclamation; the exact binary,
validation-layer state, and output were not captured, so this is an operator report rather than a
recorded validation run. The subsequent acquisition reshape (reconfiguration folded into
acquisition) changes this resize path again, so a captured physical resize/lifecycle pass on the
reshaped code remains pending on this machine.

At `7d25d1f` (reconfiguration folded into acquisition), the same machine pulled main over SSH and
passed `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace`, natively compiling the reshaped Metal acquisition path. With
`MTL_DEBUG_LAYER=1`, `mulciber-api-cube --frames 120 --abandon-acquired-frame-once` (4x path,
abandonment, recovery) and `--frames 120 --force-one-sample` each presented 120 frames and exited
zero with only the validation-enabled banner. These were again static-window runs; the physical
resize/lifecycle pass on the reshaped acquisition remains pending.

## Conformance probe evidence

The first `mulciber-api-conformance` run on this machine (2026-07-17, `MTL_DEBUG_LAYER=1`) caught
a fatal Metal-only defect no earlier run had reached: the textured session stored a dropped
frame's token — autorelease pool included — for a deferred flush, so the pool outlived its
enclosing AppKit autorelease scope and Objective-C aborted with an invalid pool-nesting error the
first time the Drop-abandonment path executed. The fix releases the drawable inline in
`defer_abandon`, since Metal abandonment is an infallible drawable release at the token's
autorelease boundary and there is nothing to defer. After the fix the probe passes twelve asserted
cases under Metal API validation with exit zero. Metal runs the stable-generation branch
(abandonment does not replace the generation), while the Linux Vulkan driver's base-swapchain
abandonment replaces it and asserts the superseded-target rejection as a thirteenth case — the
same game-facing outcomes over different native machinery.

On 2026-07-18, the resource-lifetime development tree ran `mulciber-api-conformance` on this Apple
M2 / macOS 15.7.7 machine with `MTL_DEBUG_LAYER=1`. The probe explicitly destroyed mesh, texture,
textured pipeline, postprocess pipeline, direct targets, and postprocess targets; then it reclaimed
32 dropped meshes through reusable generational slots and presented successfully with replacement
resources. All fifteen Metal cases passed, both fallible shutdowns succeeded, the process exited
zero, and Metal emitted no diagnostic beyond its validation-enabled banner. This establishes the
new lifetime behavior for the current Metal slice only; Vulkan physical validation is not inferred.

Later on 2026-07-17, the platform pump-contract reshape (fallible event handler, platform-owned
`wait_for_first_metrics`, const `ClearColor::opaque`) was validated on this machine over SSH as
an applied patch of the uncommitted tree based on `ccfc4d7` (the checkout was then restored
clean): workspace clippy and tests passed natively, and with `MTL_DEBUG_LAYER=1` the conformance
probe repeated its twelve Metal cases, `mulciber-api-cube --frames 120` presented 120 frames,
`mulciber-api-clear --frames 60 --abandon-acquired-frame-once` recovered through abandonment, and
`mulciber-metal-triangle --abandon-acquired-frame-once --frames 120` abandoned one drawable and
submitted 120 frames at 0.834 ms average GPU frame time. All exited zero with only the
validation-enabled banner; these are finite static-window runs. The checkout on this machine was
renamed from `~/dev/zinc_platform-cube` to `~/dev/mulciber` the same day.

## Single-backend build evidence

At revision `7d25d1f` on the Apple M2 (8 cores, macOS 15.7.7, Rust 1.97.0, default release
profile), the macOS build of `examples/cube` was measured as the Metal-only single-backend data
point:

- `cargo tree` matches Linux: `mulciber` depends only on `mulciber-platform`, which depends on
  nothing; the example adds `glam` as its own math choice.
- `cargo clean` followed by `cargo build --release -p mulciber-cube` completed in 1.3 seconds of
  wall clock.
- The produced binary is 519,824 bytes as built and 412,496 bytes stripped.
- `otool -L` lists only `libobjc`, Metal, QuartzCore, `libSystem`, AppKit, and CoreFoundation.
  No Vulkan loader, MoltenVK, or other graphics library is referenced.
- The binary contains zero Vulkan symbols or strings (`vkCreateInstance`, `libvulkan`, `VK_KHR`,
  `VkDevice` all absent); the Vulkan backend module is excluded at `cfg(target_os)` level, so it
  is not compiled, linked, initialized, or reachable.
- Backend dispatch is compile-time module aliasing (`crates/mulciber/src/backend/mod.rs`); the
  ordinary frame path contains no backend-selection branch, table, or trait object.

The Vulkan-only mirror of this record is in the Linux runbook's single-backend build evidence.

## Textured cube checkpoint evidence

The resource-backed same-source cube was compiled and linted natively on the Apple M2 / macOS
15.7.7 machine on 2026-07-17. `mulciber-shader` compiled the single WGSL module through Naga 30 to
MSL 3.1 and Xcode linked the cached metallib artifact. Its SHA-256 is
`986cd8c811af3a7686ba2a08d76b78897c51a4bb174c9b1834d0ae8fddfb5345`.

With `MTL_DEBUG_LAYER=1`, the preferred 4x MSAA path uploaded indexed cube geometry and an RGBA8
sRGB checkerboard, created depth and memoryless multisample targets, explicitly abandoned one
drawable, recovered, presented 240 frames, and drained shutdown. The forced 1x path then presented
120 frames. Both runs exited zero and Metal emitted only its validation-enabled banner. This is
finite native execution evidence, not user-confirmed visual correctness, resize/lifecycle,
multi-display, or broader Apple hardware evidence.

## Clear checkpoint evidence

A clear-only Gate 2 checkpoint based on revision `2d24f8f` plus the uncommitted extraction changes
was compiled and linted natively on 2026-07-17 on the Apple M2 / macOS 15.7.7 machine below. The
same-source `mulciber-clear` application initially exposed an AppKit startup difference: drawable
metrics were not yet available immediately after showing the window. Startup now obtains the first
metrics through the platform event contract instead of assuming synchronous availability.

With that correction, this command enabled Metal API Validation, explicitly abandoned one acquired
drawable, recovered for 120 presented clear frames, and completed fallible shutdown without Metal
diagnostics:

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-clear -- \
  --frames 120 --abandon-acquired-frame-once
```

That command records the tested revision. Validation-only controls have since moved unchanged to
`mulciber-api-clear`; the ordinary `mulciber-clear` example is now interactive-only.

The user then ran the requested interactive clear smoke independently with `MTL_DEBUG_LAYER=1` and
reported that it worked without an issue. The observed solid blue/teal output is the intended
full-surface clear, not a missing triangle. This records a validation-enabled physical smoke of the
new application; no separate output archive was captured. Display change, multi-display/backing-scale,
and broader Apple-silicon/macOS tiers remain untested by this checkpoint.

The macOS probes exercise Metal directly through the Objective-C runtime with no Rust package
dependencies. This runbook captures the physical evidence required for the AppKit presentation
milestone and records what has actually been exercised.

## Current status

- The capability probe (`mulciber-metal-info`) and the presentation/resource probe
  (`mulciber-metal-triangle`) compile, lint, and run on Apple silicon.
- Automated finite-run evidence with Apple's Metal API validation layer was recorded on
  2026-07-16 on an Apple M2 (see below), covering the cold binary-archive generation run and the
  strict cross-process load run.
- Initial physical lifecycle evidence (continuous drag resize, minimize/restore, zoom/restore,
  full occlusion/reveal, titlebar close) was recorded on the same machine and date; see below.
- A deterministic acquired-frame abandonment run on the same M2 acquired one drawable without
  submission or presentation, drained its per-frame autorelease pool, recovered for 120 submitted
  frames, and shut down cleanly under Metal API validation; see below.
- The first experimental API extraction moved AppKit application, window, event pumping, drawable
  metrics, and window metric revisions into `mulciber-platform`. Development runs based on revision
  `449c01c` exercised the full Metal probe, acquired-frame abandonment, and a new physical lifecycle
  pass through that boundary; see below. No new display coverage is inferred.
- The first experimental graphics lifecycle extraction is now natively validated at revision
  `931b0dc`: the Metal probe consumed `mulciber` surface generations, acquisition outcomes, and frame
  dispositions through finite archive, abandonment, and physical lifecycle passes; see below.
- The primary evidence machine runs macOS 15.7.7 with a Metal 3 device family. The roadmap's
  macOS 26 / Metal 4 runtime comparison was recorded against a second machine (Apple M5, macOS
  26.5.2); see below. The capability probe detects Metal 4 objects through the Objective-C
  runtime, so the report does not depend on the build SDK; the probe itself still compiles no
  Metal 4 SDK symbols.
- Both evidence machines have a single built-in display. Display-change and multi-display
  behavior cannot be evidenced on them.

## Recorded evidence

Revision `8e62d02b537593eafd365c0d598780542f7538cf` was exercised on 2026-07-16 on a MacBook Air
with an Apple M2 (8 GPU cores, unified memory, Metal 3 family, argument buffers tier 2), macOS
15.7.7 build 24G720, a single built-in 2560x1664 Retina display, and Rust 1.97.0. The working tree
was clean before capture.

The structural preflight (`cargo fmt --all -- --check`, `cargo check --workspace --all-targets`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
`git diff --check`) passed.

The capability report ran in both human-readable and JSON forms. It identified the Apple M2,
unified memory, a 5.33 GiB recommended working set, 4.00 GiB maximum buffer length, argument
buffers tier 2, read-write texture tier 2, families Apple 7/8, Mac 2, Common 3, and Metal 3
(Apple 9 unsupported), and ray tracing, function pointers (including in render), and dynamic
libraries all available. The JSON report parsed without repair as schema version 1.

Two automated 600-frame `mulciber-metal-triangle` runs completed under `MTL_DEBUG_LAYER=1` with
exit code zero and no validation output beyond the "Metal API Validation Enabled" banner:

- A `--rebuild-binary-archive` cold run generated `target/mulciber-metal-pipelines.metalarc` with
  4 strict pipeline hits, 0.861 ms pipeline creation, and 0.875 ms average GPU frame time.
- A fresh strict process loaded that archive with 4 strict hits
  (`MTLPipelineOptionFailOnBinaryArchiveMiss`), 0.499 ms pipeline creation, and 0.841 ms average
  GPU frame time.

Both runs exercised the full workload: BC1 upload and compute decompression with base and 1x1
mip-tail readback verification, compute-written storage buffer readback, indexed-indirect drawing
from a native argument buffer, triple-buffered uniforms, shadow depth, memoryless 4x MSAA scene
resolve, and the fullscreen post pass. Shutdown came from the `--frames` limit, so these runs
establish no physical lifecycle evidence.

### Initial physical lifecycle evidence

An interactive session on the same machine, date, and revision ran without `--frames` under
`MTL_DEBUG_LAYER=1`. The probe's source revision was clean; the working tree contained only this
uncommitted runbook. The user physically exercised continuous drag resize including very small
sizes, minimize to the Dock and restore, zoom and restore, full occlusion behind another
application and reveal, and titlebar close, and reported that everything looked good with no lag
or artifacts. The process loaded the binary archive with 4 strict hits, rendered 5,840 frames at
0.917 ms average GPU frame time, and exited with code zero and no validation output beyond the
startup banner, draining retained in-flight command buffers during shutdown.

This establishes initial single-display lifecycle evidence on one Apple M2 machine. Display
change, multi-display, differing backing scale factors, explicit input handling, the macOS 26 /
Metal 4 runtime, and broader Apple-silicon hardware coverage remain outstanding. Rendered resize
cadence was accepted visually and was not instrumented or measured.

### Acquired-frame abandonment evidence

On 2026-07-16, a development tree based on revision
`fce165e5878db5bcc86f3c41ed5194688c4b8b18` added and exercised the deterministic
`--abandon-acquired-frame-once` path on the same Apple M2 and macOS 15.7.7 machine. The final run
was:

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle -- \
  --abandon-acquired-frame-once --frames 120
```

The probe loaded the existing binary archive with four strict hits, acquired a drawable and
accessed its texture, then intentionally created no command buffer and scheduled no presentation
for that drawable. Returning from the iteration drained its autorelease pool. The next acquisition
succeeded, 120 later frames were submitted, retained command buffers drained at shutdown, and the
process exited successfully. Average GPU frame time was 0.830 ms over those 120 submitted frames.
The validation layer printed nothing beyond its enabled banner.

This establishes the Metal behavior of one intentionally abandoned acquired frame followed by
continued rendering. It does not establish repeated abandonment under pressure, abandonment
during resize or occlusion, or the corresponding Vulkan acquired-image behavior. The validation
archive and exact development-tree status are recorded below.

The ignored archive is
`validation-artifacts/macos-metal-abandon-frame-20260716-222617.tar.gz` with SHA-256
`75f83ef29373a84aab113755e55d92d6dbbd6776fddb582d7a611ecfefd6fca9`. It contains the
environment, the display inventory returned during the run, and the verbatim validation log.

### Experimental platform extraction regression

On 2026-07-16, a development tree based on revision
`449c01cb1997fedd674a4a58bd0105f141a3317b` moved AppKit application/window creation, event
dispatch, drawable extent and backing-scale queries, suspension policy, and window metric revisions into
the experimental `mulciber-platform` API. The full Metal probe consumed that boundary while retaining
its existing Metal renderer. Two validation-enabled runs completed on the same M2/macOS 15.7.7
machine:

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle -- --frames 3
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle -- \
  --abandon-acquired-frame-once --frames 120
```

The final smoke run loaded four strict binary-archive hits, submitted three frames, reported 0.947 ms
average GPU frame time, and exited zero. The second loaded four strict hits, abandoned exactly one
acquired drawable, recovered for 120 submitted frames, reported 1.061 ms average GPU frame time, and
exited zero. Neither run emitted Metal validation output beyond the enabled banner.

This is development-tree regression evidence for the first extraction, not a clean-revision archive.
It proves that finite rendering and the targeted drawable-abandonment behavior survive the new
platform boundary.

The same development tree then ran interactively without `--frames`. After approximately four minutes
idle, the user physically exercised continuous resize including very small sizes, minimize/restore,
zoom/restore, full occlusion/reveal, and titlebar close. The process loaded four strict binary-archive
hits, submitted 6,504 frames at a reported 0.917 ms average GPU frame time, and exited zero with no
Metal validation output beyond the enabled banner. No visual artifacts or lag were reported. This
repeats the physical lifecycle pass through the extracted boundary on the single-display M2; it does
not establish display-change or multi-display behavior, and the console output was observed during
development rather than preserved in a new validation archive.

A later development tree based on `d68817b0635af3d5bdd634a6b0d215190603b317` replaced the initial
visibility-based closure check with an owned AppKit delegate, allowing hidden or ordered-out windows
to suspend rather than terminate. It also made the metrics delivered with `RedrawRequested` the Metal
probe's direct render input. A validation-enabled `--frames 3` smoke run loaded four strict
binary-archive hits, submitted three frames at 0.879 ms average GPU frame time, and exited zero. A
targeted validation-enabled run then abandoned one acquired drawable, recovered, submitted 120 later
frames at 0.951 ms average GPU frame time, and exited zero. Neither emitted validation output beyond
the enabled banner. This is automated construction, rendering, and exceptional non-submission evidence
only; hide/restore and titlebar close must be repeated physically before claiming those behaviors for
the delegate-backed revision.

### Experimental graphics lifecycle extraction regression

Revision `931b0dc6b03540818e918784e44ca1ad78bbaf0e` was exercised on 2026-07-17 on the
Apple M2 MacBook Air described above: macOS 15.7.7 build 24G720, 8 GPU cores, one built-in
2560x1664 Retina display, and Rust 1.97.0. The working tree was clean before capture. The structural
preflight, including `git diff --check`, passed natively.

This revision made the Metal probe consume `mulciber`'s experimental physical surface extent,
graphics-owned surface generation, acquisition outcome, and frame disposition vocabulary. Three
finite runs completed with `MTL_DEBUG_LAYER=1` and no validation output beyond the enabled banner:

- The 600-frame archive-rebuild run generated four strict pipeline hits, reported 0.368 ms pipeline
  creation and 1.002 ms average GPU frame time, and exited zero.
- A fresh 600-frame process loaded the archive with four strict hits, reported 0.497 ms pipeline
  creation and 0.923 ms average GPU frame time, and exited zero.
- The acquired-frame abandonment path loaded four strict hits, abandoned exactly one drawable,
  reported recovery after later submission, completed 120 submitted frames at 0.953 ms average GPU
  frame time, and exited zero.

An interactive run then loaded four strict hits and ran under the same validation layer. The user
physically exercised continuous resize including very small sizes, minimize/restore, zoom/restore,
full occlusion/reveal, and titlebar close, and reported correct rendering with no visual or lifecycle
issue. Shutdown exited zero after 1,906 rendered frames at 0.942 ms average GPU frame time, with no
validation output beyond the enabled banner.

This establishes single-display physical regression evidence for the extracted platform and graphics
lifecycle boundaries on one Apple M2. It does not establish display-change, multi-display, a differing
backing scale factor, an explicit zero-sized drawable, Metal 4 rendering, or broader Apple-silicon
hardware coverage. Resize cadence was accepted visually and was not instrumented.

The ignored evidence archive is
`validation-artifacts/macos-graphics-lifecycle-20260717-151027.tar.gz` with SHA-256
`adb727443397184de2c0ae70ec883068bb7ba41068aca6ead3022c453c1390e1`. It contains the exact
revision/toolchain status, OS and display inventory, and verbatim finite and interactive logs.

### macOS 26 / Metal 4 capability comparison

On 2026-07-16 the capability report ran on a second machine: a MacBook Air with an Apple M5
(8 GPU cores, Metal 4 per `system_profiler`), macOS 26.5.2 build 25F84, a single built-in
2560x1664 Retina display, Rust 1.97.1, and Command Line Tools with the macOS 26.5 SDK. The source
was the GitHub zip of `main` matching `90602d54aa0dceb9006aa04f6a0cd143e7a9326f` plus the updated
`probes/metal-info/src/main.rs` that adds Metal 4 runtime-symbol detection, copied from the
development tree before commit; it is therefore development evidence for the probe change. The
run was driven over SSH; the capability probe opens no window, so a graphical session was not
required.

The same updated probe ran on the Apple M2 / macOS 15.7.7 machine as the comparison baseline and
negative control. Differences between the two reports, both of which parsed without repair as
schema version 1:

- Device: Apple M2 versus Apple M5 (both 8 GPU cores, unified memory).
- Recommended working set: 5.33 GiB versus 11.84 GiB; maximum buffer length: 4.00 GiB versus
  8.88 GiB.
- Families: the M5 adds Apple 9; both support Apple 7/8, Mac 2, Common 3, and Metal 3.
- Metal 4 runtime symbols: absent on macOS 15 (all six report `no`) and present on macOS 26 (the
  `newMTL4CommandQueue` device selector and the `MTL4CommandQueueDescriptor`,
  `MTL4CommandAllocatorDescriptor`, `MTL4ArgumentTableDescriptor`, `MTL4CompilerDescriptor`, and
  `MTL4PipelineDataSetSerializerDescriptor` classes all report `yes`).
- Everything else was identical: argument buffers tier 2, read-write texture tier 2, 32.00 KiB
  maximum threadgroup memory, and all five advanced selectors available.

This establishes capability-report evidence only. No Metal 4 object was created, nothing was
rendered on the M5, and the triangle probe has not run there. The comparison archive is
`validation-artifacts/macos-metal4-compare-20260716-213831.tar.gz` with SHA-256
`7608855475b87d61b1e1980b7c632cb7f6b9ef56e9ce1dbea499a0ad89a27a10`, containing both machines'
environments, display inventories, and human-readable and JSON reports.

The ignored validation archive for the M2 lifecycle evidence is
`validation-artifacts/macos-metal-20260716-210416.tar.gz` with
SHA-256 `0cd30bcb6ad2540632362ccdce2ab46f8f44466587cdb2196e22115551a3c3fb`. It contains the
environment, display inventory, capability reports, and all run logs, including the interactive
session.

## Machine requirements

- Apple silicon Mac on a supported macOS release (Metal 3 baseline).
- Xcode or the Command Line Tools providing the `metal` and `metallib` build tools; the build
  script compiles and embeds the shader library, so no shader compiler runs in the probe.
- Rust 1.97 or the repository-pinned compatible toolchain.

Record the environment before running:

```sh
sw_vers
uname -a
rustc --version --verbose
system_profiler SPDisplaysDataType
```

Preserve the full `system_profiler SPDisplaysDataType` output. It records the GPU core count,
Metal support tier, and every attached display, which later capability comparisons need.

## Structural preflight

From the repository root:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Passing this preflight does not prove that presentation or lifecycle behavior works on the
machine.

## Capability run

```sh
cargo run -p mulciber-metal-info
cargo run -q -p mulciber-metal-info -- --json | tee mulciber-metal.json
python3 -c "import json,sys; r=json.load(open('mulciber-metal.json')); \
assert r['schema_version'] == 1 and r['backend'] == 'metal'"
```

The run must identify the device, memory facts, device families, each advanced capability
selector, and each Metal 4 runtime symbol explicitly. Metal 4 detection queries the Objective-C
runtime, so it reflects the running OS rather than the build SDK; on a pre-macOS-26 system every
Metal 4 entry must report `no` rather than disappear from the report.

## Automated presentation runs

Run the finite matrix with Apple's Metal API validation layer enabled:

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle -- --rebuild-binary-archive --frames 600
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle -- --frames 600
```

The first run must report that it generated the binary archive with strict hits for all four
pipelines (shadow, main, post, compute). The second, as a fresh process, must report that it
loaded the archive with strict hits, proving cross-process reuse under
`MTLPipelineOptionFailOnBinaryArchiveMiss`. Every startup readback check (BC1 or expanded texels,
storage buffer, mip tail) fails the run on any mismatch. Success means exit code zero, the
animated textured scene was visible, and the validation layer printed nothing beyond its startup
banner. These finite runs shut down through the frame limit and are not lifecycle evidence.

Exercise the acquired-but-unsubmitted frame path separately:

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle -- \
  --abandon-acquired-frame-once --frames 120
```

The run must report exactly one abandoned drawable, then report recovery after later rendering was
submitted. The frame limit counts only submitted frames, so this proves acquisition recovered
rather than allowing the abandoned iteration to satisfy the limit.

Exercise the presentation-feedback scenarios from the [Gate 4 pacing plan](gate4-pacing-plan.md):

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle -- \
  --frames 300 --pacing-csv target/pacing-steady.csv
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle -- \
  --frames 300 --load-spike 120:30:40 --pacing-csv target/pacing-spike.csv
```

Every run prints the presentation-feedback report. The steady run must show presented callbacks
for every present with no unmatched callbacks or pending submissions at exit, presented intervals
on the display's nominal vsync grid, and zero missed intervals; a small number of startup frames
reporting no presented time is expected while the window comes on screen. The load-spike run must
keep non-spike intervals nominal while spike intervals quantize to whole multiples of the refresh
interval. Record the reports and CSVs with the run.

## Interactive lifecycle pass

Run without `--frames`:

```sh
MTL_DEBUG_LAYER=1 cargo run -p mulciber-metal-triangle
```

Then physically exercise, in one session:

1. Drag-resize continuously from every edge and corner, including very small sizes.
2. Minimize to the Dock for several seconds and restore. Rendering must pause while miniaturized
   (the probe skips rendering when miniaturized or fully occluded) and resume on restore.
3. Zoom (green button or double-click the titlebar) and restore.
4. Fully occlude the window behind another app for several seconds, then reveal it.
5. If multiple displays are available, move the window between displays, including displays with
   different backing scale factors.
6. Close with the titlebar close button; the event loop must exit and shutdown must drain and
   release every retained in-flight command buffer before printing the GPU timing summary.

The scene must remain stable and proportion-correct during resize with no stale or stretched
frames, remain VSync-paced, resume correctly after minimize and occlusion, and shut down with
exit code zero and no validation output. Record apparent resize lag, pacing anomalies, or visual
artifacts even when the run otherwise passes. Note that the probe has no Quit menu item; titlebar
close is the supported shutdown path.

## Success criteria

- Every command exits with code zero without panic or Objective-C exception.
- The Metal validation layer prints nothing beyond its startup banner.
- All startup GPU-to-CPU readback verifications pass.
- Binary-archive generation and strict cross-process loading both report hits for all four
  pipelines.
- The acquired-frame abandonment run reports one abandonment followed by a later submission.
- The interactive pass completes every listed action with correct rendering before, during, and
  after each one.

## Evidence to preserve

- macOS product version and build, hardware model, GPU, and display inventory.
- Rust toolchain version.
- Human-readable and JSON capability reports.
- Console output from every run, including the interactive session, verbatim.
- Exact Git revision and working-tree status.
- Which lifecycle actions were physically performed, and which (display change, multi-display,
  Metal 4 runtime) remain unexercised on the machine.

Do not claim multi-display or display-change coverage from a single-display machine, Metal 4
coverage from a Metal 3 device or pre-macOS-26 SDK, or lifecycle coverage from `--frames` runs.
Validation archives belong under `validation-artifacts/` and are not source files.
