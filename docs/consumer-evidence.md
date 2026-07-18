# Consumer evidence from five real wgpu/winit games

On 2026-07-18, five pre-existing games by the Mulciber maintainer were surveyed by code reading.
They were written on `wgpu`/`winit` before and independently of Mulciber's design, so unlike the
pre-registered comparison workloads, none of them was shaped by Mulciber's API. That makes them the
first evidence about what real games in this portfolio actually need, use, and repeatedly rewrite.

This is code reading, not measurement: no benchmarks, traces, or physical runs were performed for
this document, and all five games share one author, so this is portfolio evidence rather than a
survey of independent developers. Surveyed revisions: `portals_rust` `4ac275d` (a portal-mechanics
first-person prototype), `fw_rust` `f5ba9ca` (an open-world squad RPG prototype), `rust_voxel`
`252ea99` (a voxel sandbox), `das_bootleg` `a655f5a` (a first-person horror prototype), and
`asteroid_rodeo` `b91bd9f` (a released-track 3D game with Steam integration). File:line citations
below refer to those revisions.

## Portfolio overview

| | portals_rust | fw_rust | rust_voxel | das_bootleg | asteroid_rodeo |
| --- | --- | --- | --- | --- | --- |
| Source lines (approx.) | 5.2k | 38k | 45k | 21k | 131k |
| wgpu / winit | 27 / 0.30 | 27 / 0.30 | 28 / 0.30 | 29 / 0.30 | 29 / 0.30 |
| Native features requested | none | none | none stated | none | timestamp query |
| MSAA | none | none | none | none | optional 4x |
| Compute | none | none | cull + Hi-Z | fluid, sky LUTs, skinning | none |
| Indirect / GPU-driven | no | no | multi-draw indirect | no | no |
| Surface-loss handling | partial | none (`unwrap`) | yes | yes | partial |

Five projects sit on four different `wgpu` majors (27, 27, 28, 29, 29), so upstream migration cost
recurs per project inside one portfolio.

## Unanimously reimplemented glue

Every one of the five games hand-rolls the same runtime policy that `mulciber-runtime` extracts:

- A fixed-timestep accumulator near 60 Hz with a ~250 ms hitch clamp and render interpolation
  appears in all five (`portals_rust` `game_logic.rs:53`, `fw_rust` `engine/mod.rs:30-31,353-363`,
  `rust_voxel` main accumulator, `das_bootleg` `app/mod.rs:730-737`, `asteroid_rodeo`
  `app_tick.rs:316-336`).
- The cursor-capture fallback hack, `CursorGrabMode::Locked` versus `Confined` tried in one order or
  the other, appears in all five; `mulciber-platform` currently has no pointer-capture API.
- All five use borderless windowed fullscreen or none at all; none attempts exclusive fullscreen.
  `das_bootleg` documents this as deliberate compositor/multi-monitor avoidance
  (`app/mod.rs:384-388`).
- Focus-loss handling that releases held keys exists in `das_bootleg` (`app/mod.rs:520-533`) and
  `asteroid_rodeo` (`lib.rs:1807`) and is absent in the other three, which therefore carry latent
  stuck-key bugs. `fw_rust` additionally works around one-shot inputs being lost on high-refresh
  monitors by only clearing input state when a simulation tick ran (`engine/mod.rs:365-372`),
  evidence for the runtime roadmap's open per-tick input staging item.

## Surface lifecycle is mishandled more often than handled

- `fw_rust` calls `get_current_texture().unwrap()` (`renderer.rs:1463`); surface loss panics.
- `portals_rust` reconfigures on `Lost` but lets `Outdated`/`Timeout` fall into a print-and-ignore
  arm (`main.rs:909-915`).
- `asteroid_rodeo` skips on `Timeout`/`Occluded` but has no explicit `Lost`/`Outdated` reconfigure
  path (`render_passes.rs:68-73`).
- `rust_voxel` and `das_bootleg` handle loss correctly, each hand-rolled; `rust_voxel`'s comment
  notes stale surfaces on fullscreen toggle and compositor resize (`plugin.rs:385-390`).
- Suspension/occlusion handling is absent in three of five.

Three of five games by an experienced author getting this wrong supports the claim that correct
surface lifecycle must be the API's natural path rather than application policy.

## Presentation pacing pain is universal

