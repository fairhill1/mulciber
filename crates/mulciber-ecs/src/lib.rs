//! A small sparse-set entity component store.
//!
//! Layout: each component type lives in its own [`SparseSet`], a pair of parallel dense arrays
//! (entities and component values) plus a sparse index keyed by entity slot. Dense storage keeps
//! iteration cache-friendly; the sparse index makes insert, remove and lookup O(1). Removal
//! swap-pops the dense arrays, so iteration order is unspecified.
//!
//! Entities are a slot index plus a generation. Despawning bumps the slot's generation, so stale
//! [`Entity`] handles held across a despawn miss lookups instead of aliasing a recycled slot. A
//! slot whose generation is spent retires rather than wrapping back to a generation an old handle
//! could still carry.
//!
//! There are no systems, schedules or built-in components: any `'static` type is a component, and
//! the game decides what runs when.
//!
//! ```
//! use mulciber_ecs::World;
//!
//! struct Position(f32, f32);
//! struct Velocity(f32, f32);
//!
//! let mut world = World::new();
//! let ball = world.spawn();
//! world.insert(ball, Position(0.0, 0.0));
//! world.insert(ball, Velocity(1.0, 2.0));
//!
//! for (_, position, velocity) in world.query2_mut::<Position, Velocity>() {
//!     position.0 += velocity.0;
//!     position.1 += velocity.1;
//! }
//! assert_eq!(world.get::<Position>(ball).map(|p| (p.0, p.1)), Some((1.0, 2.0)));
//! ```

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// A handle to an entity: a slot index plus the generation the slot had when spawned.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Entity {
    index: u32,
    generation: u32,
}

impl Entity {
    /// The entity's slot. Slots are reused after a despawn, so on its own this is not an
    /// identity; pair it with [`Entity::generation`] or compare whole handles.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// The slot's generation when this entity was spawned.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Marker for components. Blanket-implemented for every `'static` type.
pub trait Component: 'static {}
impl<T: 'static> Component for T {}

const EMPTY: u32 = u32::MAX;

/// Dense storage for one component type, indexed sparsely by entity slot.
pub struct SparseSet<T> {
    /// Maps entity slot to position in the dense arrays, or `EMPTY`.
    sparse: Vec<u32>,
    entities: Vec<Entity>,
    data: Vec<T>,
}

impl<T> Default for SparseSet<T> {
    fn default() -> Self {
        Self {
            sparse: Vec::new(),
            entities: Vec::new(),
            data: Vec::new(),
        }
    }
}

impl<T> SparseSet<T> {
    /// Dense position for `entity`, if present with a matching generation.
    fn dense_index(&self, entity: Entity) -> Option<usize> {
        let slot = *self.sparse.get(entity.index as usize)?;
        if slot == EMPTY {
            return None;
        }
        let dense = slot as usize;
        (self.entities[dense] == entity).then_some(dense)
    }

    /// Inserts `value` for `entity`, returning the previous value if one existed.
    ///
    /// # Panics
    ///
    /// If the set already holds `u32::MAX - 1` values, the most its dense index can address.
    pub fn insert(&mut self, entity: Entity, value: T) -> Option<T> {
        if let Some(dense) = self.dense_index(entity) {
            return Some(std::mem::replace(&mut self.data[dense], value));
        }
        let dense = u32::try_from(self.data.len())
            .ok()
            .filter(|&dense| dense != EMPTY)
            .expect("sparse set holds fewer than u32::MAX values");
        let index = entity.index as usize;
        if index >= self.sparse.len() {
            self.sparse.resize(index + 1, EMPTY);
        }
        self.sparse[index] = dense;
        self.entities.push(entity);
        self.data.push(value);
        None
    }

    /// Removes and returns the component, swap-popping the dense arrays.
    pub fn remove(&mut self, entity: Entity) -> Option<T> {
        let dense = self.dense_index(entity)?;
        let last = self.entities.len() - 1;
        self.entities.swap_remove(dense);
        let value = self.data.swap_remove(dense);
        let slot = std::mem::replace(&mut self.sparse[entity.index as usize], EMPTY);
        if dense != last {
            // The former tail moved into the vacated position; repoint its sparse entry.
            self.sparse[self.entities[dense].index as usize] = slot;
        }
        Some(value)
    }

