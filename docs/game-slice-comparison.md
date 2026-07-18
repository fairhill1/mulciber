# Forge Run Mulciber, direct Metal, and wgpu/winit comparison

This comparison holds one small playable result constant while reviewing the outside-in application
experience of Mulciber versus a practical direct AppKit/Metal Rust stack and pinned wgpu/winit.

The direct stack was pinned before implementation to `metal` 0.33.0, `objc2` 0.6.4,
`objc2-app-kit` 0.3.2, `objc2-foundation` 0.3.2, and `objc2-quartz-core` 0.3.2. It uses public Rust
wrappers rather than Mulciber, winit, wgpu, or hand-written Metal/AppKit declarations. `wgpu` is
pinned to 30.0.0 and `winit` to 0.30.13. The direct implementation is intentionally macOS-only;
portability receives no credit in its single-backend comparison.

## Equivalent workload

`mulciber-game-slice`, `metal-game-slice`, and `wgpu-game-slice` all provide:

- the same top-down arena, collision rules, eight collectible crystals, four moving sentries,
  camera, reset/win loop, movement speed, and diagonal facing;
- W/A/S/D and arrow-key held movement, R edge-triggered reset, and focus-loss clearing;
- a 60 Hz fixed simulation, 250 ms hitch clamp, at most eight catch-up updates, clamped variable
  cosmetic animation, previous/current render interpolation, and rendering suspension without
  catch-up time;
- the same cube and pyramid geometry, five textures and instance batches, depth, preferred 4x MSAA
  with 1x fallback, shader, clear color, and fullscreen postprocess; and
- simulation updates before surface acquisition, so a temporarily unavailable drawable does not
  directly gate game time.

The Mulciber application obtains snapshots and timing plans from `mulciber-runtime`. The direct and
wgpu/winit applications implement the consumed keyboard subset and accumulator locally because
their graphics/window libraries do not provide that game-loop policy. The comparison does not
recreate unused pointer, scroll, released-membership, configuration-validation, or dropped-time APIs.

The direct Metal peer consumes the checked-in metallib payload generated from the same 67-line WGSL
module used by the native Mulciber slice; it does not ship a runtime shader compiler. This holds the
shader program constant and makes the comparison about application integration and rendering rather
than an independently rewritten MSL program.

## Raw source counts

These are raw `wc -l` Rust application-source counts. They include comments, blank lines, game tests,
geometry, and equivalent game data. They exclude manifests, Mulciber's artifact-copy build script,
and the shared 67-line WGSL shader.

| Source responsibility | Mulciber | Direct AppKit/Metal | wgpu/winit |
| --- | ---: | ---: | ---: |
| Game rules and simulation state | 266 | 262 | 268 |
| Window loop, input/timing/lifecycle coordination | included in top level | 263 | 252 |
| Geometry, game data, camera, and transforms | 175 | 188 | 194 |
| Top-level resources and scene submission | 190 | included above | included above |
| Explicit GPU setup, resources, resize, passes, synchronization, and presentation | included in top level | 443 | 626 |
| **Total** | **631** | **1,156** | **1,340** |

The near-identical game-rule counts are useful: most of each difference is integration and graphics
plumbing rather than different gameplay scope. Excluding those equivalent game-rule files, the
outside-in platform/runtime/render portions are 365 Mulciber lines, 894 direct AppKit/Metal lines,
and 1,072 wgpu/winit lines.

This metric does not compare total implementation size or maturity. It excludes Mulciber's native
backends and runtime implementation just as it excludes metal-rs, objc2, wgpu, and winit internals.
It also does not credit Mulciber for broader snapshot diagnostics that the workload does not consume.
The result is evidence that the current narrow Mulciber slice makes this particular native desktop
game materially shorter; it is not evidence of broader ecosystem, hardware, or lifecycle
superiority.

## Developer-cost checkpoint

The following release builds ran sequentially from distinct empty `CARGO_TARGET_DIR` directories on
the Apple M2 machine. Times are single observations from `/usr/bin/time -p`, not statistical build
benchmarks. Package counts are unique normal dependency-tree entries including the application
package and local Mulciber crates. Binary sizes are unstripped Cargo release outputs.

| Measurement | Mulciber | Direct AppKit/Metal | wgpu/winit |
| --- | ---: | ---: | ---: |
| Clean release build, wall time | 3.44 s | 5.56 s | 22.71 s |
| Unique normal dependency packages | 5 | 28 | 79 |
| Release executable | 605,840 B | 560,688 B | 6,399,200 B |
| Unsafe sites in application source | 0 | 5 | 0 |

