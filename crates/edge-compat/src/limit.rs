//! Per-session token buckets (doc 11 R4: a session cannot make the room do unbounded work).
#![allow(
    clippy::redundant_pub_crate,
    reason = "rustc's unreachable_pub (denied) needs crate visibility in this private module"
)]

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// Tokens in thousandths, so slow refill rates stay exact in integers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bucket {
    cap: u64,
    tokens: u64,
    refill_milli_per_sec: u64,
    last: Instant,
}

impl Bucket {
    /// Full at `now`, holding at most `burst` tokens.
    pub(crate) fn new(burst: u32, refill_milli_per_sec: u32, now: Instant) -> Self {
        let cap = u64::from(burst).saturating_mul(1000);
        Self { cap, tokens: cap, refill_milli_per_sec: u64::from(refill_milli_per_sec), last: now }
    }

    /// Take one token at `now` if there is one.
    pub(crate) fn take(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens >= 1000 {
            self.tokens = self.tokens.saturating_sub(1000);
            true
        } else {
            false
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed_ms =
            u64::try_from(now.saturating_duration_since(self.last).as_millis()).unwrap_or(u64::MAX);
        // milli-tokens = ms * (milli-tokens per s) / 1000
        let gained = elapsed_ms.saturating_mul(self.refill_milli_per_sec) / 1000;
        if gained > 0 {
            self.tokens = self.tokens.saturating_add(gained).min(self.cap);
            self.last = now;
        }
    }
}

/// Buckets by key (a slot, a client address), shared by every session of a room, so a
/// reconnect does not refill them.
#[derive(Debug)]
pub(crate) struct RateTable<K> {
    burst: u32,
    refill_milli_per_sec: u32,
    max_keys: usize,
    buckets: Mutex<HashMap<K, Bucket>>,
}

impl<K: Hash + Eq> RateTable<K> {
    pub(crate) fn new(burst: u32, refill_milli_per_sec: u32, max_keys: usize) -> Self {
        Self { burst, refill_milli_per_sec, max_keys, buckets: Mutex::new(HashMap::new()) }
    }

    /// One token for `key` at `now`. The lock is never held across an await.
    pub(crate) fn take(&self, key: K, now: Instant) -> bool {
        let mut map = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if !map.contains_key(&key) && map.len() >= self.max_keys {
            // A full bucket is the same as a new one, so forgetting it changes nothing.
            map.retain(|_, b| {
                b.refill(now);
                b.tokens < b.cap
            });
            if map.len() >= self.max_keys {
                return false;
            }
        }
        map.entry(key)
            .or_insert_with(|| Bucket::new(self.burst, self.refill_milli_per_sec, now))
            .take(now)
    }
}
