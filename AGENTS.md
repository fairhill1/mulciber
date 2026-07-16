# Mulciber agent guide

## Read first

Before architectural or backend work, read:

- `README.md`
- `docs/roadmap.md`
- `docs/backend-contracts.md`
- `docs/viability-gates.md`
- The relevant platform validation or design document

## Project direction

- Build native Metal and Vulkan evidence before extracting shared APIs.
- Do not introduce `wgpu`, `winit`, Direct3D, or speculative abstraction layers.
- Preserve backend-specific capabilities, ownership, synchronization, and lifecycle differences.
- Prefer a runnable vertical slice with validation over broad scaffolding.
- Keep advanced capabilities independent instead of collapsing them into a linear hardware tier.

## Required checks

Before committing Rust changes, run:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git diff --check
```

Run platform-specific validation in proportion to the change. On Windows/Vulkan, use:

```powershell
.\scripts\validate-windows.ps1 -SkipInteractive
```

`-SkipInteractive` is an automated preflight. It does not replace required physical lifecycle,
multi-display, hardware-tier, or visual evidence.

## Vulkan rules

- Vulkan bindings are generated. Update `tools/vulkan-bindgen/symbols.txt`, then regenerate
  `probes/vulkan-win32-triangle/src/vk.rs`; do not hand-edit generated declarations.
- Shader source and checked-in SPIR-V must change together.
- Compile shaders for the pinned target environment and update every affected hash in
  `vulkan-toolchain.lock.toml`.
- Keep validation enabled and treat every warning or error as a failure.
- Resource destruction must account for GPU execution and presentation-engine ownership.
- New fallback paths must be observable and physically exercised when the current machine permits.
- Pipeline cache work must follow `docs/vulkan-pipeline-cache.md`.

## Evidence and documentation

When capability evidence changes, update the applicable files:

- `README.md`
- `docs/roadmap.md`
- `docs/backend-contracts.md`
- The relevant platform validation runbook

Record exactly what was tested. Do not claim:

- multi-display coverage from a single-display test;
- physical lifecycle coverage from an automated run;
- unsupported hardware or driver coverage;
- visual correctness from validation-layer success alone.

Validation archives belong under `validation-artifacts/` and are not source files.

## Git and parallel work

- Preserve unrelated user changes and avoid unrelated cleanup.
- Use fresh worktrees for parallel tasks and assign non-overlapping files or separable commits.
- Base new worktrees on current `main` and keep commits focused.
- Use `--ff-only` only when ancestry permits it. If `main` advanced, rebase or cherry-pick instead of
  forcing history.
- Commit or push only when the task authorizes it.
- After pushing, verify that the working tree is clean and `HEAD` equals `origin/main`.

## Scope discipline

- Do not turn probes into production abstractions prematurely.
- Do not mark roadmap or viability work complete without checked-in code, reproducible validation,
  and the required written evidence.
- Clearly report evidence that remains unavailable on the current machine.
- Add narrower nested `AGENTS.md` files only when a subtree develops materially different rules.
