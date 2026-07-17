# Mulciber

`mulciber` is the research-stage graphics and presentation layer of the Mulciber native
game-development stack. The project is validating native Vulkan and Metal resource, rendering,
presentation, and lifecycle implementations before it extracts a stable public graphics API.

Version 0.2.0 contains the first unstable surface-generation and frame-lifecycle vocabulary extracted
from the native Metal and Vulkan probes, tracking the reshaped `mulciber-platform` 0.2 event-pump
contract and adding a const `ClearColor` constructor for literals. Both probes consume the shared
contract and have passed their platform validation matrices after integration. The API remains
research-stage and may change without compatibility guarantees.

The current repository also contains an unreleased two-pass experiment with generation-bound
offscreen scene color, a fullscreen postprocess pipeline, and one narrow postprocessed draw/present
operation. It is pressure evidence for a future command vocabulary, not a stable render-pass or
frame-graph API.

Development and runnable probes live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