All five games fight frame pacing, and none can reach the control it needs through the
`wgpu`/`winit` seam:

- `rust_voxel` implements a phase-locked-loop frame pacer that reconstructs the vsync grid, with a
  comment stating that neither `winit` nor `wgpu` exposes true presentation timing
  (`main.rs:143-145`).
- `asteroid_rodeo` is limited to `AutoVsync` with no user-facing present-mode control
  (`context.rs:103`), and its MetalFX upscaling path requires a full `device.poll(Wait)` CPU stall
  mid-frame through wgpu's HAL interop (`render_passes.rs:1994`).
- `das_bootleg` hand-negotiates Mailbox, Immediate, then Fifo (`init.rs:91-99`) and ships
  instrumentation dedicated to chasing resume-frame hitches (`app/mod.rs:843-855`).
- `fw_rust` maintains roughly seven per-frame work budgets across its systems purely to avoid
  hitches, plus stutter items in its TODO ledger.
- `portals_rust` accepts `AutoVsync` and lives with the result.

This is the strongest cross-game signal in the survey, and it lands in the category the viability
docs call native value: presentation timing and pacing feedback are controls the portable
abstraction hides.

## Native GPU-feature reality

Four of five games request no native-only features and never hit the WebGPU feature model's
ceiling; their demands are reliability and pacing, not feature reach. The exceptions are precise:

- `asteroid_rodeo` drops to raw `objc2-metal` for MetalFX upscaling, the one production escape
  hatch in the portfolio, and pays the mid-frame stall above for it.
- `rust_voxel` is a genuinely GPU-driven renderer: compute frustum plus Hi-Z occlusion culling
  feeding `multi_draw_indexed_indirect` over a hand-built sub-allocator with bump allocation and
  free-list regions so chunk streaming avoids buffer churn (`gpu_driven.rs:151-272`). It also
  carries the two classic workarounds a native bindless/GPU-driven path should dissolve: a single
  texture atlas instead of texture arrays, and `base_vertex` baked into indices to dodge a
  wgpu-on-Metal limitation (`gpu_driven.rs:1022,1861`).

## Beyond graphics

- Audio is hand-rolled in every game: `kira` twice, and the same custom
  `cpal`+`hound`+`lewton`+`hrtf` stack duplicated across `asteroid_rodeo` and `rust_voxel`. The
  portfolio copy-maintains its own platform glue between projects.
- Text/UI is hand-rolled in four of five (`fontdue` three times, a forked `glyphon` patched for
  depth-ordered text in `fw_rust`).
- Gamepad support exists nowhere except through Steam Input in `asteroid_rodeo`.
- `asteroid_rodeo` establishes a constraint the backend contracts do not yet record: the Steam
  overlay injects into the native graphics API, so native Metal and Vulkan presentation must not
  break overlay injection, and this needs eventual physical validation before any serious-game
  support claim.

## Implications proposed for Mulciber (not yet decided)

1. Gate 4 candidate selection: native presentation pacing/timing (and an integrated upscaler path
   on Metal) has five-for-five consumer evidence, including one game that reimplements vsync
   reconstruction in software and one that stalls the CPU to reach MetalFX. The bindless/GPU-driven
   candidate retains exactly one real consumer, `rust_voxel`, which would serve as its reference
   workload; a voxel-style streaming slice could exercise both candidates in one comparison.
2. `mulciber-platform` roadmap additions with direct evidence: a pointer-capture API that owns the
   Locked/Confined policy (five reimplementations), and focus-loss input clearing as a default
   (already in `mulciber-runtime`; two games lack it and carry the bug).
3. `mulciber-runtime`: the high-refresh one-shot-input workaround in `fw_rust` and the
   render-rate-decoupled mouse look in `das_bootleg` (`app/mod.rs:720-730`) are concrete inputs to
   the open per-tick input staging evaluation.
4. Backend contracts: record Steam-overlay compatibility as a native presentation constraint and
   future validation item.
5. Scope honesty for the vision docs: this portfolio's recurring non-graphics cost is audio, text,
   and gamepad, which Mulciber deliberately does not own. The evidence does not demand owning them,
   but any future scope decision should weigh that they, not GPU features, dominate the repeated
   glue in real projects.

These implications are proposals; adopting any of them, including a Gate 4 candidate change, is a
separate recorded decision.
