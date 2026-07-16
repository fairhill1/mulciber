# Vulkan pipeline-cache evidence plan

This document defines the evidence required before Mulciber marks Vulkan pipeline caching complete.
It is an implementation plan for the native probe, not the eventual shipping-cache API. The slice
should begin only after the shadow pipeline stabilizes the probe's initial pipeline set.

## Goal

The probe must demonstrate that it can:

1. create every compute and graphics pipeline against one application pipeline cache;
2. serialize the opaque cache without interpreting or rewriting its payload;
3. load it in a later process only when its Vulkan header matches the selected device;
4. distinguish an application-cache hit from a driver-internal cache or an ordinary fast compile;
5. expand the cache deliberately when a new pipeline variant is introduced;
6. recover safely from missing, truncated, incompatible, or ineffective data; and
7. keep caching optional for correctness.

Pipeline creation duration alone is not proof of cache use. Drivers may maintain internal caches,
creation time is noisy, and a pipeline can be cheap to compile. Mulciber should use Vulkan's cache
feedback and compile-control facilities to make the validation claim falsifiable.

## Vulkan facts that constrain the design

- `vkGetPipelineCacheData` returns a driver-owned opaque blob. A null data pointer queries its
  maximum size; a later retrieval may return `VK_INCOMPLETE` if the provided buffer is too small.
- Version-one cache data begins with a tightly packed 32-byte little-endian header containing
  `headerSize`, `headerVersion`, `vendorID`, `deviceID`, and `pipelineCacheUUID`.
- Incompatible or invalid initial data is ignored by the implementation. Passing it to
  `vkCreatePipelineCache` is therefore not, by itself, evidence that the payload was accepted.
- Vulkan 1.3 promotes pipeline creation feedback. When valid, the
  `VK_PIPELINE_CREATION_FEEDBACK_APPLICATION_PIPELINE_CACHE_HIT_BIT` specifically reports that the
  application-provided cache avoided most pipeline creation work.
- Vulkan 1.3 also promotes pipeline creation cache control. When the optional
  `pipelineCreationCacheControl` feature is enabled,
  `VK_PIPELINE_CREATE_FAIL_ON_PIPELINE_COMPILE_REQUIRED_BIT` makes creation return
  `VK_PIPELINE_COMPILE_REQUIRED` instead of compiling.
- Strict creation success proves that no compilation was required, but it does not alone prove that
  the serialized application cache was the source; an implementation-internal cache may also avoid
  compilation. The application-cache-hit feedback bit is the evidence for that stronger claim.
- Pipeline cache use is internally synchronized by default. The probe is single-threaded and should
  not opt into externally synchronized cache access.

## Probe pipeline set

The first cache artifact should cover every pipeline created by the representative workload:

- startup compute;
- depth-only shadow;
- main scene at native 4x MSAA when supported;
- main scene at forced 1x;
- fullscreen post-processing; and
- any format-dependent variants selected by the current surface and depth-format negotiation.

The 4x and 1x scene pipelines are distinct cache entries. A cache trained only on one path must not
be described as complete for the other. Resize normally reuses the pipeline when formats and sample
count remain stable; a format change may introduce another legitimate variant.

## Capability and device creation

Adapter selection should query
`VkPhysicalDevicePipelineCreationCacheControlFeatures::pipelineCreationCacheControl` and record the
result. Enable the feature in the logical-device `pNext` chain only when supported.

Pipeline creation feedback is core in the Vulkan 1.4 baseline and does not require a separate feature
bit. Every graphics and compute create info should chain a `VkPipelineCreationFeedbackCreateInfo`
with whole-pipeline feedback storage. Per-stage feedback is useful diagnostics but is not necessary
to prove the artifact slice.

The selected physical-device properties already expose the four version-one header facts Mulciber
needs: `vendorID`, `deviceID`, and the 16-byte `pipelineCacheUUID`, plus the fixed version and header
size. Driver version should be recorded in logs and evidence, but compatibility must follow the
Vulkan header rather than an invented driver-version rule.

## Artifact policy

The probe artifact is a raw `vkGetPipelineCacheData` blob. It must not be prefixed with a Mulciber
header because `VkPipelineCacheCreateInfo::pInitialData` must point to data previously returned by
Vulkan with the original size.

