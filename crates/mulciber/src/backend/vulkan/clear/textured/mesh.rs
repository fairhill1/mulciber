//! Immutable mesh placement and frame-owned transfer storage.
//!
//! Device-local meshes retain no CPU mirror after submission. Each frame slot
//! owns its staging buffer until its fence completes; only small staging buffers
//! are cached. Mesh allocations are returned through the existing all-frame
//! retirement boundary, which also makes reuse of an uploaded range safe.

use super::{
    Buffer, ClearSurface, GraphicsError, MESH_ALLOCATION_ALIGNMENT, MESH_BUFFER_BLOCK_SIZE,
    MeshAllocation, MeshIndexData, MeshPartResource, MeshResource, bytes_of_slice, create_buffer,
    create_buffer_with_memory, destroy_buffer, destroy_buffer_device, error, insert_free_range,
    map_buffer, plan_mesh_storage, take_free_range, vk,
};
use crate::GraphicsErrorKind;
use core::ptr;
use std::ops::Range;
use std::{eprintln, vec::Vec};

const HOST_MEMORY: u32 = (vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
    | vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
    .cast_unsigned();
const LOCAL_MEMORY: u32 = vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT.cast_unsigned();
const MESH_USAGE: u32 = (vk::VK_BUFFER_USAGE_VERTEX_BUFFER_BIT
    | vk::VK_BUFFER_USAGE_INDEX_BUFFER_BIT
    | vk::VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT)
    .cast_unsigned();
/// At most 4 MiB per frame slot remains cached after upload completion.
const STAGING_RETAIN_LIMIT: u64 = 4 * 1024 * 1024;

struct MeshBufferBlock {
    buffer: Buffer,
    /// Non-null only for directly CPU-visible placement (UMA/full BAR or fallback).
    mapped: *mut u8,
    free: Vec<Range<u64>>,
}

struct PendingUpload {
    allocation: MeshAllocation,
    bytes: Vec<u8>,
}

struct StagingBuffer {
    buffer: Buffer,
    mapped: *mut u8,
}

pub(super) struct MeshBufferArena {
    blocks: Vec<MeshBufferBlock>,
    pending: Vec<PendingUpload>,
    staging: [Option<StagingBuffer>; ClearSurface::frames_in_flight()],
    force_host: bool,
}

impl MeshBufferArena {
    pub(super) fn new() -> Self {
        Self {
            blocks: Vec::new(),
            pending: Vec::new(),
            staging: core::array::from_fn(|_| None),
            force_host: std::env::var("MULCIBER_VULKAN_MESH_MEMORY")
                .is_ok_and(|value| value == "host"),
        }
    }

    pub(super) fn create_mesh(
        &mut self,
        surface: &ClearSurface<'_>,
        vertex_bytes: &[u8],
        index_parts: &[MeshIndexData<'_>],
    ) -> Result<MeshResource, GraphicsError> {
        let vertex_size = u64::try_from(vertex_bytes.len())
            .map_err(|_| error("mesh vertex bytes exceed Vulkan address space"))?;
        let (mut parts, size) = plan_mesh_storage(vertex_size, index_parts)?;
        let allocation = self.allocate(surface, size)?;
        if let Err(failure) = self.write_mesh(allocation, vertex_bytes, index_parts, &parts) {
            self.free(allocation);
            return Err(failure);
        }
        for part in &mut parts {
            // The packing plan is within `size`, and allocation is within its block.
            part.index_offset += allocation.offset;
            part.indirect_offset += allocation.offset;
        }
        Ok(MeshResource {
            buffer: self.blocks[allocation.block].buffer.handle,
            allocation,
            vertex_offset: allocation.offset,
            parts,
        })
    }

    fn write_mesh(
        &mut self,
        allocation: MeshAllocation,
        vertices: &[u8],
        indices: &[MeshIndexData<'_>],
        parts: &[MeshPartResource],
    ) -> Result<(), GraphicsError> {
        let mapped = self.blocks[allocation.block].mapped;
        let mut bytes = Vec::new();
        let destination = if mapped.is_null() {
            self.pending
                .try_reserve(1)
                .map_err(|_| host_allocation_error())?;
            let size = usize::try_from(allocation.size)
                .map_err(|_| error("mesh upload size exceeds address space"))?;
            bytes
                .try_reserve_exact(size)
                .map_err(|_| host_allocation_error())?;
            bytes.resize(size, 0);
            bytes.as_mut_ptr()
        } else {
            // SAFETY: The allocation is within this mapped block and is unused by
            // every in-flight frame until creation returns and the mesh is submitted.
            unsafe {
                mapped.add(usize::try_from(allocation.offset).expect("allocated offset fits usize"))
            }
        };
        // SAFETY: The validated packing plan fits the allocation or temporary vector.
        // Sources are borrowed immutable input and do not overlap backend storage.
        unsafe {
            ptr::copy_nonoverlapping(vertices.as_ptr(), destination, vertices.len());
            for (index, part) in indices.iter().zip(parts) {
                ptr::copy_nonoverlapping(
                    index.bytes.as_ptr(),
                    destination
                        .add(usize::try_from(part.index_offset).expect("validated mesh offset")),
                    index.bytes.len(),
                );
                let draw = vk::VkDrawIndexedIndirectCommand {
                    indexCount: part.index_count,
                    instanceCount: 1,
                    firstIndex: 0,
                    vertexOffset: 0,
                    firstInstance: 0,
                };
                let draw_bytes = bytes_of_slice(core::slice::from_ref(&draw));
                ptr::copy_nonoverlapping(
                    draw_bytes.as_ptr(),
                    destination.add(
                        usize::try_from(part.indirect_offset).expect("validated indirect offset"),
                    ),
                    draw_bytes.len(),
                );
            }
        }
        if mapped.is_null() {
            self.pending.push(PendingUpload { allocation, bytes });
        }
        Ok(())
    }

    fn allocate(
        &mut self,
        surface: &ClearSurface<'_>,
        size: u64,
    ) -> Result<MeshAllocation, GraphicsError> {
        for (block, storage) in self.blocks.iter_mut().enumerate() {
            if let Some(offset) =
                take_free_range(&mut storage.free, size, MESH_ALLOCATION_ALIGNMENT)
            {
                return Ok(MeshAllocation {
                    block,
                    offset,
                    size,
                });
            }
        }
        let block_size = MESH_BUFFER_BLOCK_SIZE.max(
            size.checked_next_power_of_two()
                .ok_or_else(|| error("mesh buffer block size overflow"))?,
        );
        self.blocks
            .try_reserve(1)
            .map_err(|_| host_allocation_error())?;
        let mut free = Vec::new();
        free.try_reserve(1).map_err(|_| host_allocation_error())?;
        free.push(0..block_size);
        let size_usize =
            usize::try_from(block_size).map_err(|_| error("mesh block exceeds address space"))?;
        let (buffer, mapped) = self.create_block(surface, size_usize)?;
        let offset = take_free_range(&mut free, size, MESH_ALLOCATION_ALIGNMENT)
            .expect("new mesh block fits its allocation");
        let block = self.blocks.len();
        self.blocks.push(MeshBufferBlock {
            buffer,
            mapped,
            free,
        });
        Ok(MeshAllocation {
            block,
            offset,
            size,
        })
    }

    fn create_block(
        &self,
        surface: &ClearSurface<'_>,
        size: usize,
    ) -> Result<(Buffer, *mut u8), GraphicsError> {
        if self.force_host {
            eprintln!("Vulkan mesh storage: forced host-visible fallback");
        } else {
            // A small discrete-GPU BAR heap is not a replacement for the VRAM heap.
            // UMA or a fully CPU-visible VRAM heap can be written directly instead.
            let direct = full_local_heap_is_host_visible(surface);
            let required = if direct {
                LOCAL_MEMORY | HOST_MEMORY
            } else {
                LOCAL_MEMORY
            };
            let result = create_buffer_with_memory(
                surface,
                size,
                MESH_USAGE | vk::VK_BUFFER_USAGE_TRANSFER_DST_BIT.cast_unsigned(),
                &[],
                required,
            )
            .and_then(|buffer| {
                if direct {
                    mapped_buffer(surface, buffer)
                } else {
                    Ok((buffer, ptr::null_mut()))
                }
            });
            match result {
                Ok(storage) => {
                    eprintln!(
                        "Vulkan mesh storage: device-local, {} ({} MiB block)",
                        if direct {
                            "direct mapped"
                        } else {
                            "staged uploads"
                        },
                        size / (1024 * 1024)
                    );
                    return Ok(storage);
                }
                Err(failure) if fallback_allowed(failure.kind()) => {
                    eprintln!("Vulkan mesh storage: host-visible fallback after {failure}");
                }
                Err(failure) => return Err(failure),
            }
        }
        let buffer = create_buffer(surface, size, MESH_USAGE, &[])?;
        mapped_buffer(surface, buffer)
    }

    /// Called only after acquisition has completed this frame slot's fence.
    /// Pending bytes survive abandonment and failed recording/submission.
    pub(super) fn prepare_uploads(
        &mut self,
        surface: &ClearSurface<'_>,
    ) -> Result<(), GraphicsError> {
        let required = self.pending.iter().try_fold(0_usize, |sum, upload| {
            sum.checked_add(upload.bytes.len())
                .ok_or_else(|| error("mesh upload size overflow"))
        })?;
        let slot = &mut self.staging[surface.frame_slot_index()];
        if slot
            .as_ref()
            .is_some_and(|staging| discard_staging(staging.buffer.size, required))
        {
            destroy_staging(surface.device(), slot.take().expect("checked staging"));
        }
        if required == 0 {
            return Ok(());
        }
        if slot.is_none() {
            let size = required
                .checked_next_power_of_two()
                .ok_or_else(|| error("mesh staging size overflow"))?;
            let buffer = create_buffer(
                surface,
                size,
                vk::VK_BUFFER_USAGE_TRANSFER_SRC_BIT.cast_unsigned(),
                &[],
            )?;
            let (buffer, mapped) = mapped_buffer(surface, buffer)?;
            *slot = Some(StagingBuffer { buffer, mapped });
        }
        let staging = slot.as_ref().expect("staging allocated");
        let mut offset = 0;
        for upload in &self.pending {
            // SAFETY: This completed slot's coherent mapping fits the checked sum.
            unsafe {
                ptr::copy_nonoverlapping(
                    upload.bytes.as_ptr(),
                    staging.mapped.add(offset),
                    upload.bytes.len(),
                );
            }
            offset += upload.bytes.len();
        }
        Ok(())
    }

    pub(super) fn record_uploads(&self, surface: &ClearSurface<'_>) {
        if self.pending.is_empty() {
            return;
        }
        let staging = self.staging[surface.frame_slot_index()]
            .as_ref()
            .expect("uploads prepared");
        let functions = &surface.device().functions;
        let mut offset = 0;
        for upload in &self.pending {
            let destination = self.blocks[upload.allocation.block].buffer.handle;
            let region = vk::VkBufferCopy2 {
                sType: vk::VK_STRUCTURE_TYPE_BUFFER_COPY_2,
                srcOffset: offset,
                dstOffset: upload.allocation.offset,
                size: upload.allocation.size,
                ..Default::default()
            };
            let copy = vk::VkCopyBufferInfo2 {
                sType: vk::VK_STRUCTURE_TYPE_COPY_BUFFER_INFO_2,
                srcBuffer: staging.buffer.handle,
                dstBuffer: destination,
                regionCount: 1,
                pRegions: &raw const region,
                ..Default::default()
            };
            let barrier = vk::VkBufferMemoryBarrier2 {
                sType: vk::VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER_2,
                srcStageMask: vk::VK_PIPELINE_STAGE_2_COPY_BIT,
                srcAccessMask: vk::VK_ACCESS_2_TRANSFER_WRITE_BIT,
                dstStageMask: vk::VK_PIPELINE_STAGE_2_VERTEX_INPUT_BIT
                    | vk::VK_PIPELINE_STAGE_2_DRAW_INDIRECT_BIT,
                dstAccessMask: vk::VK_ACCESS_2_VERTEX_ATTRIBUTE_READ_BIT
                    | vk::VK_ACCESS_2_INDEX_READ_BIT
                    | vk::VK_ACCESS_2_INDIRECT_COMMAND_READ_BIT,
                srcQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
                dstQueueFamilyIndex: vk::VK_QUEUE_FAMILY_IGNORED.cast_unsigned(),
                buffer: destination,
                offset: upload.allocation.offset,
                size: upload.allocation.size,
                ..Default::default()
            };
            let dependency = vk::VkDependencyInfo {
                sType: vk::VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
                bufferMemoryBarrierCount: 1,
                pBufferMemoryBarriers: &raw const barrier,
                ..Default::default()
            };
            // SAFETY: Recording precedes all mesh reads, ranges are aligned and
            // disjoint, and both buffers remain owned until frame completion.
            unsafe {
                functions.cmd_copy_buffer2.expect("loaded function")(
                    surface.frame_command_buffer(),
                    &raw const copy,
                );
                functions.cmd_pipeline_barrier2.expect("loaded function")(
                    surface.frame_command_buffer(),
                    &raw const dependency,
                );
            }
            offset += upload.allocation.size;
        }
    }

    pub(super) fn uploads_submitted(&mut self) {
        self.pending.clear();
    }

    pub(super) fn free(&mut self, allocation: MeshAllocation) {
        self.pending.retain(|upload| {
            upload.allocation.block != allocation.block
                || upload.allocation.offset != allocation.offset
        });
        insert_free_range(
            &mut self.blocks[allocation.block].free,
            allocation.offset..allocation.offset + allocation.size,
        );
    }

    pub(super) fn destroy(&mut self, device: &super::super::Device) {
        self.pending.clear();
        for slot in &mut self.staging {
            if let Some(staging) = slot.take() {
                destroy_staging(device, staging);
            }
        }
        for block in self.blocks.drain(..) {
            // SAFETY: The session drained GPU work before arena destruction.
            unsafe {
                if !block.mapped.is_null() {
                    device.functions.unmap_memory.expect("loaded function")(
                        device.handle,
                        block.buffer.memory,
                    );
                }
                destroy_buffer_device(device, block.buffer);
            }
        }
    }
}

fn mapped_buffer(
    surface: &ClearSurface<'_>,
    buffer: Buffer,
) -> Result<(Buffer, *mut u8), GraphicsError> {
    match map_buffer(surface, &buffer) {
        Ok(mapped) => Ok((buffer, mapped)),
        Err(failure) => {
            destroy_buffer(surface, buffer);
            Err(failure)
        }
    }
}

#[allow(clippy::needless_pass_by_value)] // Consumes the sole staging owner.
fn destroy_staging(device: &super::super::Device, staging: StagingBuffer) {
    // SAFETY: Called only for a completed frame slot or after session drain.
    unsafe {
        device.functions.unmap_memory.expect("loaded function")(
            device.handle,
            staging.buffer.memory,
        );
        destroy_buffer_device(device, staging.buffer);
    }
}

fn host_allocation_error() -> GraphicsError {
    GraphicsError::with_kind(
        GraphicsErrorKind::OutOfMemory,
        "cannot reserve mesh upload bookkeeping",
    )
}

fn fallback_allowed(kind: GraphicsErrorKind) -> bool {
    matches!(
        kind,
        GraphicsErrorKind::OutOfMemory | GraphicsErrorKind::Unsupported
    )
}

fn discard_staging(capacity: u64, required: usize) -> bool {
    capacity < required as u64 || capacity > STAGING_RETAIN_LIMIT
}

fn full_local_heap_is_host_visible(surface: &ClearSurface<'_>) -> bool {
    let device = surface.device();
    let mut properties = vk::VkPhysicalDeviceMemoryProperties::default();
    // SAFETY: The physical device and output structure are live.
    unsafe {
        device
            .instance
            .functions
            .get_physical_device_memory_properties
            .expect("loaded function")(device.adapter.handle, &raw mut properties);
    }
    has_full_mapped_heap(&properties)
}

fn has_full_mapped_heap(properties: &vk::VkPhysicalDeviceMemoryProperties) -> bool {
    let heaps = &properties.memoryHeaps[..properties.memoryHeapCount as usize];
    let largest_local = heaps
        .iter()
        .filter(|heap| heap.flags & vk::VK_MEMORY_HEAP_DEVICE_LOCAL_BIT.cast_unsigned() != 0)
        .map(|heap| heap.size)
        .max()
        .unwrap_or(0);
    largest_local != 0
        && properties.memoryTypes[..properties.memoryTypeCount as usize]
            .iter()
            .any(|memory| {
                memory.propertyFlags & (LOCAL_MEMORY | HOST_MEMORY) == LOCAL_MEMORY | HOST_MEMORY
                    && heaps[memory.heapIndex as usize].size == largest_local
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_bar_heap_does_not_select_direct_mapping() {
        let mut properties = vk::VkPhysicalDeviceMemoryProperties {
            memoryHeapCount: 2,
            memoryTypeCount: 1,
            ..Default::default()
        };
        properties.memoryHeaps[0].size = 8 << 30;
        properties.memoryHeaps[0].flags = vk::VK_MEMORY_HEAP_DEVICE_LOCAL_BIT.cast_unsigned();
        properties.memoryHeaps[1].size = 256 << 20;
        properties.memoryHeaps[1].flags = vk::VK_MEMORY_HEAP_DEVICE_LOCAL_BIT.cast_unsigned();
        properties.memoryTypes[0].heapIndex = 1;
        properties.memoryTypes[0].propertyFlags = HOST_MEMORY | LOCAL_MEMORY;
        assert!(!has_full_mapped_heap(&properties));
        properties.memoryTypes[0].heapIndex = 0;
        assert!(has_full_mapped_heap(&properties));
    }

    #[test]
    fn streaming_spike_does_not_pin_large_staging_buffers() {
        assert!(discard_staging(128 << 20, 0));
        assert!(discard_staging(128 << 20, 4096));
        assert!(!discard_staging(STAGING_RETAIN_LIMIT, 0));
        assert!(discard_staging(4096, 8192));
    }

    #[test]
    fn fallback_never_hides_device_loss_or_validation_failure() {
        assert!(fallback_allowed(GraphicsErrorKind::OutOfMemory));
        assert!(fallback_allowed(GraphicsErrorKind::Unsupported));
        assert!(!fallback_allowed(GraphicsErrorKind::DeviceFailure));
        assert!(!fallback_allowed(GraphicsErrorKind::Validation));
        assert!(!fallback_allowed(GraphicsErrorKind::NativeFailure));
    }
}
