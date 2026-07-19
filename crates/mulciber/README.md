# Mulciber

`mulciber` is the research-stage graphics and presentation layer of the Mulciber native
game-development stack. The project is validating native Vulkan and Metal resource, rendering,
presentation, and lifecycle implementations before it extracts a stable public graphics API.

Version 0.4.0 adds drained native presentation feedback: `Surface::take_present_feedback` returns
identified presented frames carrying the display time when the native system reports one, keeps
undrained samples in a bounded queue, and answers `Unsupported` on backends without native
feedback, so estimation fallbacks are an observable application decision. Metal implements
feedback through drawable presented handlers; the Vulkan implementation is deliberately absent
until its extension-availability survey runs on physical tiers. Validation diagnostics now name
the offending handle in mixed-session rejections and report every mixed-session handle as an
invalid request, reserving stale-resource errors for surface-information mismatches whose
correction is a rebuild. It tracks the `mulciber-platform` 0.4 pointer-capture contract. The API
remains research-stage and may change without compatibility guarantees.

Development, design contracts, runnable examples, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
