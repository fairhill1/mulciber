# Mulciber

`mulciber` is the research-stage graphics and presentation layer of the Mulciber native
game-development stack. The project is validating native Vulkan and Metal resource, rendering,
presentation, and lifecycle implementations before it extracts a stable public graphics API.

Version 0.1.0 intentionally contains only the documented library shell. The development tree now
contains the first unstable surface-generation and frame-lifecycle vocabulary extracted from the
native probes; it is not ready for application use or a new release until both backends consume that
contract.

Development and runnable probes live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
