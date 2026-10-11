//! Renderer-local reuse. Values contain pixels, never paragraphs or host leases.
use rustc_hash::FxHashMap;
use std::{hash::Hash, sync::Arc};

pub(super) const IDLE_IMAGES: usize = 50;
pub(super) const IDLE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
struct Entry<V> {
    value: Arc<V>,
    bytes: usize,
    touched: u64,
}

#[derive(Debug)]
pub(super) struct Cache<K, V> {
    entries: FxHashMap<K, Entry<V>>,
    clock: u64,
}

impl<K: Eq + Hash, V> Cache<K, V> {
    pub(super) fn new() -> Self {
        Self {
            entries: FxHashMap::default(),
            clock: 0,
        }
    }

    pub(super) fn get(&mut self, key: &K) -> Option<Arc<V>> {
        let entry = self.entries.get_mut(key)?;
        self.clock += 1;
        entry.touched = self.clock;
        Some(entry.value.clone())
    }

    pub(super) fn insert(&mut self, key: K, value: Arc<V>, bytes: usize) {
        self.clock += 1;
        self.entries.insert(
            key,
            Entry {
                value,
                bytes,
                touched: self.clock,
            },
        );
    }

    pub(super) fn trim(&mut self) {
        self.trim_to(IDLE_IMAGES, IDLE_BYTES);
    }

    fn trim_to(&mut self, max_images: usize, max_bytes: usize) {
        self.entries
            .retain(|_, entry| Arc::strong_count(&entry.value) > 1 || entry.bytes <= max_bytes);
        let mut idle = Vec::new();
        let mut bytes = 0;
        for entry in self.entries.values_mut() {
            if Arc::strong_count(&entry.value) == 1 {
                idle.push((entry.touched, entry.bytes));
                bytes += entry.bytes;
            } else {
                // An image used by an active occurrence stays recent even when
                // that occurrence uses the paragraph-identity fast path.
                self.clock += 1;
                entry.touched = self.clock;
            }
        }
        if idle.len() <= max_images && bytes <= max_bytes {
            return;
        }
        idle.sort_unstable_by_key(|&(touched, _)| touched);
        let mut count = idle.len();
        let mut cutoff = 0;
        for (touched, size) in idle {
            if count <= max_images && bytes <= max_bytes {
                break;
            }
            count -= 1;
            bytes -= size;
            cutoff = touched;
        }
        self.entries
            .retain(|_, entry| Arc::strong_count(&entry.value) > 1 || entry.touched > cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_idle_values_obey_count_and_byte_limits() {
        let mut cache = Cache::new();
        for key in 0..60 {
            cache.insert(key, Arc::new(key), 1024);
        }
        assert_eq!(*cache.get(&0).unwrap(), 0); // Touch the oldest entry.
        cache.trim();
        assert_eq!(cache.entries.len(), 50);
        assert!(cache.get(&0).is_some());
        assert!(cache.get(&1).is_none());
        cache.trim_to(50, 2048);
        assert_eq!(cache.entries.len(), 2);
        assert!(cache.get(&0).is_some());
        assert!(cache.get(&59).is_some());
        cache.trim_to(0, 0);
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn active_values_survive_pressure_then_retire_into_idle_lru() {
        let mut cache = Cache::new();
        let live: Vec<_> = (0..64).map(Arc::new).collect();
        for (key, value) in live.iter().enumerate() {
            cache.insert(key, value.clone(), 1024 * 1024);
        }
        for key in 64..124 {
            cache.insert(key, Arc::new(key), 1024 * 1024);
        }
        cache.trim();
        assert_eq!(cache.entries.len(), 114); // 64 active, 50 idle.
        assert!(cache.get(&0).is_some());
        drop(live);
        cache.trim();
        assert_eq!(cache.entries.len(), 50);
        assert!(cache.get(&0).is_some());
        assert!(cache.get(&123).is_none());
    }

    #[test]
    fn bytes_alone_can_evict_and_external_owners_remain_valid() {
        let mut cache = Cache::new();
        let live = Arc::new(1);
        cache.insert(1, live.clone(), 80 * 1024 * 1024);
        cache.insert(2, Arc::new(2), 40 * 1024 * 1024);
        cache.insert(3, Arc::new(3), 40 * 1024 * 1024);
        cache.trim();
        assert_eq!(cache.entries.len(), 2);
        assert!(cache.get(&2).is_none());
        assert_eq!(*live, 1);
        drop(live);
        cache.trim();
        assert!(cache.get(&1).is_none());
        assert!(cache.get(&3).is_some());
        let weak = Arc::downgrade(&cache.get(&3).unwrap());
        drop(cache);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn hash_collisions_require_full_key_equality() {
        #[derive(Debug, PartialEq, Eq)]
        struct Key(u32);
        impl Hash for Key {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                0_u32.hash(state);
            }
        }
        let mut cache = Cache::new();
        cache.insert(Key(1), Arc::new("first"), 1);
        cache.insert(Key(2), Arc::new("second"), 1);
        assert_eq!(*cache.get(&Key(1)).unwrap(), "first");
        assert_eq!(*cache.get(&Key(2)).unwrap(), "second");
        assert!(cache.get(&Key(3)).is_none());
    }
}
