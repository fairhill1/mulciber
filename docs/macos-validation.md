# macOS AppKit/Metal validation runbook

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
