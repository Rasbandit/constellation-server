//! A cache of immutable values by content key, held in memory under a fixed number of bytes:
//! the least recently used go first. One implementation for the manifest and prepared world
//! bytes the server reads ([`crate::manifests`]) and for the prepared worlds a fill worker has
//! loaded ([`crate::sandbox`]).

use data_fill::prepared::Prepared;
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

/// A loaded world counts every byte it holds (`Prepared::held_bytes`), not its file's size.
impl Held for Prepared {
    fn held_bytes(&self) -> usize {
        Self::held_bytes(self)
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
    use data_fill::job;
    use data_fill::synth::{Shape, world};
    use serde_json::json;

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

    /// Synthetic world `n` prepared: its file, and a fill request for one player on it.
    fn prepared(n: u64) -> (Vec<u8>, Vec<u8>) {
        let (mut manifest, inst) = world(&Shape { checks: 60, ..Shape::default() }, 1, n);
        manifest["game"] = json!(format!("Synth {n}"));
        manifest["version"] = json!("1");
        let out = job::prepare(manifest.to_string().as_bytes()).unwrap();
        let file = job::unframe(&out).unwrap().1.to_vec();
        let player = json!({"name": "P", "instance": {
            "locations": inst.locations, "pool": inst.pool, "pool_class": inst.pool_class,
        }});
        let head = json!({"seed_name": "s", "players": [player], "rng_seed": 1, "workers": 1});
        let request = job::frame(head.to_string().as_bytes(), &file);
        (file, request)
    }

    fn loaded(n: u64) -> (BlobKey, Arc<Prepared>) {
        let file = prepared(n).0;
        (log::blob_key(&file), Arc::new(Prepared::open(&file).unwrap()))
    }

    /// Loaded worlds count the bytes they hold; a hit is the very world that was put; the
    /// least recently used world leaves first; and the bytes held never pass the cap.
    #[test]
    fn loaded_worlds_leave_least_recently_used_first_and_never_pass_the_cap() {
        let worlds: Vec<_> = (0..6).map(loaded).collect();
        let size = |k: usize| worlds[k].1.held_bytes();
        assert!(size(0) > 10_000, "a loaded world holds far more than a pointer: {}", size(0));
        let cap = size(0) + size(1) + size(2);
        let mut c = Cache::new(cap);
        for (key, world) in &worlds[..3] {
            c.put(*key, Arc::clone(world));
        }
        assert_eq!((c.held.len(), c.used), (3, cap), "three fit exactly");
        let hit = c.get(&worlds[0].0).unwrap();
        assert!(Arc::ptr_eq(&hit, &worlds[0].1), "a hit is the same loaded world, not a copy");
        // World 0 was just used, so world 1 is the least recently used: it makes room first.
        let held = |c: &mut Cache<Prepared>| -> Vec<bool> {
            worlds.iter().map(|(k, _)| c.held.contains_key(k)).collect()
        };
        let mut most = 0;
        for (key, world) in &worlds[3..] {
            c.put(*key, Arc::clone(world));
            let counted: usize = c.held.values().map(|(w, _)| w.held_bytes()).sum();
            assert_eq!(c.used, counted, "the count is the bytes of what is held");
            assert!(c.used <= cap, "{} over the cap {cap}", c.used);
            most = most.max(c.used);
            if *key == worlds[3].0 {
                let now = held(&mut c);
                assert!(now[0] && !now[1] && now[3], "world 1 left before world 0: {now:?}");
            }
        }
        assert!(most > 0 && !c.held.contains_key(&worlds[0].0), "in the end world 0 left too");
        let mut small = Cache::new(size(0) - 1);
        small.put(worlds[0].0, Arc::clone(&worlds[0].1));
        assert_eq!(
            (small.held.len(), small.used),
            (0, 0),
            "a world over the whole cap is not kept"
        );
    }

    /// A fill worker loads its world through the cache: the first fill of a world loads it,
    /// the next finds it loaded, and both print the seed a fill without a cache prints.
    #[test]
    fn a_fill_worker_loads_a_world_once_and_fills_the_same_seed_from_the_cache() {
        let (file, request) = prepared(0);
        let mut worlds = Cache::new(config::PREPARED_CACHE_BYTES);
        let first = crate::sandbox::job_on(&mut worlds, "fill", &request).unwrap();
        let key = log::blob_key(&file);
        let world = Arc::clone(&worlds.held[&key].0);
        assert_eq!(worlds.used, world.held_bytes());
        let again = crate::sandbox::job_on(&mut worlds, "fill", &request).unwrap();
        assert!(Arc::ptr_eq(&worlds.held[&key].0, &world), "found loaded, not loaded again");
        assert_eq!(worlds.held.len(), 1);
        assert_eq!((&first, &again), (&job::fill(&request).unwrap(), &first));
        // The other jobs go through unchanged, and a bad world is refused, not cached.
        assert!(crate::sandbox::job_on(&mut worlds, "prepare", b"{").is_err());
        let cut = &request[..request.len() - 9];
        assert!(crate::sandbox::job_on(&mut worlds, "fill", cut).is_err());
        assert_eq!(worlds.held.len(), 1);
    }
}
