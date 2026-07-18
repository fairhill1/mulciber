# Game and runtime dogfood slice

`mulciber-game-slice` is the first playable application built from the extracted platform and
graphics checkpoints. Its original application-owned timing/input implementation provided the
pressure evidence for the first narrow `mulciber-runtime` extraction; it is now that crate's first
consumer.

## Playable loop

The top-down Forge Run scene uses W/A/S/D or arrow keys to move a player through a bounded arena.
Eight animated crystals disappear when collected, four moving sentries reset the player on contact,
static obstacles resolve movement per axis, the camera follows the player, and R resets the run.
Console diagnostics report collection, hits, completion, and reset.

Rendering uses the existing instanced textured pipeline, two meshes, several textures, depth,
capability-selected 4x/1x MSAA, and postprocessing. The scene dynamically omits the pickup batch
after the eighth crystal is collected. No graphics or platform API was added for this checkpoint;
that is evidence that the current extracted slice can express this small workload, not a claim that
its fixed vertex/material vocabulary is sufficient for general games.

Run it with:

```sh
cargo run -p mulciber-game-slice
```

Input is currently implemented by `mulciber-platform` on AppKit and Win32. The example can render on
the existing Linux Vulkan path, but it is not a playable Linux claim until Wayland and X11 input
peers exist.

## Runtime extraction

The initial application locally owned:

- a four-direction held-key snapshot assembled from ordered press/release transitions;
- focus-loss invalidation of held input;
- elapsed-time sampling and a 50 ms variable-update clamp;
- game update before transform generation and frame submission;
- simulation state, collision, win/reset transitions, and diagnostic output; and
- the platform pump, nonfatal frame acquisition, generation-bound target rebuild, and shutdown loop.

The first extraction moves only the generic pieces into `mulciber-runtime`. Ordered
`mulciber-platform` transitions accumulate into frame-scoped held/pressed/released snapshots, and
focus loss releases every held key or pointer button. A 60 Hz accumulator produces zero or more
fixed gameplay updates per displayed frame, caps hitch recovery at eight steps, reports discarded
time, and supplies an interpolation fraction. Forge Run retains previous/current simulation states
and interpolates player motion, facing, camera, and sentries for the renderer. Cosmetic pickup
animation consumes the clamped variable frame delta. A scoped runtime frame clears transient input
automatically on normal completion or early return.

Simulation advances before graphics acquisition, so a temporarily unavailable surface does not
make game time conditional on acquiring a drawable. Collision, camera, game state, reset/win policy,
the native event pump, and rendering remain application-owned. See the
[experimental runtime contract](runtime-contract.md).

This is not Gate 5 completion. Native frame-pacing policy, suspension, fullscreen/display changes,
device recovery, supported Linux input, process/OS suspension, and the full lifecycle comparison
remain pending. Rendering suspension from zero-sized, minimized, or occluded windows is now
coordinated, with focused physical evidence on macOS only.

The focused timing/input/rendering comparison is now implemented as
`comparisons/wgpu-game-slice`. It locally composes equivalent held/pressed input and fixed-step
accumulator policy around `winit` and uses `wgpu` for the same five instance batches, depth, MSAA,
and postprocess result. See the [game-slice comparison](game-slice-comparison.md). The broader Gate 5
comparison remains open because neither side of this checkpoint exercises the missing suspension,
display-transition, pacing-diagnostic, or recovery work.

## macOS checkpoint

On 2026-07-18, an uncommitted tree based on `4c43bde` ran `mulciber-game-slice` on the Apple M2 /
macOS 15.7.7 machine with `MTL_DEBUG_LAYER=1`. It selected Metal and four samples. A visually
inspected screenshot showed the arena, player, obstacles, crystals, moving sentries, depth, and the
expected final grade/vignette. Metal emitted no diagnostic beyond the validation-enabled banner.

The operator physically played the slice through collection and sentry collision. The first pass
exposed a game-math defect: positive-Y quaternion yaw mirrored the player's diagonal facing because
the model's visual forward axis is local negative Z. Negating the yaw corrected W+D and W+A facing,
and the operator confirmed the final angles. All eight crystals were collected, proving collection,
dynamic removal of the final pickup batch, and the win transition. The validation-driven process was
deliberately interrupted rather than closed through a lifecycle pass; no resize, minimize/restore,
display change, or deterministic rendering readback is claimed here.

After migration to `mulciber-runtime`, the operator replayed Forge Run on the same Apple M2 / macOS
15.7.7 machine and reported that the game and interpolated movement felt correct. This is a visual
and interaction confirmation of the fixed-step consumer path, not measured cadence evidence or a
repeat of the broader lifecycle matrix.

The follow-up rendering-suspension slice then ran under Metal API Validation on the same machine.
The operator held movement, minimized the window, released the key while minimized, waited, and
restored it. They confirmed no catch-up jump and no stuck movement. The run also exercised
collection, sentry collision, and normal close without Metal diagnostics beyond the enabled banner.
This does not establish process/OS sleep behavior or Windows/Linux runtime-backed suspension.
