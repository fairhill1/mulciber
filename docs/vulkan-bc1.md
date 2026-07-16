# Vulkan BC1 compressed-texture evidence plan

This document defines the evidence required before Mulciber marks Vulkan compressed-texture support
complete. It is an implementation plan for the native probe, not the eventual public texture-format
API. The pipeline-cache slice should land first so this work can verify that changing the sampled
texture path does not silently weaken the cache evidence.

## Goal

The probe must demonstrate that it can:

1. distinguish core BC compression support from support for the exact image roles the workload uses;
2. upload a deterministic BC1 block payload into an optimal-tiled image;
3. prove the uploaded block bytes survive a GPU round trip;
4. sample that compressed image directly in the existing scene pass;
5. select the existing RGBA8 path when BC1 is unavailable or deliberately bypassed;
6. fail with an actionable capability reason when BC1 is explicitly required; and
7. preserve the native 4x, forced 1x, resize, synchronization, and pipeline-cache evidence.

BC1 must remain optional for adapter selection. It is representative format evidence, not a new
baseline requirement. Normal rendering correctness must not depend on compression being available.

## Reuse the cross-backend fixture

Port the exact deterministic fixture from `probes/metal-triangle/src/main.rs` instead of inventing a
second compressed asset:

- dimensions: 8x8 texels;
- format: `VK_FORMAT_BC1_RGBA_UNORM_BLOCK`;
- block extent: 4x4 texels;
- block size: 8 bytes;
- layout: four blocks in a 2x2 checkerboard, 32 bytes total; and
- RGBA fallback: the existing 8x8 `CHECKER_PIXELS` expansion of those blocks.

The two solid RGB565 endpoints already used by Metal make each block deterministic: selector zero is
used for every texel, so each 4x4 block resolves to one known opaque color. Reusing the encoded bytes
and expanded fallback bytes gives the two backends a shared test asset without implying that Vulkan
must follow Metal's compute-decompression path.

Vulkan should sample BC1 directly. Metal's compute decompression exists to exercise a Metal workload
capability; it is not a portable implementation prescription.

## Capability and mode selection

Adapter inspection should retain both of these facts:

1. `VkPhysicalDeviceFeatures::textureCompressionBC` is true; and
2. `vkGetPhysicalDeviceFormatProperties` reports all optimal-tiling features required for
   `VK_FORMAT_BC1_RGBA_UNORM_BLOCK`:
   - `VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT`;
   - `VK_FORMAT_FEATURE_TRANSFER_DST_BIT`; and
   - `VK_FORMAT_FEATURE_TRANSFER_SRC_BIT`.

The sampler is nearest-filtered today, so linear-filter support is not part of this slice's actual
requirement. Do not demand an unused feature. If the sampler changes to linear filtering later, add
`VK_FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR_BIT` to the negotiated role set at that point.

Carry the selected texture path on `Adapter` so logical-device creation, resource creation, upload,
diagnostics, and validation all consume one decision. Enable `textureCompressionBC` on the logical
device only for the BC1 path. The RGBA8 fallback must neither enable nor use it.

Use one mutually exclusive diagnostic control:

- `MULCIBER_VULKAN_TEXTURE_MODE=auto` or unset: prefer BC1, otherwise use RGBA8 and print every
  missing requirement;
- `MULCIBER_VULKAN_TEXTURE_MODE=bc1`: require BC1 and reject startup with the missing core-feature or
  exact format-feature names; and
- `MULCIBER_VULKAN_TEXTURE_MODE=rgba8`: bypass BC1 and force the fallback even on capable hardware.

Reject any other value before device creation. A forced RGBA8 path is needed to physically validate
fallback behavior on the current BC-capable Windows machine. Required mode makes the failure
contract observable instead of leaving it as an untested internal branch.

The fixed fixture uses an ordinary 8x8, single-mip, single-layer, single-sampled 2D image. The exact
format-role query covers the compressed operations added by this slice without expanding the
generated ABI. If the implementation generalizes dimensions, flags, array layers, mip counts, or
usage combinations, add `vkGetPhysicalDeviceImageFormatProperties` rather than silently extending
this fixed probe assumption.

## Resource shape and descriptor stability

Replace the current hard-coded 4x4 sampled texture constants with a small selected-path description
containing:

