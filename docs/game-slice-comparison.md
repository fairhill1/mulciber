# Forge Run Mulciber and wgpu/winit comparison

This comparison holds one small playable result constant while reviewing the outside-in application
experience of Mulciber versus pinned wgpu 30.0.0 and winit 0.30.13.

## Equivalent workload

`mulciber-game-slice` and `wgpu-game-slice` both provide:

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

The Mulciber application obtains snapshots and timing plans from `mulciber-runtime`. The wgpu/winit
application implements the keyboard subset and accumulator locally because neither graphics nor
window library provides that game-loop policy. The comparison does not recreate unused pointer,
scroll, released-membership, configuration-validation, or dropped-time APIs on the wgpu side.

## Raw source counts

These are raw `wc -l` Rust application-source counts. They include comments, blank lines, game tests,
geometry, and equivalent game data. They exclude manifests, Mulciber's artifact-copy build script,
and the shared 67-line WGSL shader.

| Source responsibility | Mulciber | wgpu/winit |
| --- | ---: | ---: |
| Game rules and simulation state | 266 | 268 |
| Window loop, input/timing/lifecycle coordination, and top-level resources | 190 | 252 |
| Geometry, game data, camera, and transforms | 175 | 194 |
| Explicit GPU setup, resources, resize, passes, and submission | included in the 190-line top level | 626 |
| **Total** | **631** | **1,340** |

The near-identical game-rule counts are useful: most of the 709-line difference is integration and
graphics plumbing rather than different gameplay scope. Excluding those equivalent game-rule files,
the outside-in platform/runtime/render portions are 365 Mulciber lines versus 1,072 wgpu/winit lines.

This metric does not compare total implementation size or maturity. It excludes Mulciber's native
backends and runtime implementation just as it excludes wgpu and winit internals. It also does not
credit Mulciber for broader snapshot diagnostics that the workload does not consume. The result is
evidence that the current narrow Mulciber slice makes this particular native desktop game materially
shorter; it is not evidence of broader ecosystem, hardware, or lifecycle superiority.

## Physical checkpoint

On 2026-07-18, `wgpu-game-slice` ran on the Apple M2 / macOS 15.7.7 machine with Metal API
Validation enabled. It selected the Metal backend and four samples. The operator visually and
interactively confirmed that the scene and game behavior matched the runtime-backed Mulciber peer;
the console recorded three crystal collections before normal close. No resize, minimize/restore,
display transition, measured cadence, deterministic readback, Windows, or Linux claim is made from
this focused comparison.

The suspension-matched revision was then relaunched under Metal API Validation. It again selected
Metal and four samples, collected two crystals, and closed without validation diagnostics. The
operator did not explicitly report the hold/minimize/restore result for that second wgpu run, so this
document claims compile-time equivalence and a clean interactive launch/close—not physical wgpu
suspension correctness.
