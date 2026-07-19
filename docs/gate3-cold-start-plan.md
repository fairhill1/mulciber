# Gate 3 cold-start learnability plan

Gate 3 requires that Mulciber be easier to learn from the checked-out repository than established
alternatives are to recall from prior knowledge, because Mulciber has no tutorial corpus and no
training-data presence. This document pre-registers the first cold-start run's subjects, tasks,
protocol, measurements, and pass conditions before any run executes, following the same discipline
as the [Gate 4 pacing plan](gate4-pacing-plan.md).

## Scope of this run

This run measures **current coding models only**. The gate also names an unfamiliar human Rust
developer; that arm remains open and is not claimed by this run. Additional limits, stated up front:

- One machine and tier: Apple M2, macOS 15.7.7, Metal/AppKit. No Vulkan-side cold start is claimed.
- Lifecycle actions are agent-driven synthetic input (AppleScript/`screencapture`), not physical
  operator evidence, per the existing evidence rules.
- One attempt per task per arm, so this is an existence-and-friction record, not a reliability
  distribution. Repetition across models and machines belongs to later runs.

## Subjects and arms

- **Mulciber arm**: five independent fresh agents (model recorded in the results), one per task.
  Each receives a clean git worktree of the pinned revision and may use only repository contents:
  README, `docs/`, examples, probes, and crate source. Reading backend internals is permitted but
  recorded, because the gate scores needing them as a failure signal. No web access of any kind. No
  Mulciber knowledge exists in training data; general Rust and graphics knowledge is expected and
  allowed.
- **wgpu/winit control arm**: five independent fresh agents, one per task, with prior ecosystem
  familiarity explicitly allowed per the gate. Each receives an empty pinned binary crate
  (`wgpu = "=30.0.0"`, `winit = "=0.30.13"`, matching the repository's comparison pins) and may add
  ordinary utility crates from the registry, read locally downloaded crate sources, and run
  `cargo doc`. No web browsing. The Mulciber repository is not visible to this arm.

Infrastructure that is provided rather than measured: each arm shares one cargo target directory
with dependency builds pre-warmed before the runs, so time-to-first-correct-run measures reading,
authoring, and debugging rather than cold dependency compilation on the 8 GB machine. Registry
access for `cargo add`/`cargo build` is permitted in both arms and is not web browsing.

## Tasks

The five Gate 3 tasks, restated with completion criteria. Tasks are scored separately and each is
attempted from scratch by a fresh agent; no agent sees another agent's work or report.

1. **Clear**: open a native window, clear it to a single chosen solid color, remain VSync-paced,
   and close cleanly. Complete when the program compiles clean, a capture shows the color, pacing
   is plausibly VSync-bound (not a busy spin), and exit is clean.
2. **Textured mesh with depth**: upload a mesh and a texture and render them depth-tested with
   visible animation. Complete when a capture shows the textured geometry with correct occlusion
   and the animation demonstrably advances per frame.
3. **Lifecycle**: respond correctly to continuous resize, minimize and restore, and titlebar close.
   Complete when synthetic resize/minimize/restore/close runs produce no crash, no native
   diagnostic, and rendering resumes after restore.
4. **Optional capability with fallback**: request 4x multisampling as optional, visibly report the
   selected sample count, and demonstrate the 1x path when forced or unavailable. Complete when
   both the selection report and a forced-fallback run are shown.
5. **Failure diagnosis**: construct one intentionally invalid resource or synchronization request
   and judge whether the compile-time or runtime diagnostic identifies the violated contract and a
   likely correction. Complete when the agent produces the invalid program, the diagnostic, and an
   accurate explanation derived from that diagnostic.

## Protocol

Each agent receives the same prompt template, parameterized only by arm and task: the task text
above, the working directory, the materials rule for its arm, and the reporting requirements. The
prompt names no Mulciber crate, type, document, or example, and gives no API guidance for either
arm. Agents self-verify (captures, logs, synthetic input) and must preserve their program source
and evidence in the working directory.

Each agent reports: status (complete, partial, failed); wall-clock start and end from `date`; every
file consulted, classified as README/docs, example or probe source, public crate source, backend
internals, or (control arm) crate docs/source; every compiler or runtime error encountered and
whether the message identified the violated contract and correction; undocumented conventions it
had to guess; and its verification evidence.

The judge (the orchestrating session) then independently rebuilds each submitted program, spot-runs
it, and reviews the code against the completion criteria before scoring. Agent self-reports are
treated as claims until this check passes.

## Measurements

Per task and arm, following the ergonomics rules in the
[measurement protocol](api-extraction-plan.md#measurement-protocol):

- completion status against the criteria above, with the judge's verification result;
- wall-clock time to first correct run;
- files consulted, by class, and specifically whether backend internals were required;
- application lines of the submitted program (evidence, not a score);
- diagnostic quality for every error hit, not only task 5; and
- friction notes: guesses, dead ends, missing documentation.

## Pass conditions

Restating the gate operationally for this run:

- The mental-model documentation suffices: Mulciber-arm agents complete tasks from README, docs,
  and examples without needing backend internals to make progress.
- Canonical examples are complete, searchable, executable, and current: agents locate and
  successfully adapt them.
- Diagnostics identify the violated contract and a likely correction, judged on every error
  encountered, with task 5 as the focused case.
- No task requires guessing undocumented conventions.
- Mulciber-arm completion is at least as reliable, and time-to-first-correct-run at least as good,
  as the control arm task-by-task, with the single-attempt limitation reported alongside any claim.

A failed condition is a stop-or-redesign signal for the documentation and API surface, recorded as
such; it is not softened by partial credit. Results, raw agent reports, and the judge's
verification live in the dated results record once the run completes; this plan is not edited
after results exist except by a dated addendum.

## Threats to validity

Recorded before the run:

- The pinned `wgpu` 30.0.0 / `winit` 0.30.13 releases postdate the subject models' knowledge
  cutoff; control-arm familiarity may be stale relative to the pinned API. This mirrors the real
  situation the gate describes but must be reported with the results.
- Agents may violate the no-web rule; compliance is instructed and self-reported, not enforced.
- General graphics knowledge (Metal, Vulkan, GPU concepts) benefits both arms and cannot be
  controlled for.
- Single attempt per task: one lucky or unlucky path can dominate a task's result.
- Self-reported metrics (time, files read) are audited only by spot checks against the judge's
  rebuild.
- Synthetic lifecycle input cannot exercise physical display changes or prove felt pacing.
