# Pre-runtime game dogfood slice

`mulciber-game-slice` is the first playable application built from the extracted platform and
graphics checkpoints. It is intentionally application-owned rather than a premature
`mulciber-runtime` implementation.

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

## Runtime pressure observed

The application currently owns policy that the planned `mulciber-runtime` may absorb:

- a four-direction held-key snapshot assembled from ordered press/release transitions;
- focus-loss invalidation of held input;
- elapsed-time sampling and a 50 ms variable-update clamp;
- game update before transform generation and frame submission;
- simulation state, collision, win/reset transitions, and diagnostic output; and
- the platform pump, nonfatal frame acquisition, generation-bound target rebuild, and shutdown loop.

This does not yet justify extracting the runtime crate. Gate 5 also requires fixed and variable
updates, frame-pacing policy, suspension, fullscreen/display changes, device recovery, and supported
platform coverage. The next runtime experiment should move only the generic timing/input/coordination
policy above—not collision, camera, game state, or unrelated engine architecture—then compare the
result with the same application composed from `winit` and the existing wgpu scene plumbing.

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
