//! A cache of immutable values by content key, held in memory under a fixed number of bytes:
//! the least recently used go first. Holds the manifest and prepared world bytes the server
//! reads ([`crate::manifests`]); generic over what it holds, counted by [`Held`], so the loaded
//! worlds of a fill worker that stays up can use it too (see #200).

use log::BlobKey;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// What a cached value counts against the cap: the bytes it holds in memory.
pub(crate) trait Held {
    fn held_bytes(&self) -> usize;
}

impl Held for Vec<u8> {
    fn held_bytes(&self) -> usize {
        self.len()
    }
}

/// Values held in memory, at most `cap` bytes in all: the least recently used go first. An
/// entry larger than the cap is never kept.
#[derive(Debug)]
pub(crate) struct Cache<V> {
    cap: usize,
    pub(crate) used: usize,
    tick: u64,
    /// id to (value, last use).
    pub(crate) held: HashMap<BlobKey, (Arc<V>, u64)>,
    /// last use to id: the first entry is the least recently used.
    order: BTreeMap<u64, BlobKey>,
}

impl<V: Held> Cache<V> {
    pub(crate) fn new(cap: usize) -> Self {
        Self { cap, used: 0, tick: 0, held: HashMap::new(), order: BTreeMap::new() }
    }

    pub(crate) fn get(&mut self, id: &BlobKey) -> Option<Arc<V>> {
        self.tick += 1;
        let (value, used) = self.held.get_mut(id)?;
        self.order.remove(used);
        *used = self.tick;
        self.order.insert(self.tick, *id);
        Some(Arc::clone(value))
    }

    pub(crate) fn put(&mut self, id: BlobKey, value: Arc<V>) {
        let size = value.held_bytes();
        if size > self.cap || self.held.contains_key(&id) {
            return;
        }
        while self.used + size > self.cap {
            let Some((_, oldest)) = self.order.pop_first() else { break };
            if let Some((v, _)) = self.held.remove(&oldest) {
                self.used -= v.held_bytes();
            }
        }
        self.tick += 1;
        self.used += size;
        self.order.insert(self.tick, id);
        self.held.insert(id, (value, self.tick));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> BlobKey {
        [n; 32]
    }

    #[test]
    fn the_cache_stays_within_its_bytes_and_drops_the_least_recently_used() {
        let mut c = Cache::new(10);
        c.put(id(1), Arc::new(vec![0; 4]));
        c.put(id(2), Arc::new(vec![0; 4]));
        assert!(c.get(&id(1)).is_some(), "touched: now id 2 is the oldest");
        c.put(id(3), Arc::new(vec![0; 4]));
        assert!(c.get(&id(2)).is_none() && c.get(&id(1)).is_some() && c.get(&id(3)).is_some());
        assert_eq!(c.used, 8);
        c.put(id(4), Arc::new(vec![0; 11]));
        assert!(c.get(&id(4)).is_none(), "larger than the whole cache: never kept");
        assert_eq!(c.used, 8, "and it evicted nothing");
        c.put(id(5), Arc::new(vec![0; 10]));
        assert_eq!((c.held.len(), c.used), (1, 10), "one big entry pushes the rest out");
    }
}