    /// The component of `entity`, if it has one.
    #[must_use]
    pub fn get(&self, entity: Entity) -> Option<&T> {
        self.dense_index(entity).map(|dense| &self.data[dense])
    }

    /// The component of `entity`, mutably, if it has one.
    pub fn get_mut(&mut self, entity: Entity) -> Option<&mut T> {
        self.dense_index(entity).map(|dense| &mut self.data[dense])
    }

    /// Whether `entity` has a component in this set.
    #[must_use]
    pub fn contains(&self, entity: Entity) -> bool {
        self.dense_index(entity).is_some()
    }

    /// Number of components stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether no components are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Every entity in the set with its component, in dense order.
    pub fn iter(&self) -> impl Iterator<Item = (Entity, &T)> {
        self.entities.iter().copied().zip(self.data.iter())
    }

    /// Every entity in the set with its component mutably, in dense order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Entity, &mut T)> {
        self.entities.iter().copied().zip(self.data.iter_mut())
    }
}

/// Object-safe view of a sparse set so the world can despawn without knowing `T`.
trait AnyStorage: Any {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn remove_entity(&mut self, entity: Entity);
}

impl<T: Component> AnyStorage for SparseSet<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn remove_entity(&mut self, entity: Entity) {
        self.remove(entity);
    }
}

/// Hashes a `TypeId`, which is already a well-mixed hash, by passing its word through rather
/// than running `SipHash` on every component lookup. Anything written as bytes is folded FNV-1a
/// style, in case `TypeId`'s `Hash` ever stops writing a single word.
#[derive(Default)]
struct TypeIdHasher(u64);

impl Hasher for TypeIdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 ^= value;
    }
}

type Storages = HashMap<TypeId, Box<dyn AnyStorage>, BuildHasherDefault<TypeIdHasher>>;

/// Entity allocator plus one sparse set per component type in use.
#[derive(Default)]
pub struct World {
    /// Current generation per slot.
    generations: Vec<u32>,
    /// Slots available for reuse.
    free: Vec<u32>,
    storages: Storages,
}

impl World {
    /// An empty world.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawns an entity with no components, reusing a freed slot when there is one.
    ///
    /// # Panics
    ///
    /// If every one of the `u32::MAX` slots is live or retired.
    pub fn spawn(&mut self) -> Entity {
        if let Some(index) = self.free.pop() {
            return Entity {
                index,
                generation: self.generations[index as usize],
            };
        }
        let index = u32::try_from(self.generations.len())
            .ok()
            .filter(|&index| index != u32::MAX)
            .expect("fewer than u32::MAX entity slots");
        self.generations.push(0);
        Entity {
            index,
            generation: 0,
        }
    }

    /// Removes the entity and all of its components. No-op if it is already dead.
    pub fn despawn(&mut self, entity: Entity) {
        if !self.is_alive(entity) {
            return;
        }
        for storage in self.storages.values_mut() {
            storage.remove_entity(entity);
        }
        // Live generations are below `u32::MAX`, so this can't overflow. A slot that reaches it
        // retires: wrapping would revive handles from its first generations.
        let generation = &mut self.generations[entity.index as usize];
        *generation += 1;
        if *generation != u32::MAX {
            self.free.push(entity.index);
        }
    }

    /// A handle is live while its generation matches the slot's current one; `despawn` bumps
    /// the slot's generation, so a freed slot never matches a stale handle.
    #[must_use]
    pub fn is_alive(&self, entity: Entity) -> bool {
        entity.generation != u32::MAX
            && self.generations.get(entity.index as usize) == Some(&entity.generation)
    }

    /// The set holding every `T`, if any `T` was ever inserted.
    #[must_use]
    pub fn storage<T: Component>(&self) -> Option<&SparseSet<T>> {
        self.storages
            .get(&TypeId::of::<T>())
            .and_then(|s| s.as_any().downcast_ref())
    }

    fn storage_mut<T: Component>(&mut self) -> Option<&mut SparseSet<T>> {
        self.storages
            .get_mut(&TypeId::of::<T>())
            .and_then(|s| s.as_any_mut().downcast_mut())
    }