The direct application's five unsafe sites cover AppKit object initialization, an extern run-loop
mode static, window-close ownership policy, CPU writes into the mapped instance buffer, and the
`CAMetalLayer` attachment. The last site exists because current `metal-rs` and `objc2` wrap the same
Objective-C object through different Rust binding generations. This is a real integration rough edge
in the selected practical stack, not an inherent requirement of every possible Metal binding.
Cargo 1.97 also warns that the `block` 0.1.6 dependency selected by `metal-rs` contains code that a
future Rust release will reject. It did not affect this build or run, but it is retained as a binding
maintenance risk rather than attributed to Metal itself.

Mulciber's smaller dependency graph and faster clean build are partly consequences of owning its
native declarations and implementation in the repository. The source and maintenance cost of that
implementation is intentionally not counted as application code, just as the implementations inside
metal-rs, objc2, wgpu, and winit are not counted. Conversely, the direct executable being about 45 KB
smaller than Mulciber is retained rather than normalized away, but receives negligible decision
weight for a desktop game; it would become material only if it exposed substantial accidental
backend baggage or grew into a meaningful distribution cost.

At the application boundary, the direct path must explain AppKit event polling and native key codes;
view/backing-size and occlusion state; layer/drawable ownership; device, queue, and command-buffer
completion; buffer/texture storage and usage; shader libraries/functions; vertex, render-pipeline,
depth, sampler, and texture descriptors; render-pass load/store/resolve actions; encoders; explicit
resize target retirement; and presentation. Mulciber still exposes its window/event, runtime timing
and snapshot, resource/pipeline, scene-batch/output, acquisition, and shutdown concepts, but removes
the native descriptor and synchronization vocabulary from this ordinary path. The reduction is
material even with portability deliberately excluded.

## Physical checkpoint

On 2026-07-18, `metal-game-slice` ran on the Apple M2 / macOS 15.7.7 machine with Metal API
Validation enabled. It selected four samples. The operator exercised W/A/S/D and arrow input,
diagonal movement/facing, reset, continuous resize, minimize/restore, and normal titlebar close and
reported that it ran correctly. The console recorded three crystal collections and a reset. The
captured output visually matched Forge Run, and no Metal validation warning or error was emitted
through command-buffer drain and shutdown.

On the same date, `wgpu-game-slice` ran on the Apple M2 / macOS 15.7.7 machine with Metal API
Validation enabled. It selected the Metal backend and four samples. The operator visually and
interactively confirmed that the scene and game behavior matched the runtime-backed Mulciber peer;
the console recorded three crystal collections before normal close.

The suspension-matched wgpu revision was then relaunched under Metal API Validation. It again
selected Metal and four samples, collected two crystals, and closed without validation diagnostics.
The operator did not explicitly report the hold/minimize/restore result for that second wgpu run, so
this document claims compile-time equivalence and a clean interactive launch/close, not physical
wgpu suspension correctness.

Claims not made from these runs, for any of the three implementations: display transitions, external
displays, forced 1x, deterministic readback, cadence distribution, CPU/GPU frame timing, memory,
clean pipeline timing, device loss, multi-machine coverage, and Windows or Linux behavior. The wgpu
runs additionally establish no resize or minimize/restore evidence, and the developer-cost
measurements above do not establish runtime performance.

The checkpoint used rustc 1.97.0 and Cargo 1.97.0. The interactive run and sequential clean release
build measurements used these commands, with a distinct previously nonexistent target directory for
each build:

```sh
MTL_DEBUG_LAYER=1 cargo run --manifest-path comparisons/Cargo.toml -p metal-game-slice
CARGO_TARGET_DIR=/tmp/mulciber-clean-native-game-sequential-20260718 \
  cargo build --release -p mulciber-game-slice
CARGO_TARGET_DIR=/tmp/mulciber-clean-metal-game-sequential-20260718 \
  cargo build --release --manifest-path comparisons/Cargo.toml -p metal-game-slice
CARGO_TARGET_DIR=/tmp/mulciber-clean-wgpu-game-sequential-20260718 \
  cargo build --release --manifest-path comparisons/Cargo.toml -p wgpu-game-slice
```

## Current decision

The direct-Metal serious-game checkpoint says **continue**. For this workload, Mulciber materially
reduces application code, unsafe integration, dependency/build cost, and native lifecycle/rendering
bookkeeping even when cross-platform source sharing receives zero credit. Its release binary is
slightly larger than the direct peer (a non-decisive difference here), and runtime-performance
equivalence remains unmeasured.

This checkpoint measures coordination value rather than native necessity. Nothing in this workload
yet shows behavior that a practical coordination layer built above `wgpu`/`winit` could not have
provided; that discrimination belongs to the hidden-control comparisons (live-resize presentation,
pacing and timing feedback, acquired-frame release) and to Gate 4.

This is strong evidence for the practical single-backend portion of Gate 2, but it is not a Gate 2
pass by itself. The comparison plan still requires matched failure diagnosis, frame/cadence and
resource-cost measurements, and the Gate 4 native-differentiation feature/fallback. Other declared
comparison families also remain unevaluated. Those gaps stay open rather than being inferred from a
smooth 26-object game run.
