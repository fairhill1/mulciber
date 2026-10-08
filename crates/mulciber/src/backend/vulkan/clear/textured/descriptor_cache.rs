//! Descriptor-set caches keyed by the resources each set names.
//!
//! Every record looks its set up on every submission, and a scene of thousands of records over
//! hundreds of distinct texture tuples per pipeline cannot afford a scan per record. The caches
//! are hash maps with a small multiply-rotate hasher. The sampled-tuple key is held inline and
//! borrows as the record's own slice, so a hit is probed with that slice and copies nothing, and
//! only a miss builds a key, without allocating.
use core::borrow::Borrow;
use core::hash::{BuildHasherDefault, Hash, Hasher};
use std::collections::HashMap;

use super::{ResourceId, vk};
use crate::MATERIAL_TEXTURE_COUNT_LIMIT;

/// Most resources one sampled tuple names: every declared texture, the shadow map and the scene
/// depth snapshot all occupy texture slots, which validation caps at this count per pipeline.
const SAMPLED_KEY_CAPACITY: usize = MATERIAL_TEXTURE_COUNT_LIMIT as usize;

/// Sets cached per sampled-identity tuple until a pool reset.
pub(super) type SampledSets = HashMap<SampledKey, vk::VkDescriptorSet, BuildFxHasher>;

/// Sets cached per resource and frame slot until a pool reset.
pub(super) type KeyedSets = HashMap<(ResourceId, usize), vk::VkDescriptorSet, BuildFxHasher>;

/// An ordered tuple of sampled identities stored inline; equality and hashing see only the
/// occupied prefix, exactly as the slice it borrows as, so order and length both distinguish keys.
#[derive(Clone, Copy)]
pub(super) struct SampledKey {
    ids: [ResourceId; SAMPLED_KEY_CAPACITY],
    len: u8,
}

impl SampledKey {
    /// The key for `ids`, or `None` when it names more resources than a pipeline may declare.
    pub(super) fn new(ids: &[ResourceId]) -> Option<Self> {
        if ids.len() > SAMPLED_KEY_CAPACITY {
            return None;
        }
        let mut key = Self {
            ids: [ResourceId::UNUSED; SAMPLED_KEY_CAPACITY],
            len: u8::try_from(ids.len()).expect("key capacity fits u8"),
        };
        key.ids[..ids.len()].copy_from_slice(ids);
        Some(key)
    }

    fn as_slice(&self) -> &[ResourceId] {
        &self.ids[..usize::from(self.len)]
    }
}

impl Borrow<[ResourceId]> for SampledKey {
    fn borrow(&self) -> &[ResourceId] {
        self.as_slice()
    }
}

impl PartialEq for SampledKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for SampledKey {}

impl Hash for SampledKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

pub(super) type BuildFxHasher = BuildHasherDefault<FxHasher>;

/// `FxHash`'s word mix for small integer keys: no keying, since identities are not attacker-chosen
/// and the maps are rebuilt on every pool reset. The final rotation brings the well-mixed high
/// bits down to where the table takes its bucket index.
#[derive(Default)]
pub(super) struct FxHasher {
    hash: u64,
}

impl FxHasher {
    const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(Self::SEED);
    }
}

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for &word in words {
            self.add(u64::from_le_bytes(word));
        }
        if !rest.is_empty() {
            let mut word = [0; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(word));
        }
    }

    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    fn write_u16(&mut self, value: u16) {
        self.add(u64::from(value));
    }

    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }

    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    fn finish(&self) -> u64 {
        self.hash.rotate_left(26)
    }
}

#[cfg(test)]
mod tests {
    use core::hash::BuildHasher;
    use core::hint::black_box;
    use std::collections::{HashMap, HashSet};
    use std::println;
    use std::time::Instant;
    use std::vec::Vec;

    use super::{BuildFxHasher, SAMPLED_KEY_CAPACITY, SampledKey};
    use crate::resource::{Arena, ResourceId};

    fn ids(count: usize) -> Vec<ResourceId> {
        let mut arena = Arena::new("test texture");
        (0..count)
            .map(|_| arena.insert(()).expect("insertion"))
            .collect()
    }

    fn hash(key: &SampledKey) -> u64 {
        BuildFxHasher::default().hash_one(key)
    }