    /// Attaches a component, replacing and returning any existing one.
    ///
    /// # Panics
    ///
    /// On a dead entity, to catch stale-handle bugs early.
    pub fn insert<T: Component>(&mut self, entity: Entity, value: T) -> Option<T> {
        assert!(self.is_alive(entity), "insert on dead entity {entity:?}");
        self.storages
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(SparseSet::<T>::default()))
            .as_any_mut()
            .downcast_mut::<SparseSet<T>>()
            .expect("storage keyed by TypeId")
            .insert(entity, value)
    }

    /// Detaches and returns the entity's `T`, if it has one.
    pub fn remove<T: Component>(&mut self, entity: Entity) -> Option<T> {
        self.storage_mut::<T>()?.remove(entity)
    }

    /// The entity's `T`, if it has one.
    #[must_use]
    pub fn get<T: Component>(&self, entity: Entity) -> Option<&T> {
        self.storage::<T>()?.get(entity)
    }

    /// The entity's `T` mutably, if it has one.
    pub fn get_mut<T: Component>(&mut self, entity: Entity) -> Option<&mut T> {
        self.storage_mut::<T>()?.get_mut(entity)
    }

    /// Whether the entity has a `T`.
    #[must_use]
    pub fn has<T: Component>(&self, entity: Entity) -> bool {
        self.storage::<T>().is_some_and(|s| s.contains(entity))
    }

    /// Iterates every entity with a `T`.
    pub fn query<T: Component>(&self) -> impl Iterator<Item = (Entity, &T)> {
        self.storage::<T>().into_iter().flat_map(SparseSet::iter)
    }

    /// Iterates every entity with a `T`, mutably.
    pub fn query_mut<T: Component>(&mut self) -> impl Iterator<Item = (Entity, &mut T)> {
        self.storage_mut::<T>()
            .into_iter()
            .flat_map(SparseSet::iter_mut)
    }

    /// Iterates entities that have both an `A` and a `B`, driven by the `A` set.
    pub fn query2<A: Component, B: Component>(&self) -> impl Iterator<Item = (Entity, &A, &B)> {
        let b = self.storage::<B>();
        self.query::<A>()
            .filter_map(move |(entity, a)| Some((entity, a, b?.get(entity)?)))
    }

    /// Iterates entities that have both, with `A` mutable and `B` shared, driven by the `A` set.
    ///
    /// # Panics
    ///
    /// If `A` and `B` are the same type; use [`World::query_mut`] for that.
    pub fn query2_mut<A: Component, B: Component>(
        &mut self,
    ) -> impl Iterator<Item = (Entity, &mut A, &B)> {
        assert_ne!(
            TypeId::of::<A>(),
            TypeId::of::<B>(),
            "query2_mut requires distinct types"
        );
        let [a, b] = self
            .storages
            .get_disjoint_mut([&TypeId::of::<A>(), &TypeId::of::<B>()]);
        let b: Option<&SparseSet<B>> = b.and_then(|s| s.as_any().downcast_ref());
        a.and_then(|s| s.as_any_mut().downcast_mut::<SparseSet<A>>())
            .into_iter()
            .flat_map(SparseSet::iter_mut)
            .filter_map(move |(entity, a)| Some((entity, a, b?.get(entity)?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Pos(f32, f32);
    #[derive(Debug, PartialEq)]
    struct Vel(f32, f32);

    #[test]
    fn insert_get_remove() {
        let mut world = World::new();
        let e = world.spawn();
        assert_eq!(world.insert(e, Pos(1.0, 2.0)), None);
        assert_eq!(world.get::<Pos>(e), Some(&Pos(1.0, 2.0)));
        assert!(world.has::<Pos>(e));
        assert_eq!(world.insert(e, Pos(3.0, 4.0)), Some(Pos(1.0, 2.0)));
        assert_eq!(world.remove::<Pos>(e), Some(Pos(3.0, 4.0)));
        assert_eq!(world.get::<Pos>(e), None);
        assert!(!world.has::<Pos>(e));
    }

    #[test]
    fn swap_remove_keeps_sparse_index_consistent() {
        let mut world = World::new();
        let entities: Vec<_> = (0..4).map(|_| world.spawn()).collect();
        for (i, &e) in (0_u8..).zip(&entities) {
            world.insert(e, Pos(f32::from(i), 0.0));
        }
        // Removing the first entry swap-pops the last into its place.
        world.remove::<Pos>(entities[0]);
        assert_eq!(world.get::<Pos>(entities[3]), Some(&Pos(3.0, 0.0)));
        assert_eq!(world.get::<Pos>(entities[1]), Some(&Pos(1.0, 0.0)));
        assert_eq!(world.query::<Pos>().count(), 3);
        assert_eq!(world.storage::<Pos>().map(SparseSet::len), Some(3));
    }

    #[test]
    fn despawn_recycles_slot_with_new_generation() {
        let mut world = World::new();
        let old = world.spawn();
        world.insert(old, Pos(1.0, 1.0));
        world.despawn(old);
        assert!(!world.is_alive(old));

        let new = world.spawn();
        assert_eq!(new.index(), old.index());
        assert_ne!(new, old);
        // The recycled slot must not see the old entity's components.
        assert_eq!(world.get::<Pos>(new), None);
        assert_eq!(world.get::<Pos>(old), None);
    }

    #[test]
    fn stale_handle_misses_after_recycle() {
        let mut world = World::new();
        let old = world.spawn();
        world.despawn(old);
        let new = world.spawn();
        world.insert(new, Pos(5.0, 5.0));
        // Same slot, older generation: the stale handle sees nothing.
        assert_eq!(world.get::<Pos>(old), None);
        assert!(world.get_mut::<Pos>(old).is_none());
        assert_eq!(world.remove::<Pos>(old), None);
    }

    #[test]
    fn spent_slots_retire_instead_of_wrapping() {
        let mut world = World::new();
        let first = world.spawn();
        world.generations[first.index as usize] = u32::MAX - 1;
        let last = Entity {
            index: first.index,
            generation: u32::MAX - 1,
        };
        world.despawn(last);
        assert!(!world.is_alive(last));
        assert!(!world.is_alive(first));
        // The slot is not reused; a fresh one is.
        let next = world.spawn();
        assert_ne!(next.index(), first.index());
        assert!(world.is_alive(next));
    }

    #[test]
    fn query2_mut_joins_and_mutates() {
        let mut world = World::new();
        let moving = world.spawn();
        world.insert(moving, Pos(0.0, 0.0));
        world.insert(moving, Vel(1.0, 2.0));
        let still = world.spawn();
        world.insert(still, Pos(9.0, 9.0));

        for (_, pos, vel) in world.query2_mut::<Pos, Vel>() {
            pos.0 += vel.0;
            pos.1 += vel.1;
        }
        assert_eq!(world.get::<Pos>(moving), Some(&Pos(1.0, 2.0)));
        assert_eq!(world.get::<Pos>(still), Some(&Pos(9.0, 9.0)));
        assert_eq!(world.query2::<Pos, Vel>().count(), 1);
    }

    #[test]
    fn queries_on_unused_types_are_empty() {
        let mut world = World::new();
        let e = world.spawn();
        assert_eq!(world.query::<Pos>().count(), 0);
        assert_eq!(world.query_mut::<Pos>().count(), 0);
        assert_eq!(world.query2_mut::<Pos, Vel>().count(), 0);
        assert!(world.storage::<Pos>().is_none());
        assert_eq!(world.remove::<Pos>(e), None);
    }

    #[test]
    fn distinct_types_get_distinct_storages() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, 1_u32);
        world.insert(e, 2_u64);
        world.insert(e, Pos(0.0, 0.0));
        assert_eq!(world.get::<u32>(e), Some(&1));
        assert_eq!(world.get::<u64>(e), Some(&2));
        assert_eq!(world.storages.len(), 3);
    }

    #[test]
    #[should_panic(expected = "insert on dead entity")]
    fn insert_on_a_dead_entity_panics() {
        let mut world = World::new();
        let e = world.spawn();
        world.despawn(e);
        world.insert(e, Pos(0.0, 0.0));
    }
}
