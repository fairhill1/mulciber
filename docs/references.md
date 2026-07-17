# Canonical references

References are pinned for reproducibility while implementation work uses the newest published
documentation to identify later capabilities.

## Vulkan

- API baseline: Vulkan 1.3 with `dynamicRendering` and `synchronization2`; request 1.4 when exposed.
- Reviewed specification and binding source: Vulkan 1.4.356, generated 2026-07-03.
- Specification source commit: `73836865422f9e28e17069a96cceef6d0ece1ff8`.
- Canonical specification: <https://registry.khronos.org/vulkan/specs/latest/html/vkspec.html>
- Source repository: <https://github.com/KhronosGroup/Vulkan-Docs>
- Conformance products: <https://www.khronos.org/conformance/adopters/conformant-products>
- Presentation-semaphore reuse guidance:
  <https://docs.vulkan.org/guide/latest/swapchain_semaphore_reuse.html>
- Swapchain-maintenance presentation fences:
  <https://docs.vulkan.org/refpages/latest/refpages/source/VkSwapchainPresentFenceInfoKHR.html>
- Swapchain recreation sample and deferred-retirement fallback:
  <https://docs.vulkan.org/samples/latest/samples/api/swapchain_recreation/README.html>

Compatible revisions of Vulkan-Headers, Vulkan-Loader, Vulkan-ValidationLayers, Vulkan-Profiles,
SPIRV-Headers, SPIRV-Tools, and glslang are recorded in the machine-readable
[`vulkan-toolchain.lock.toml`](../vulkan-toolchain.lock.toml). Header inputs are SHA-256 locked.

## Metal

- Compatibility baseline: Metal 3 on Apple silicon.
- Advanced path: Metal 4 when the build SDK and runtime support it.
- Framework documentation: <https://developer.apple.com/documentation/metal>
- Feature tables: <https://developer.apple.com/metal/capabilities/>
- Metal 4 overview:
  <https://developer.apple.com/documentation/metal/understanding-the-metal-4-core-api>

Apple's installed SDK headers are the ABI authority. Each build records the Xcode build number and
macOS SDK version. The initial development machine has Xcode 16.4 with macOS SDK 15.5, which exposes
Metal 3 but not the Metal 4 API family.

## API comparison baselines

The [API extraction and comparison plan](api-extraction-plan.md) assigns each baseline a specific
role. These links identify the upstream projects; exact releases, source revisions, features, Rust
bindings, shader tools, and integration choices are pinned in each benchmark record before results
are collected.

- `ash`: <https://github.com/ash-rs/ash>
- `wgpu`: <https://github.com/gfx-rs/wgpu>
- `winit`: <https://github.com/rust-windowing/winit>
- SDL3 GPU: <https://wiki.libsdl.org/SDL3/CategoryGPU>
- SDL3 source: <https://github.com/libsdl-org/SDL>
- Vulkano: <https://github.com/vulkano-rs/vulkano>
- raylib: <https://github.com/raysan5/raylib>

The practical Metal/AppKit Rust stack and the maintained Rust bindings used for SDL3 or raylib are
selected and pinned at benchmark time. Binding maturity is recorded separately so it does not become
an unexplained score for or against the underlying API.
