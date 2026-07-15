# Canonical references

References are pinned for reproducibility while implementation work uses the newest published
documentation to identify later capabilities.

## Vulkan

- API baseline: Vulkan 1.4.
- Reviewed specification: Vulkan 1.4.356, generated 2026-07-03.
- Specification source commit: `73836865422f9e28e17069a96cceef6d0ece1ff8`.
- Canonical specification: <https://registry.khronos.org/vulkan/specs/latest/html/vkspec.html>
- Source repository: <https://github.com/KhronosGroup/Vulkan-Docs>
- Conformance products: <https://www.khronos.org/conformance/adopters/conformant-products>

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