Recommended probe controls:

- `--pipeline-cache PATH` selects the artifact;
- `--rebuild-pipeline-cache` ignores existing data and starts empty;
- `--require-pipeline-cache-hits` forbids compilation and requires valid application-cache-hit
  feedback for every requested pipeline; and
- the default path lives under `target`, is device-specific, and is not a source-controlled asset.

Normal mode is a learning mode: it loads compatible data when present, permits missing entries to
compile, reports feedback for every pipeline, and serializes the expanded cache at orderly shutdown.
Strict mode is validation: it must not silently learn or replace missing entries.

The default filename may include a lowercase hexadecimal `pipelineCacheUUID` for operator clarity,
but the file contents remain authoritative and must still be checked. Shader or pipeline changes do
not make an older Vulkan cache unsafe; they may simply create misses. The validation workflow uses
explicit rebuild and strict modes to establish that the current complete pipeline set is present.

## Load and preflight sequence

1. Resolve the requested path before device creation diagnostics are printed.
2. If the file is absent, create an empty cache and report a cold start.
3. Reject unreasonable filesystem objects such as directories and report ordinary read failures
   with the selected path.
4. Require at least 32 bytes before parsing a version-one header.
5. Decode header fields explicitly as little-endian bytes; do not cast the file buffer to a Rust or
   bindgen structure whose packing is not guaranteed.
6. Require `headerSize == 32` and `headerVersion == VK_PIPELINE_CACHE_HEADER_VERSION_ONE`.
7. Compare `vendorID`, `deviceID`, and all 16 `pipelineCacheUUID` bytes with the selected adapter.
8. On a truncated, unknown-version, or incompatible header, report the exact reason and continue
   with an empty cache in learning mode. Strict mode fails before pipeline creation.
9. Pass a header-compatible blob to `vkCreatePipelineCache`. Vulkan remains responsible for opaque
   payload validation.

Preflight avoids presenting obviously wrong data to the driver and makes incompatibility actionable.
It cannot prove that the opaque payload is intact or useful; pipeline feedback supplies that evidence.

## Pipeline creation behavior

All compute and graphics pipeline helpers should accept the owned cache handle and a stable diagnostic
name such as `compute`, `shadow`, `scene-4x`, `scene-1x`, or `post`.

For each create call:

1. initialize whole-pipeline feedback storage;
2. chain it to the pipeline create info;
3. pass the application pipeline cache instead of `VK_NULL_HANDLE`;
4. in strict mode, set `VK_PIPELINE_CREATE_FAIL_ON_PIPELINE_COMPILE_REQUIRED_BIT` when the feature is
   supported;
5. handle `VK_PIPELINE_COMPILE_REQUIRED` as a named cache miss, not as a generic Vulkan failure; and
6. report feedback validity, application-cache hit, and creation duration.

Strict validation passes only when every requested pipeline has valid feedback with the
application-cache-hit bit set. If cache control is available, strict mode additionally forbids
compilation. If cache control is unavailable, feedback can still demonstrate an application-cache
hit, but the probe must report that compile prohibition was unavailable. If feedback is invalid or
the hit bit is absent, the run cannot claim that serialized entries were used.

An unsuccessful strict run must leave the existing artifact unchanged so the failure remains
diagnosable. Learning mode may rebuild or extend it.

## Serialization and replacement

After all pipeline creation is complete—or at orderly shutdown after no thread can modify the
cache—serialize with a retry loop:

1. call `vkGetPipelineCacheData` with a null data pointer to obtain the maximum size;
2. allocate exactly that many bytes;
3. retrieve the data;
4. on `VK_INCOMPLETE`, resize from a new size query and retry rather than persisting a prefix;
5. verify the returned blob's version-one header against the selected adapter;
6. write a sibling temporary file and flush its contents;
7. replace the destination through the platform's atomic replacement primitive; and
8. retain the previous artifact if retrieval, validation, write, flush, or replacement fails.

The Windows probe should use a Win32 replacement operation with replace-existing semantics rather
than deleting the destination before renaming. A crash may leave an orphan temporary file, but it
must leave either the previous complete artifact or the new complete artifact at the selected path.

