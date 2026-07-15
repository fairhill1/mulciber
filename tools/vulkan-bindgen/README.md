# Vulkan binding generation

`zinc-vulkan-bindgen` generates checked-in, dependency-free Rust ABI bindings from the exact
Khronos headers recorded in `../../vulkan-toolchain.lock.toml`. It is an isolated workspace so
libclang and its transitive dependencies never enter Zinc's runtime dependency graph.

Place `vulkan_core.h`, `vulkan_win32.h`, and `vk_platform.h` from the pinned Vulkan-Headers commit
in one directory, then run:

```sh
cargo run --manifest-path tools/vulkan-bindgen/Cargo.toml -- \
  /path/to/headers probes/vulkan-win32-triangle/src/vk.rs
```

Generation targets the Windows x86-64 C ABI even when the tool runs on another host.
