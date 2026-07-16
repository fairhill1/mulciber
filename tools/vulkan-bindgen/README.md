# Vulkan binding generation

`mulciber-vulkan-bindgen` generates checked-in, dependency-free Rust ABI bindings from the exact
Khronos headers recorded in `../../vulkan-toolchain.lock.toml`. It is an isolated workspace so
libclang and its transitive dependencies never enter Mulciber's runtime dependency graph.

Place the pinned Vulkan-Headers `include/vulkan` directory and its sibling `include/vk_video`
directory together under one include root, then run:

```sh
cargo run --manifest-path tools/vulkan-bindgen/Cargo.toml -- \
  /path/to/headers probes/vulkan-triangle/src/vk.rs
```

Generation targets the Windows x86-64 C ABI even when the tool runs on another host.
