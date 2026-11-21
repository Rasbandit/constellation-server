//! Per-session token buckets (doc 11 R4: a session cannot make the room do unbounded work).
#![allow(
    clippy::redundant_pub_crate,
    reason = "rustc's unreachable_pub (denied) needs crate visibility in this private module"
)]

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// The shortest time between two sweeps of a full [`RateTable`].
const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// Tokens in thousandths, so slow refill rates stay exact in integers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bucket {
    cap: u64,
    tokens: u64,
    refill_milli_per_sec: u64,
    last: Instant,
    /// Milli-tokens charged beyond the tokens there were; refill repays it first.
    debt: u64,
}

impl Bucket {
    /// Full at `now`, holding at most `burst` tokens.
    pub(crate) fn new(burst: u32, refill_milli_per_sec: u32, now: Instant) -> Self {
        let cap = u64::from(burst).saturating_mul(1000);
        let refill_milli_per_sec = u64::from(refill_milli_per_sec);
        Self { cap, tokens: cap, refill_milli_per_sec, last: now, debt: 0 }
    }

    /// Whether a charge may start at `now`: no debt and at least one token.
    pub(crate) fn ready(&mut self, now: Instant) -> bool {
        self.refill(now);
        self.debt == 0 && self.tokens >= 1000
    }

    /// Spend `cost` tokens, running into debt for what is missing (the cost of a write is
    /// known only once it is made). A bucket in debt is not [`Bucket::ready`] until refill
    /// repays it, so the long-run rate stays the refill rate.
    pub(crate) fn charge(&mut self, cost: u64, now: Instant) {
        self.refill(now);
        let milli = cost.saturating_mul(1000);
        self.debt = self.debt.saturating_add(milli.saturating_sub(self.tokens));
        self.tokens = self.tokens.saturating_sub(milli);
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
            let repaid = gained.min(self.debt);
            self.debt = self.debt.saturating_sub(repaid);
            self.tokens = self.tokens.saturating_add(gained.saturating_sub(repaid)).min(self.cap);
            self.last = now;
        }
    }
}

/// Buckets by key (a slot, a client address), shared by every session of a room, so a
/// reconnect does not refill them.
#[derive(Debug)]
pub struct RateTable<K> {
    burst: u32,
    refill_milli_per_sec: u32,
    max_keys: usize,
    /// The buckets, and when a full table was last swept (a sweep walks every key, so a flood of
    /// new keys must not trigger one per request).
    buckets: Mutex<(HashMap<K, Bucket>, Option<Instant>)>,
}

impl<K: Hash + Eq> RateTable<K> {
    /// A table of buckets that start full at `burst` tokens and refill at `refill_milli_per_sec`.
    #[must_use]
    pub fn new(burst: u32, refill_milli_per_sec: u32, max_keys: usize) -> Self {
        Self { burst, refill_milli_per_sec, max_keys, buckets: Mutex::new((HashMap::new(), None)) }
    }

    /// One token for `key` at `now`. The lock is never held across an await.
    pub fn take(&self, key: K, now: Instant) -> bool {
        self.with(key, now, |b| b.take(now))
    }

    /// Whether `key` may start a charge at `now` ([`Bucket::ready`]).
    pub fn ready(&self, key: K, now: Instant) -> bool {
        self.with(key, now, |b| b.ready(now))
    }

    /// Charge `cost` tokens to `key` at `now` ([`Bucket::charge`]); a key the full table
    /// cannot track is not charged (it was refused by `ready` first).
    pub fn charge(&self, key: K, cost: u64, now: Instant) {
        self.with(key, now, |b| {
            b.charge(cost, now);
            true
        });
    }

    /// Run `f` on `key`'s bucket, made full when new; `false` when the table is full of keys
    /// that are all still limited.
    fn with(&self, key: K, now: Instant, f: impl FnOnce(&mut Bucket) -> bool) -> bool {
        let mut guard = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        let (map, swept) = &mut *guard;
        if !map.contains_key(&key) && map.len() >= self.max_keys {
            // At most one sweep a second: a full table of limited keys is not walked again for
            // every new key. A full bucket is the same as a new one, so forgetting it changes
            // nothing; a bucket in debt has no tokens, so it is never full and is kept.
            if swept.is_none_or(|t| now.saturating_duration_since(t) >= SWEEP_EVERY) {
                *swept = Some(now);
                map.retain(|_, b| {
                    b.refill(now);
                    b.tokens < b.cap
                });
            }
            if map.len() >= self.max_keys {
                return false;
            }
        }
        f(map.entry(key).or_insert_with(|| Bucket::new(self.burst, self.refill_milli_per_sec, now)))
    }
}