- diagnostic name;
- Vulkan format;
- width and height;
- upload payload;
- image usage;
- expected readback bytes; and
- whether `textureCompressionBC` must be enabled.

The BC1 path uses `VK_FORMAT_BC1_RGBA_UNORM_BLOCK`; the fallback uses
`VK_FORMAT_R8G8B8A8_UNORM`, matching the linear RGBA expansion already verified by Metal rather than
the Vulkan probe's current sRGB fixture. Both paths create one optimal-tiled, device-local,
single-mip 2D image with
`TRANSFER_DST | TRANSFER_SRC | SAMPLED` usage. Keep the existing combined image-sampler descriptor,
descriptor binding, image view, sampler, fragment shader, scene pipeline, and lifetime ownership.
Only the selected image format and payload differ.

This deliberately introduces no shader, descriptor-layout, or pipeline variant. Therefore BC1 does
not add a pipeline-cache entry. The cache validation should still be rerun after the change; a strict
hit is evidence that the cache inventory remains complete, not an assumption based on source review.

## Upload and exact round-trip proof

The BC1 staging buffer contains exactly 32 bytes. For the full 8x8 copy, use one
`VkBufferImageCopy2` with:

- `bufferOffset = 0`;
- `bufferRowLength = 0` and `bufferImageHeight = 0` for tightly packed data;
- color aspect, mip zero, layer zero, one layer; and
- `imageExtent = { 8, 8, 1 }` expressed in texels, not `{ 2, 2, 1 }` blocks.

The full extent is block-aligned and offset zero is aligned to the eight-byte BC1 block size. Keep
the existing synchronization2 upload barrier, then add startup verification in the same one-time
command buffer:

1. transition `UNDEFINED -> TRANSFER_DST_OPTIMAL` with transfer-write access;
2. copy the staging buffer into the image;
3. transition `TRANSFER_DST_OPTIMAL -> TRANSFER_SRC_OPTIMAL`, making transfer writes available to
   transfer reads;
4. copy the full image into a temporary host-visible, host-coherent readback buffer with the same
   tightly packed region;
5. transition `TRANSFER_SRC_OPTIMAL -> SHADER_READ_ONLY_OPTIMAL`, making transfer access complete
   before fragment sampled reads;
6. submit and wait using the existing startup upload fence path; and
7. compare every returned byte with the selected upload payload before destroying staging and
   readback buffers.

Run the same exact round trip for the RGBA8 fallback. That keeps the evidence symmetric: BC1 compares
32 encoded bytes and RGBA8 compares 256 expanded bytes. A mismatch should report the selected path,
first differing offset, expected byte, and actual byte.

The byte comparison proves the payload and compressed buffer-image addressing. It does not prove
that the scene sampled the image. Direct fragment consumption is established separately by the
unchanged combined-image-sampler binding, validation-clean draw, and the visual checkerboard
inspection in the validation matrix. Do not describe either half alone as end-to-end proof.

No validation compute pipeline should be added for this slice. It would expand the pipeline-cache
inventory merely to test a fixed startup asset, while the exact image-to-buffer round trip plus the
existing rendered consumer supplies the required evidence with less machinery.

## Diagnostics and failure behavior

Print one stable selection line before resource creation:

- `Texture path: BC1_RGBA_UNORM direct sampling`; or
- `Texture path: RGBA8 fallback (<reason>)`.

The reason should distinguish a forced fallback from missing `textureCompressionBC`, sampled-image,
transfer-destination, or transfer-source support. Required BC1 mode should return the same facts in
its startup error. Do not collapse them into `BC1 unsupported`.

After verification, print the payload size and exact result, for example:

- `Texture upload: 32 BC1 bytes round-tripped exactly`; or
- `Texture upload: 256 RGBA8 bytes round-tripped exactly`.

Failures after queue submission must preserve the current ownership rule: establish device or fence
completion before destroying any staging or readback allocation referenced by the command buffer.
The sampled texture itself remains device-owned until orderly shutdown.

## Validation matrix

The BC1 slice is complete only after recording all of the following with validation and loader
warnings treated as failures:

