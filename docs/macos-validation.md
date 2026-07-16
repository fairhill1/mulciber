# macOS AppKit/Metal validation runbook

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
- The evidence machine runs macOS 15.7.7 with a Metal 3 device family. The roadmap's macOS 26 /
  Metal 4 runtime comparison cannot be produced on this machine, and the capability probe reports
  `Metal 4 SDK symbols: unavailable in this build`.
- The evidence machine has a single built-in display. Display-change and multi-display behavior
  cannot be evidenced on it.

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

The ignored validation archive is `validation-artifacts/macos-metal-20260716-210416.tar.gz` with
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

The run must identify the device, memory facts, device families, and each advanced capability
selector explicitly. Record whether Metal 4 SDK symbols were available in the build; on a
pre-macOS-26 SDK the probe must say so rather than omit the section.

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
