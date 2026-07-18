# Mulciber

`mulciber` is the research-stage graphics and presentation layer of the Mulciber native
game-development stack. The project is validating native Vulkan and Metal resource, rendering,
presentation, and lifecycle implementations before it extracts a stable public graphics API.

Version 0.3.0 grows the unstable extraction from surface/frame lifecycle into a narrow resource and
scene vocabulary: public `Device`, `Queue`, and `Surface` owners; owning mesh, texture, pipeline,
generation-bound render-target, and postprocess handles with explicit fallible destruction and
drop-queued reclamation; ordered multi-draw scene submission and native GPU instance batches; a
fixed two-pass postprocess operation; and a recovery-oriented error taxonomy that pairs contextual
messages with small failure kinds. It tracks the `mulciber-platform` 0.3 input-event contract. The
native Metal and Vulkan implementations consume the shared contract and have passed their platform
validation matrices after integration. The API remains research-stage and may change without
compatibility guarantees.

Development, design contracts, runnable examples, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