1. Run `MULCIBER_VULKAN_TEXTURE_MODE=bc1` on the native MSAA path for 600 frames. Record the BC feature
   and format-feature decision, exact 32-byte round trip, and a visually correct repeated 8x8
   checkerboard in the scene.
2. Repeat required BC1 with `MULCIBER_VULKAN_FORCE_MSAA_1X=1`; require the same upload proof and visual
   output on the 1x scene pipeline.
3. Run required BC1 without a frame limit through continuous resize, minimize/restore, and close.
   The persistent sampled texture must remain valid while resize-dependent resources turn over.
4. Run `MULCIBER_VULKAN_TEXTURE_MODE=rgba8` for 600 frames and verify the explicit forced-fallback
   reason, exact 256-byte round trip, and equivalent checkerboard consumption.
5. Exercise fallback with forced 1x and an interactive resize smoke so compression selection cannot
   conceal a sample-count or lifecycle coupling.
6. Run default `auto` mode and confirm it reports the path selected from actual capability facts.
7. Exercise the pure selection helper with missing core BC support and each missing exact format bit;
   verify `auto` chooses RGBA8 while required `bc1` returns the named actionable error.
8. Relearn the current pipeline-cache artifact if necessary, then run strict cache validation on BC1
   native 4x and forced 1x. Every pre-existing pipeline must still report an application-cache hit;
   no texture-dependent pipeline entry should appear.

For the physical runs, preserve the selected adapter, texture mode, core feature state, exact format
feature mask, payload/readback result, MSAA path, resize result where applicable, pipeline-cache
feedback, and validation/loader output. Passing Rust tests or validation layers without checking the
rendered pattern is not visual evidence.

## Documentation updates after evidence lands

Only after the matrix is physically recorded:

- add the Vulkan compressed-texture item to `docs/roadmap.md` with the machine and paths exercised;
- update the resource row and remaining-evidence list in `docs/backend-contracts.md`;
- add commands, selection diagnostics, exact readback output, and physical results to
  `docs/windows-validation.md`; and
- keep `probes/vulkan-info` reporting raw `textureCompressionBC` independently from the triangle
  probe's exact BC1 role decision.

The capability ledger should say that Vulkan has direct BC1 sampling evidence. It should not imply
that all BC formats, dimensions, mip layouts, copy shapes, or filtering modes have been exercised.

## Deliberate non-goals

- This slice does not define Mulciber's public compressed-format taxonomy.
- It does not require BC compression for the Vulkan baseline or adapter eligibility.
- It does not transcode arbitrary assets, parse DDS/KTX containers, or choose among BC families.
- It does not generate compressed mip levels or claim filtered BC1 mip evidence; the representative
  workload already exercises mip generation on an uncompressed storage image.
- It does not reproduce Metal's compute decompression on Vulkan.
- It does not add a texture-dependent graphics or compute pipeline variant.
- It does not infer broad format support from `textureCompressionBC` without checking the exact
  optimal-tiling roles used by the image.

## Canonical Vulkan references

- BC1 format definitions and decoding:
  <https://docs.vulkan.org/spec/latest/appendices/compressedtex.html#appendix-compressedtex-bc>
- Format block extents and byte sizes:
  <https://docs.vulkan.org/spec/latest/chapters/formats.html#formats-definition>
- Core `textureCompressionBC` feature:
  <https://docs.vulkan.org/spec/latest/chapters/features.html#features-features-textureCompressionBC>
- Exact format capabilities:
  <https://docs.vulkan.org/refpages/latest/refpages/source/vkGetPhysicalDeviceFormatProperties.html>
- Format feature meanings:
  <https://docs.vulkan.org/refpages/latest/refpages/source/VkFormatFeatureFlagBits.html>
- Buffer-image copy region addressing:
  <https://docs.vulkan.org/refpages/latest/refpages/source/VkBufferImageCopy2.html>
- Compressed buffer-image copy constraints:
  <https://docs.vulkan.org/spec/latest/chapters/copies.html#copies-buffers-images-addressing>

The implementation should remain smaller than this plan: one selected texture-path record, one core
feature enable, one exact format-role query, a 32-byte fixture, a temporary readback buffer, and the
existing sampled-texture machinery. The detail here exists to prevent a capability bit or a
validation-clean draw from being mistaken for complete compressed-texture evidence.
