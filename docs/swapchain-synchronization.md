# Acquired images and reused attachments

## Faults reproduced on Windows, 2026-09-22

Isle of Ran built with Mulciber 0.13.24 and `vulkan-validation`, with the
Khronos layer's `validate_sync` and submit-time validation enabled, reported
two hazards on the initial menu on an RTX 3060 Ti. The driver was
32.0.16.1692, the SDK 1.4.350.0, the surface fullscreen 2560x1440 at two samples,
and presentation Adaptive / FIFO relaxed. No device loss was required to
reproduce the validation errors.

- `WRITE_AFTER_READ`: the first swapchain-image transition had source stage
  `NONE`, so it was not chained to the acquisition semaphore wait at
  `COLOR_ATTACHMENT_OUTPUT`. Even with `oldLayout = UNDEFINED`, a layout
  transition must wait for acquisition. Discarding contents does not release
  presentation ownership.
- `WRITE_AFTER_WRITE`: the direct-render path reused one depth image across
  overlapping frames, transitioning from `UNDEFINED` with no source dependency
  on the preceding frame's late-fragment-test attachment store. The analogous
  multisample color image also had no dependency on earlier attachment writes.

The clear-only, direct-material and postprocessed paths now share the acquired
image barrier, with `COLOR_ATTACHMENT_OUTPUT` on both sides. Direct depth reuse
orders early/late depth operations, and multisample color reuse orders color
attachment writes. No device-idle wait or reduction in frames in flight was added.

The acquisition regression test checks both first-use and presented layouts.
Native before/after game logs are kept in the consuming repository under
`target/gpu-validation/20260922-101237-335/validation.log` (baseline) and
`target/gpu-validation/20260922-101840-015/validation.log` (patched).
The patched initial menu reports neither synchronization hazard. Both runs
also report the independent OBS hook warning about API 1.3 versus application
API 1.4; do not count that warning as a synchronization regression or claim a
completely warning-free validation run.

These are demonstrated synchronization fixes, not proof of the cause of the
RTX 4070 playtest crashes during dialogue and map use. Extended gameplay and
the affected GPU/driver remain separate validation evidence.

The subsequent game run at
`target/gpu-validation/20260922-102546-494/validation.log` loaded the world and
completed the requested map/dialogue test without synchronization errors or a
crash. The developer's GPU did not reproduce the original device loss before
the patch either. Poor FPS reported during this test is not a release benchmark:
it used the development profile with synchronization validation, and test
compilation overlapped gameplay. A release executable with validation disabled
was packaged and passed the consuming game's texture, packaging, extracted
asset-loading and baked-world checks.

## Release validation (0.13.25)

Workspace formatting, all-target compilation, workspace Clippy with warnings
denied, workspace tests/doctests, and package verification with
`cargo publish -p mulciber --dry-run --allow-dirty --config net.offline=false`
passed on Windows. The package verification used the published
mulciber-platform 0.5.5. The platform's macOS-only display-timing helper no
longer compiles into unrelated Windows test builds, resolving the previously
documented workspace dead-code lint without a runtime change.

`scripts/validate-windows.ps1 -SkipInteractive -InstanceOnly` passed six native
instance/device/timestamp checks; logs are in
`validation-artifacts/windows-vulkan-20260922-103748/`. The full probe-window
matrix was not rerun. Surface synchronization evidence is the consuming game's
before/after validation and map/dialogue test described above; no new Metal or
RTX 4070 physical coverage is claimed.