Strict mode is read-only with respect to the artifact. This prevents a validation run from repairing
the evidence it was meant to verify.

## Corruption and incompatibility behavior

The following cases are distinct and should remain visible in diagnostics:

| Condition | Learning mode | Strict mode |
| --- | --- | --- |
| Missing file | Start empty, learn, serialize | Fail: no artifact to verify |
| Fewer than 32 bytes | Ignore with `truncated header`, learn, atomically replace | Fail before Vulkan creation |
| Unknown header version or size | Ignore with decoded values, learn, atomically replace | Fail before Vulkan creation |
| Vendor, device, or UUID mismatch | Ignore with expected/actual identity, learn a separate compatible artifact | Fail before Vulkan creation |
| Compatible header, ineffective opaque payload | Detect misses through feedback and compile control, learn, replace | Fail with the names of missing pipelines |
| Filesystem read/write failure | Return a contextual probe error | Return a contextual probe error without changing the artifact |

Mulciber should not promise that arbitrary payload corruption will make `vkCreatePipelineCache` fail;
the specification allows invalid data to be ignored. The observable contract is safe fallback plus
feedback-based detection, not a particular driver error code.

## Validation matrix

The pipeline-cache slice is complete only after recording all of the following with validation and
loader warnings treated as failures:

1. Delete or rebuild the artifact, run native 4x in learning mode, and record cold pipeline feedback.
2. Run forced 1x in learning mode against the same artifact so it acquires the second scene variant.
3. Start a new process in strict mode on native 4x and require application-cache hits for compute,
   shadow, scene-4x, and post.
4. Start another strict process on forced 1x and require hits for compute, shadow, scene-1x, and post.
5. Complete the resize smoke in strict mode and verify that stable-format resize introduces no
   compile-required result.
6. Copy the artifact, truncate it below 32 bytes, and verify the documented cold fallback without
   validation messages.
7. Copy the artifact, alter a compatibility field, and verify that preflight rejects it before
   passing data to Vulkan.
8. Copy the artifact, alter bytes beyond the header, and verify either continued valid hits or a
   detected miss followed by safe learning-mode replacement; never require a driver error.
9. Run with a fresh path after all cache code is disabled or unavailable and confirm rendering
   correctness is unchanged.

Record creation-feedback flags and durations for each pipeline, the cache-control feature state, the
artifact path and byte size, parsed device identity, whether the run learned or required hits, and
the final replacement result.

## Deliberate non-goals

- This slice does not define Mulciber's public cache API or a universal cross-device artifact.
- It does not introduce pipeline binaries, graphics-pipeline libraries, background compilation, or
  multiple worker caches.
- It does not merge caches across processes or claim that one file is portable across vendors,
  devices, or incompatible pipeline-cache UUIDs.
- It does not treat lower creation duration as sufficient cache evidence.
- It does not require pipeline creation cache control for rendering correctness or for Mulciber's
  baseline device support.

## Canonical Vulkan references

- Pipeline cache creation and compatibility:
  <https://docs.vulkan.org/refpages/latest/refpages/source/VkPipelineCacheCreateInfo.html>
- Pipeline cache serialization:
  <https://docs.vulkan.org/refpages/latest/refpages/source/vkGetPipelineCacheData.html>
- Version-one header and cache semantics:
  <https://docs.vulkan.org/spec/latest/chapters/pipelines.html#pipelines-cache-header>
- Pipeline creation feedback:
  <https://docs.vulkan.org/refpages/latest/refpages/source/VkPipelineCreationFeedbackCreateInfo.html>
- Application pipeline-cache hit semantics:
  <https://docs.vulkan.org/refpages/latest/refpages/source/VkPipelineCreationFeedbackFlagBits.html>
- Pipeline creation cache-control feature:
  <https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDevicePipelineCreationCacheControlFeatures.html>
- Compile-required behavior:
  <https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_pipeline_creation_cache_control.html>

The implementation should remain smaller than this plan: one owned cache handle, header preflight,
feedback-aware pipeline creation, retrying serialization, atomic replacement, and an explicit
learning-versus-strict policy. The detail here exists to keep the evidence claim precise.
