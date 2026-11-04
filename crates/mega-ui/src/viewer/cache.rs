//! Bounded LRU of loaded tiles, keyed by `(TileKey, generation)`.

use std::collections::{BTreeMap, HashMap};

use super::TileKey;

/// Default cache capacity, in tiles.
pub const DEFAULT_CAPACITY: usize = 512;

struct Slot<V> {
    generation: u64,
    used: u64,
    value: V,
}

/// LRU cache holding at most one entry per tile: the newest generation
/// loaded. An older generation stays available as a drawable fallback until
/// a newer one replaces it.
pub(crate) struct TileCache<V> {
    capacity: usize,
    tick: u64,
    map: HashMap<TileKey, Slot<V>>,
    lru: BTreeMap<u64, TileKey>,
}

impl<V> TileCache<V> {
    /// An empty cache holding at most `capacity` tiles.
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            tick: 0,
            map: HashMap::new(),
            lru: BTreeMap::new(),
        }
    }

    /// Number of cached tiles.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    /// True if `key` is cached at exactly `generation` (no LRU touch).
    pub(crate) fn has_current(&self, key: TileKey, generation: u64) -> bool {
        self.map
            .get(&key)
            .is_some_and(|s| s.generation == generation)
    }

    /// The generation `key` is cached at, if any (no LRU touch).
    pub(crate) fn generation_of(&self, key: TileKey) -> Option<u64> {
        self.map.get(&key).map(|s| s.generation)
    }

    /// The cached entry for `key` at any generation, with that generation;
    /// marks it most recently used.
    pub(crate) fn get(&mut self, key: TileKey) -> Option<(u64, &V)> {
        self.tick += 1;
        let slot = self.map.get_mut(&key)?;
        self.lru.remove(&slot.used);
        slot.used = self.tick;
        self.lru.insert(self.tick, key);
        Some((slot.generation, &slot.value))
    }

    /// Insert `value` for `key` at `generation`, unless a newer generation
    /// is already cached. Evicts least recently used tiles over capacity.
    pub(crate) fn insert(&mut self, key: TileKey, generation: u64, value: V) {
        if let Some(old) = self.map.get(&key) {
            if old.generation > generation {
                return;
            }
            self.lru.remove(&old.used);
        }
        self.tick += 1;
        self.lru.insert(self.tick, key);
        self.map.insert(
            key,
            Slot {
                generation,
                used: self.tick,
                value,
            },
        );
        while self.map.len() > self.capacity {
            let Some((_, oldest)) = self.lru.pop_first() else {
                break;
            };
            self.map.remove(&oldest);
        }
    }

    /// Drop every entry.
    pub(crate) fn clear(&mut self) {
        self.map.clear();
        self.lru.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(tx: u32) -> TileKey {
        TileKey {
            level: 0,
            tx,
            ty: 0,
        }
    }

    #[test]
    fn evicts_least_recently_used() {
        let mut c = TileCache::new(2);
        c.insert(k(0), 1, "a");
        c.insert(k(1), 1, "b");
        assert!(c.get(k(0)).is_some()); // k(1) is now oldest
        c.insert(k(2), 1, "c");
        assert_eq!(c.len(), 2);
        assert!(c.get(k(1)).is_none());
        assert!(c.get(k(0)).is_some() && c.get(k(2)).is_some());
    }

    #[test]
    fn older_generation_is_fallback_until_replaced() {
        let mut c = TileCache::new(4);
        c.insert(k(0), 1, "old");
        assert!(c.has_current(k(0), 1));
        assert!(!c.has_current(k(0), 2));
        assert_eq!(c.generation_of(k(0)), Some(1));
        assert_eq!(c.generation_of(k(1)), None);
        assert_eq!(c.get(k(0)), Some((1, &"old")));
        c.insert(k(0), 2, "new");
        assert_eq!(c.get(k(0)), Some((2, &"new")));
        // A late result from an older generation does not win.
        c.insert(k(0), 1, "late");
        assert_eq!(c.get(k(0)), Some((2, &"new")));
        assert_eq!(c.len(), 1);
        c.clear();
        assert_eq!(c.len(), 0);
    }
}
