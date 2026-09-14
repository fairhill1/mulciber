//! A pipeline's descriptor sets, allocated from as many pools as they turn out to need.
//!
//! Every pipeline caches one descriptor set per distinct sampled-identity tuple for as long as
//! the textures behind it live, so how many sets a pipeline needs is a property of the scene and
//! not of the pipeline: a world that keeps streaming in new textures keeps needing new sets. One
//! fixed pool is a ceiling that scene discovers by crashing, so the pool is grown instead: an
//! allocation that a full pool refuses is retried out of a fresh one, and a reset destroys the
//! lot together.
use super::{GraphicsError, Vec, check, ptr, vk};

/// The recipe one pool is created by, so growth and reset produce pools of the same shape.
pub(super) type CreatePool =
    fn(&super::super::Device) -> Result<vk::VkDescriptorPool, GraphicsError>;

pub(super) struct DescriptorPools {
    pools: Vec<vk::VkDescriptorPool>,
    create: CreatePool,
}

impl DescriptorPools {
    /// No pools yet; the first allocation creates one.
    pub(super) const fn new(create: CreatePool) -> Self {
        Self {
            pools: Vec::new(),
            create,
        }
    }

    /// Allocates one set of `set_layout`, opening another pool when the newest one is full.
    ///
    /// `VK_ERROR_FRAGMENTED_POOL` is treated the same way: sets are never freed individually,
    /// so a pool that reports fragmentation is one whose remaining descriptors cannot serve this
    /// layout, and a new pool is the only way to get the set.
    pub(super) fn allocate(
        &mut self,
        device: &super::super::Device,
        set_layout: vk::VkDescriptorSetLayout,
        operation: &str,
    ) -> Result<vk::VkDescriptorSet, GraphicsError> {
        if self.pools.is_empty() {
            self.pools.push((self.create)(device)?);
        }
        let mut set = ptr::null_mut();
        let result = Self::allocate_from(
            device,
            *self.pools.last().expect("a pool"),
            set_layout,
            &raw mut set,
        );
        if result == vk::VK_ERROR_OUT_OF_POOL_MEMORY || result == vk::VK_ERROR_FRAGMENTED_POOL {
            let pool = (self.create)(device)?;
            self.pools.push(pool);
            check(
                Self::allocate_from(device, pool, set_layout, &raw mut set),
                operation,
            )?;
        } else {
            check(result, operation)?;
        }
        Ok(set)
    }

    fn allocate_from(
        device: &super::super::Device,
        pool: vk::VkDescriptorPool,
        set_layout: vk::VkDescriptorSetLayout,
        set: *mut vk::VkDescriptorSet,
    ) -> vk::VkResult {
        let allocate = vk::VkDescriptorSetAllocateInfo {
            sType: vk::VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            descriptorPool: pool,
            descriptorSetCount: 1,
            pSetLayouts: &raw const set_layout,
            ..Default::default()
        };
        unsafe {
            device
                .functions
                .allocate_descriptor_sets
                .expect("loaded function")(device.handle, &raw const allocate, set)
        }
    }

    /// Destroys every pool, and with them every set handed out; the caller drops its cache of
    /// those sets. The next allocation opens a fresh pool.
    ///
    /// The caller has already waited for every frame that could still be reading the sets.
    pub(super) fn reset(&mut self, device: &super::super::Device) {
        for pool in self.pools.drain(..) {
            unsafe {
                device
                    .functions
                    .destroy_descriptor_pool
                    .expect("loaded function")(device.handle, pool, ptr::null());
            }
        }
    }
}
