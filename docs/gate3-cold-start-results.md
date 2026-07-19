# Gate 3 cold-start run: 2026-07-19 results

First execution of the [Gate 3 cold-start plan](gate3-cold-start-plan.md). Ten independent fresh
agents (model `claude-fable-5`, one per task per arm) ran concurrently on the Apple M2 / macOS
15.7.7 machine: the Mulciber arm in detached worktrees of committed revision `a61432c` with
repository-only materials, the control arm in pinned skeleton crates (`wgpu` 30.0.0, `winit`
0.30.13) with prior ecosystem familiarity allowed and local crate sources permitted. No web access
in either arm. Raw agent reports, submitted program sources, logs, and screenshots are preserved
under `validation-artifacts/gate3-2026-07-19/`.

The judge (the orchestrating session) independently rebuilt every submission, inspected the logs
and screenshots, and re-ran one submission per arm to completion (`mulciber-task2-demo --frames
240`; the control MSAA demo `--frames 180`). All ten rebuilds were clean and both spot-runs
self-exited correctly.

## Results

All ten attempts completed and passed judge verification. Times are agent-reported wall clock to
the first correct run (task 5: to the captured diagnostic); lines are the submitted program only.

| Task | Mulciber time | Control time | Mulciber lines | Control lines | Mulciber compile errors | Control compile errors |
| --- | --- | --- | --- | --- | --- | --- |
| 1 Clear | 4m15s* | ~3m | 79 | 182 | 0 | 6 |
| 2 Textured mesh + depth | ~4m | 7m21s | 221 | 488 | 0 | 8 |
| 3 Lifecycle | 4m44s | ~5m | 86 | 259 | 0 | 3 |
| 4 Optional 4x MSAA | 4m34s | 4m54s | 147 (+20 build.rs) | 369 | 0 | 7 |
| 5 Failure diagnosis | ~7m* | 5m48s | 120 + 175 (+20) | 207 + 224 | 0 | 2 |

*Task 1 Mulciber: an otherwise-correct run ~2.5 minutes in was killed by a concurrent agent
(see confounds), forcing a rerun. Task 5 Mulciber lost time to display contention (occluded
windows pause redraw delivery; the agent built a CGWindowList capture helper).

Every Mulciber-arm program compiled clean on the first attempt, under the workspace's pedantic
lint set: zero library-related compile errors across the whole arm. Every control-arm agent hit
wgpu 30 breaking changes relative to its wgpu 24-26-era prior knowledge (26 compile errors across
the arm, nine distinct API changes: `CurrentSurfaceTexture` acquisition enum, `Queue::present`,
`InstanceDescriptor` constructors, `multiview_mask`, `apply_limit_buckets`, `immediate_size`,
optional depth-stencil fields, optional bind-group-layout slices, `color_space`). All were
resolved within minutes from local crate sources, and the agents judged rustc's messages
self-identifying for most. This directly illustrates the gate's premise in both directions:
repository-only learning produced error-free first builds, while prior familiarity had decayed
against the pinned current release but recovered cheaply.

Task 5's diagnostics contrast: Mulciber's deliberately provoked mixed-session draw returned a
recoverable `Err` (`InvalidRequest`: "graphics handles belong to different sessions"); the program
continued, rendered in the new session, and shut down cleanly. The agent judged the message
sufficient to identify the contract and correction, with one gap: it does not name which handle is
foreign. It also found from source that a mixed-session render-targets handle reports
`StaleResource` while other handles report `InvalidRequest` for the same conceptual violation. The
control arm's invalid program (missing `RENDER_ATTACHMENT` usage) produced an excellent validation
message but delivered it as a panic that crossed winit's Objective-C boundary and aborted the
process (exit 134 plus the macOS crash-reporter dialog), with the panic location pointing into
wgpu's source rather than user code.

## Scoring against the pre-registered pass conditions

1. **Mental-model documentation suffices without backend internals**: met. Three of five agents
   read no backend code. Task 1 read backend source only to characterize where VSync throttling
   comes from (not needed to complete); task 4 read one backend file because no document states
   that the 4x-to-1x fallback is decided by a Metal `supportsTextureSampleCount:` query. No task
   required internals to make progress.
