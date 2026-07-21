use core::fmt;
use core::ops::{Index, IndexMut};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::format;
use std::rc::Rc;
use std::vec::Vec;

use crate::GraphicsError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResourceId {
    slot: u32,
    generation: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceKind {
    Mesh,
    Texture,
    TexturedPipeline,
    InstancedTexturedPipeline,
    PostprocessPipeline,
    MaterialPipeline,
    ShadowMap,
    ShadowMapArray,
    ShadowPipeline,
    RenderTargets,
    PostprocessTargets,
}

impl ResourceKind {
    /// Diagnostic name used in error messages so a violation identifies the offending handle.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Mesh => "mesh",
            Self::Texture => "texture",
            Self::TexturedPipeline => "textured pipeline",
            Self::InstancedTexturedPipeline => "instanced textured pipeline",
            Self::PostprocessPipeline => "postprocess pipeline",
            Self::MaterialPipeline => "material pipeline",
            Self::ShadowMap => "shadow map",
            Self::ShadowMapArray => "shadow map array",
            Self::ShadowPipeline => "shadow pipeline",
            Self::RenderTargets => "render targets",
            Self::PostprocessTargets => "postprocess targets",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DestroyRequest {
    pub(crate) kind: ResourceKind,
    pub(crate) id: ResourceId,
}

#[derive(Default)]
pub(crate) struct DropQueue(RefCell<VecDeque<DestroyRequest>>);

impl DropQueue {
    pub(crate) fn take_bounded(&self, limit: usize) -> Vec<DestroyRequest> {
        let mut pending = self.0.borrow_mut();
        let count = limit.min(pending.len());
        pending.drain(..count).collect()
    }

    pub(crate) fn restore_front(&self, requests: Vec<DestroyRequest>) {
        let mut pending = self.0.borrow_mut();
        for request in requests.into_iter().rev() {
            pending.push_front(request);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.borrow().len()
    }
}

pub(crate) struct ResourceLease {
    pub(crate) session: u64,
    pub(crate) id: ResourceId,
    kind: ResourceKind,
    drops: Rc<DropQueue>,
    armed: bool,
}

impl fmt::Debug for ResourceLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceLease")
            .field("session", &self.session)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ResourceLease {
    fn eq(&self, other: &Self) -> bool {
        self.session == other.session && self.id == other.id && self.kind == other.kind
    }
}

impl Eq for ResourceLease {}

impl ResourceLease {
    pub(crate) fn new(
        session: u64,
        id: ResourceId,
        kind: ResourceKind,
        drops: Rc<DropQueue>,
    ) -> Self {
        Self {
            session,
            id,
            kind,
            drops,
            armed: true,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ResourceLease {
    fn drop(&mut self) {
        if self.armed {
            self.drops.0.borrow_mut().push_back(DestroyRequest {
                kind: self.kind,
                id: self.id,
            });
        }
    }
}

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

pub(crate) struct Arena<T> {
    label: &'static str,
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}

impl<T> Arena<T> {
    pub(crate) const fn new(label: &'static str) -> Self {
        Self {
            label,
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    pub(crate) fn insert(&mut self, value: T) -> Result<ResourceId, GraphicsError> {
        if let Some(slot) = self.free.pop() {
            let index = usize::try_from(slot).expect("u32 slot fits usize");
            debug_assert!(self.slots[index].value.is_none());
            self.slots[index].value = Some(value);
            return Ok(ResourceId {
                slot,
                generation: self.slots[index].generation,
            });
        }
        let slot = u32::try_from(self.slots.len()).map_err(|_| {
            GraphicsError::internal(format!("{} identity space is exhausted", self.label))
        })?;
        self.slots.push(Slot {
            generation: 1,
            value: Some(value),
        });
        Ok(ResourceId {
            slot,
            generation: 1,
        })
    }

    pub(crate) fn get(&self, id: ResourceId) -> Result<&T, GraphicsError> {
        self.slots
            .get(usize::try_from(id.slot).expect("u32 slot fits usize"))
            .filter(|slot| slot.generation == id.generation)
            .and_then(|slot| slot.value.as_ref())
            .ok_or_else(|| {
                GraphicsError::stale_resource(format!("stale or invalid {} handle", self.label))
            })
    }

    pub(crate) fn index_of(&self, id: ResourceId) -> Result<usize, GraphicsError> {
        self.get(id)?;
        Ok(usize::try_from(id.slot).expect("u32 slot fits usize"))
    }

    pub(crate) fn remove(&mut self, id: ResourceId) -> Result<T, GraphicsError> {
        let index = usize::try_from(id.slot).expect("u32 slot fits usize");
        let slot = self
            .slots
            .get_mut(index)
            .filter(|slot| slot.generation == id.generation)
            .ok_or_else(|| {
                GraphicsError::stale_resource(format!("stale or invalid {} handle", self.label))
            })?;
        let value = slot.value.take().ok_or_else(|| {
            GraphicsError::stale_resource(format!("stale or invalid {} handle", self.label))
        })?;
        if let Some(generation) = slot.generation.checked_add(1) {
            slot.generation = generation;
            self.free.push(id.slot);
        }
        Ok(value)
    }

    pub(crate) fn remove_if_live(&mut self, id: ResourceId) -> Option<T> {
        self.remove(id).ok()
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.slots.iter_mut().filter_map(|slot| slot.value.as_mut())
    }

    pub(crate) fn take_all(&mut self) -> Vec<T> {
        self.free.clear();
        self.slots.drain(..).filter_map(|slot| slot.value).collect()
    }
}

impl<T> Index<usize> for Arena<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        self.slots[index]
            .value
            .as_ref()
            .expect("validated resource slot is live")
    }
}

impl<T> IndexMut<usize> for Arena<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.slots[index]
            .value
            .as_mut()
            .expect("validated resource slot is live")
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use crate::GraphicsErrorKind;

    use super::{Arena, DropQueue, ResourceKind, ResourceLease};

    #[test]
    fn removed_slots_are_reused_without_reviving_stale_ids() {
        let mut arena = Arena::new("test resource");
        let first = arena.insert(11).expect("first insertion");
        assert_eq!(arena.remove(first), Ok(11));
        assert_eq!(
            arena
                .get(first)
                .expect_err("removed ID must be stale")
                .kind(),
            GraphicsErrorKind::StaleResource
        );
        let replacement = arena.insert(22).expect("replacement insertion");
        assert_ne!(first, replacement);
        assert_eq!(arena.get(replacement), Ok(&22));
        assert_eq!(
            arena
                .get(first)
                .expect_err("reused slot must not revive stale ID")
                .kind(),
            GraphicsErrorKind::StaleResource
        );
    }

    #[test]
    fn repeated_churn_keeps_the_arena_bounded() {
        let mut arena = Arena::new("test resource");
        for value in 0..1_000 {
            let id = arena.insert(value).expect("insertion");
            assert_eq!(arena.remove(id), Ok(value));
        }
        assert_eq!(arena.slots.len(), 1);
        assert_eq!(arena.free.len(), 1);
    }

    #[test]
    fn dropping_a_lease_queues_its_identity_once() {
        let mut arena = Arena::new("test resource");
        let id = arena.insert(()).expect("insertion");
        let drops = Rc::new(DropQueue::default());
        drop(ResourceLease::new(
            7,
            id,
            ResourceKind::Mesh,
            Rc::clone(&drops),
        ));
        let pending = drops.take_bounded(usize::MAX);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].kind, ResourceKind::Mesh);
        assert!(drops.take_bounded(usize::MAX).is_empty());
    }

    #[test]
    fn large_drop_batch_is_taken_in_bounded_fifo_chunks() {
        let mut arena = Arena::new("test resource");
        let drops = Rc::new(DropQueue::default());
        for _ in 0..41 {
            let id = arena.insert(()).expect("insertion");
            drop(ResourceLease::new(
                7,
                id,
                ResourceKind::Mesh,
                Rc::clone(&drops),
            ));
        }

        let first = drops.take_bounded(8);
        assert_eq!(first.len(), 8);
        assert_eq!(drops.len(), 33);

        let second = drops.take_bounded(8);
        assert_eq!(second.len(), 8);
        assert_eq!(drops.len(), 25);
        assert_ne!(first[0].id, second[0].id);

        drops.restore_front(first.clone());
        assert_eq!(drops.len(), 33);
        assert_eq!(drops.take_bounded(8), first);
    }
}