    #[test]
    fn sampled_keys_compare_by_the_occupied_tuple() {
        let ids = ids(3);
        let key = SampledKey::new(&ids).expect("fits");
        assert!(key == SampledKey::new(&ids).expect("fits"));
        assert_eq!(hash(&key), hash(&SampledKey::new(&ids).expect("fits")));
        assert!(key != SampledKey::new(&ids[..2]).expect("fits"));
        assert!(key != SampledKey::new(&[ids[0], ids[2], ids[1]]).expect("fits"));
        assert!(SampledKey::new(&[]).expect("fits") == SampledKey::new(&[]).expect("fits"));
        assert!(SampledKey::new(&[]).expect("fits") != SampledKey::new(&ids[..1]).expect("fits"));
    }

    #[test]
    fn sampled_keys_hash_distinct_and_reordered_tuples_apart() {
        let ids = ids(64);
        let mut hashes = HashSet::new();
        let mut keys = 0;
        for &first in &ids {
            for &second in &ids {
                assert!(hashes.insert(hash(&SampledKey::new(&[first, second]).expect("fits"))));
                keys += 1;
            }
            assert!(hashes.insert(hash(&SampledKey::new(&[first]).expect("fits"))));
            keys += 1;
        }
        assert_eq!(hashes.len(), keys);
        let forward = SampledKey::new(&ids[..4]).expect("fits");
        let reversed: Vec<_> = ids[..4].iter().rev().copied().collect();
        assert_ne!(
            hash(&forward),
            hash(&SampledKey::new(&reversed).expect("fits"))
        );
    }

    #[test]
    fn sampled_sets_are_probed_with_the_records_slice() {
        let ids = ids(3);
        let mut sets = HashMap::<SampledKey, usize, BuildFxHasher>::default();
        sets.insert(SampledKey::new(&ids).expect("fits"), 1);
        sets.insert(SampledKey::new(&ids[..2]).expect("fits"), 2);
        sets.insert(SampledKey::new(&[]).expect("fits"), 3);
        assert_eq!(sets.get(ids.as_slice()), Some(&1));
        assert_eq!(sets.get(&ids[..2]), Some(&2));
        assert_eq!(sets.get(&[][..]), Some(&3));
        assert_eq!(sets.get(&[ids[1], ids[0]][..]), None);
        sets.clear();
        assert_eq!(sets.get(ids.as_slice()), None);
    }

    #[test]
    fn sampled_keys_refuse_more_than_a_pipeline_declares() {
        let ids = ids(SAMPLED_KEY_CAPACITY + 1);
        assert!(SampledKey::new(&ids[..SAMPLED_KEY_CAPACITY]).is_some());
        assert!(SampledKey::new(&ids).is_none());
    }

    /// Times a frame of material lookups against the scan this cache replaced.
    #[test]
    #[ignore = "timing; run by hand in release"]
    fn sampled_lookup_timing() {
        const LOOKUPS: usize = 3000;
        const FRAMES: u32 = 200;
        let pool = ids(4096);
        for tuples in [16, 100, 500] {
            let keys: Vec<Vec<ResourceId>> = (0..tuples)
                .map(|tuple| {
                    (0..4)
                        .map(|slot| pool[(tuple * 7 + slot * 977) % 4096])
                        .collect()
                })
                .collect();
            let mut state = 0x2545_f491_u32;
            let order: Vec<usize> = (0..LOOKUPS)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    state as usize % tuples
                })
                .collect();
            let linear: Vec<(Vec<ResourceId>, usize)> = keys
                .iter()
                .enumerate()
                .map(|(index, key)| (key.clone(), index))
                .collect();
            let hashed: HashMap<SampledKey, usize, BuildFxHasher> = keys
                .iter()
                .enumerate()
                .map(|(index, key)| (SampledKey::new(key).expect("fits"), index))
                .collect();
            let start = Instant::now();
            for _ in 0..FRAMES {
                for &index in &order {
                    let wanted = black_box(keys[index].as_slice());
                    let found = linear.iter().find(|(ids, _)| ids.as_slice() == wanted);
                    black_box(found.map(|(_, set)| *set));
                }
            }
            let scan = start.elapsed() / FRAMES;
            let start = Instant::now();
            for _ in 0..FRAMES {
                for &index in &order {
                    let wanted = black_box(keys[index].as_slice());
                    let found = hashed.get(wanted);
                    black_box(found.copied());
                }
            }
            let map = start.elapsed() / FRAMES;
            println!("{tuples} tuples, {LOOKUPS} lookups per frame: scan {scan:?}, hashed {map:?}");
        }
    }
}