2. **Canonical examples complete, searchable, executable, current**: met, strongly. Every agent
   went README example table to a matching example, and every adaptation compiled clean on the
   first attempt.
3. **Diagnostics identify the violated contract and correction**: met on the available evidence,
   which is thin on the Mulciber side precisely because no agent triggered a compile error. The
   one provoked runtime diagnostic was judged sufficient minus the which-handle gap and the
   kind inconsistency above; both are recorded as improvements.
4. **No undocumented-convention guessing required**: met with reservations. Agents guessed, and
   correctly resolved from examples: how to add a new workspace package, that new programs reuse
   checked-in shader artifacts (no runtime WGSL path), that `RenderTargets` are rebuilt on
   surface-info change, that finite execution needs an application-owned frame counter, and (twice,
   at real time cost) that full occlusion suspends redraw delivery. The suspension policy is in
   fact documented in the platform contract, and task 3 found it there; tasks 4 and 5 did not,
   so this is a discoverability failure rather than a documentation absence.
5. **At least as reliable and efficient as the control arm**: reliability met (5/5 vs 5/5,
   judge-verified). The task-by-task time condition is **not met**: Mulciber was faster on tasks
   2, 3, and 4 but slower on tasks 1 and 5. Both slower tasks carry recorded interference
   confounds, and all differences are minutes on a single attempt, but the pre-registered
   condition is scored as written.

**Verdict: no Gate 3 pass is claimed.** Condition 5's time half failed as pre-registered, the run
is a single attempt per task, only the coding-model subject and the Metal/AppKit tier were
exercised, and the human cold-start arm remains open. What the run does establish: five-for-five
cold-start task completion from repository materials alone, first-compile success on every task,
and no reliability deficit against a familiarity-assisted control. Gate 3 remains open with
favorable first evidence.

## Recorded improvement actions

- Document how to add a new program: workspace membership, the example package skeleton, and the
  shader-artifact reuse convention (the artifact story was the most-cited cold-start hurdle; no
  document states that runtime WGSL is impossible and artifact reuse is intended).
- Surface the occlusion/minimize suspension policy where beginners look (README or the graphics
  contract), not only in the platform contract.
- Provide a minimal device-capability path or example: sample-count reporting currently requires
  the full textured-pipeline setup, and no document states the fallback's capability-query basis.
- Name the offending handle in mixed-session diagnostics, and unify the `InvalidRequest` versus
  `StaleResource` kind choice for mixed-session handles.
- Document the pacing model: that VSync throttling comes from display-synced drawable acquisition
  in the backend, currently discoverable only from backend source.

Addressed on 2026-07-19, same day: the new-program and shader-artifact conventions plus the
suspension policy now have a "Writing your own program" section in the README, and the graphics
contract states the pacing model. The minimal capability path/example and the mixed-session
diagnostic changes remain open because they change code, not documentation.

## Confounds and threats

- The ten agents shared one machine and display. Concurrent windows caused focus stealing,
  polluted screenshots, occlusion-induced suspensions (correct behavior, twice mistaken for a
  hang), and one agent killing another's in-flight run as a stray process. Task 1's Mulciber time
  and task 5's in both arms are inflated by this. Future runs should serialize agents or isolate
  displays.
- Single attempt per task; minutes-scale time differences carry no statistical weight.
- Task 5's Mulciber agent had read the conformance probe (which exercises the same violation)
  before judging the diagnostic, and disclosed this; its judgment is not fully cold.
- The control arm's knowledge-cutoff mismatch with wgpu 30 is the realistic condition the gate
  describes, but a control developer current with wgpu 30 would likely have been faster on every
  task; treat control times as familiarity-with-decay, not expert times.
- Both arms ran with pre-warmed dependency builds; compile-time cost differences (the Mulciber
  workspace cold-builds in ~11 s on this machine; the pinned control stack's shared target
  directory weighs 569 MB) are recorded here as context, not as part of the timing metric.
- Agent self-reports were audited by rebuild, log inspection, and two spot-runs, not by transcript
  replay.
