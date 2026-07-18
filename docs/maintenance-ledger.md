# Maintenance ledger

Gate 6 asks whether the supported target set can be maintained to the promised quality bar by the
available team. That question cannot be answered retrospectively without data, so this ledger
records maintenance reality as it happens, starting 2026-07-18.

Record an entry whenever externally caused upkeep occurs: an OS, driver, compositor, SDK, toolchain,
or dependency change that breaks or degrades existing validated behavior, or a workaround added for
one. Do not record ordinary feature work, planned refactors, or bugs in unvalidated code; the ledger
measures the cost of keeping already-validated surface area true, not development effort.

Each entry records: date, trigger (what changed externally), affected surface (backend/platform),
effort (rough hours), resolution (fixed, worked around, coverage narrowed, pending), and whether any
validated claim had to be withdrawn or re-validated.

| Date | Trigger | Affected surface | Effort | Resolution | Claims affected |
| --- | --- | --- | --- | --- | --- |

No entries yet. The first OS point release, driver update, or compositor change that breaks a
validated behavior becomes the first data point rather than only a bad afternoon.
