//! Many rooms in one process, each found by its room key.
//!
//! A room key is the room's public address (doc 01 D1.1, D-12): 10 characters of lowercase
//! base32 (`a` to `z`, `2` to `7`), so it is also a valid DNS label for the hostname form
//! `{room_key}.{host}` (doc 01 section 3.8.1). It is not a secret. A new room draws its key at
//! random (50 bits from the OS); with Postgres the key is stored with the room, so a restart
//! serves the room under the same key.
//!
//! A listed room is either in memory (resident) or not (cold); doc 02 section 4.3. Each
//! listing counts its open connections, so the host can tell how long a room has been idle.

use edge_compat::CompatRoom;
use proto::RoomId;
use runtime::RoomHandle;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

/// Characters in a room key.
pub const ROOM_KEY_LEN: usize = 10;

/// RFC 4648 base32 in lowercase: valid in a DNS label and never case-sensitive.
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// A room's public address: 10 lowercase base32 characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomKey(String);

impl RoomKey {
    /// A new random key: 50 bits from the operating system's random source.
    ///
    /// # Errors
    /// When the operating system has no random source.
    pub fn random() -> Result<Self, String> {
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).map_err(|e| format!("room key: no random source: {e}"))?;
        Ok(Self::from_bits(u64::from_be_bytes(bytes)))
    }

    /// The key for 64 bits: the top 50, 5 bits per character, most significant first.
    #[must_use]
    pub fn from_bits(bits: u64) -> Self {
        let bits = bits >> (64 - 5 * ROOM_KEY_LEN);
        let key = (0..ROOM_KEY_LEN)
            .rev()
            .map(|i| {
                let five = (bits >> (5 * i)) & 0x1f;
                // `five` is below 32, so the index is in range.
                char::from(ALPHABET[usize::try_from(five).unwrap_or(0)])
            })
            .collect();
        Self(key)
    }

    /// Read a key typed by a person or sent by a client: case is ignored.
    ///
    /// # Errors
    /// When the text is not 10 base32 characters.
    pub fn parse(text: &str) -> Result<Self, String> {
        let key = text.to_ascii_lowercase();
        if key.len() != ROOM_KEY_LEN || !key.bytes().all(|b| ALPHABET.contains(&b)) {
            return Err(format!(
                "{text:?} is not a room key ({ROOM_KEY_LEN} characters, a to z and 2 to 7)"
            ));
        }
        Ok(Self(key))
    }

    /// The key as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for RoomKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A room in memory: what its sessions share and its task's liveness.
#[derive(Debug, Clone)]
pub struct Room {
    /// The seed name it was made from.
    pub seed: String,
    /// Shared by every session of the room.
    pub compat: Arc<CompatRoom>,
    /// The room task's liveness.
    pub live: RoomHandle,
}

/// A room the server lists, in memory or not.
#[derive(Debug, Clone)]
pub struct Listing {
    /// The room's id.
    pub id: RoomId,
    /// The seed name it was made from.
    pub seed: String,
    /// The room while it is in memory; `None` while it is cold.
    pub room: Option<Room>,
    /// Its connections, open and past.
    pub activity: Arc<Activity>,
}

/// A moment on this process's monotonic clock, in milliseconds since its first use. Earlier
/// moments are negative, so a room idle for longer than the machine has been up keeps its
/// idle time (an [`Instant`] cannot go back past boot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Moment(i64);

impl Moment {
    /// Now.
    #[must_use]
    pub fn now() -> Self {
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        Self(millis(ORIGIN.get_or_init(Instant::now).elapsed()))
    }

    /// The moment `by` before this one.
    #[must_use]
    pub fn minus(self, by: Duration) -> Self {
        Self(self.0.saturating_sub(millis(by)))
    }

    /// How long after `earlier` this is; zero when it is not after it.
    #[must_use]
    pub fn since(self, earlier: Self) -> Duration {
        Duration::from_millis(u64::try_from(self.0.saturating_sub(earlier.0)).unwrap_or(0))
    }
}

fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// How many connections a room has open, and since when it has had none.
#[derive(Debug)]
pub struct Activity {
    open: AtomicUsize,
    last: Mutex<Moment>,
}

impl Activity {
    /// A room whose last connection was `idle` ago.
    #[must_use]
    pub fn new(idle: Duration) -> Arc<Self> {
        Arc::new(Self { open: AtomicUsize::new(0), last: Mutex::new(Moment::now().minus(idle)) })
    }

    /// A connection begins; it counts until the returned [`Visit`] is dropped.
    #[must_use]
    pub fn enter(self: &Arc<Self>) -> Visit {
        self.open.fetch_add(1, Ordering::SeqCst);
        self.touch();
        Visit(Arc::clone(self))
    }

    /// Restart the idle clock: a connection is about to enter (it woke the room).
    pub fn touch(&self) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Moment::now();
    }

    /// How long the room has had no connection; `None` while one is open.
    #[must_use]
    pub fn idle_for(&self) -> Option<Duration> {
        if self.open.load(Ordering::SeqCst) > 0 {
            return None;
        }
        Some(Moment::now().since(*self.last.lock().unwrap_or_else(PoisonError::into_inner)))
    }

    /// When the room was last used: `now` while a connection is open.
    #[must_use]
    pub fn last_used(&self, now: Moment) -> Moment {
        if self.open.load(Ordering::SeqCst) > 0 {
            return now;
        }
        *self.last.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Move the idle clock back by `by`, as if that much time had passed (tests only, so no
    /// test sleeps; docs/TESTING.md rule 5).
    #[cfg(test)]
    pub fn age(&self, by: Duration) {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        *last = last.minus(by);
    }
}

/// One open connection to a room ([`Activity::enter`]).
#[derive(Debug)]
pub struct Visit(Arc<Activity>);

impl Drop for Visit {
    fn drop(&mut self) {
        // The time first, so a reader that sees no open connection never sees an old time.
        self.0.touch();
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Read the room list. Each change to it is one call, so a writer that panicked left it
/// whole and the poison is ignored.
pub(crate) fn read(rooms: &RwLock<Rooms>) -> RwLockReadGuard<'_, Rooms> {
    rooms.read().unwrap_or_else(PoisonError::into_inner)
}

/// Change the room list (see [`read`]).
pub(crate) fn write(rooms: &RwLock<Rooms>) -> RwLockWriteGuard<'_, Rooms> {
    rooms.write().unwrap_or_else(PoisonError::into_inner)
}

/// Every room this process lists, by key.
#[derive(Debug, Clone, Default)]
pub struct Rooms {
    by_key: BTreeMap<RoomKey, Listing>,
}

impl Rooms {
    /// List a room under its key.
    ///
    /// # Errors
    /// When another room already has the key (a collision of 50-bit prefixes).
    pub fn insert(&mut self, key: RoomKey, listing: Listing) -> Result<(), String> {
        if let Some(old) = self.by_key.get(&key) {
            return Err(format!(
                "room key {key} is taken by seed {:?}; seed {:?} cannot use it",
                old.seed, listing.seed
            ));
        }
        self.by_key.insert(key, listing);
        Ok(())
    }

    /// Stop listing the room with this key.
    pub fn remove(&mut self, key: &RoomKey) -> Option<Listing> {
        self.by_key.remove(key)
    }

    /// The room with this key.
    #[must_use]
    pub fn get(&self, key: &RoomKey) -> Option<&Listing> {
        self.by_key.get(key)
    }

    /// The room with this key, to change.
    pub fn get_mut(&mut self, key: &RoomKey) -> Option<&mut Listing> {
        self.by_key.get_mut(key)
    }

    /// The key of the room with this id.
    #[must_use]
    pub fn key_of(&self, id: RoomId) -> Option<&RoomKey> {
        self.by_key.iter().find(|(_, l)| l.id == id).map(|(k, _)| k)
    }

    /// The only room and its key, when there is exactly one.
    #[must_use]
    pub fn only_with_key(&self) -> Option<(&RoomKey, &Listing)> {
        let mut all = self.by_key.iter();
        match (all.next(), all.next()) {
            (Some(room), None) => Some(room),
            _ => None,
        }
    }

    /// How many rooms.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// No rooms at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// Every room with its key, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&RoomKey, &Listing)> {
        self.by_key.iter()
    }

    /// True while every room in memory has its task running.
    #[must_use]
    pub fn all_running(&self) -> bool {
        self.by_key.values().filter_map(|l| l.room.as_ref()).all(|r| r.live.is_running())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_ten_lowercase_base32_characters_from_the_top_50_bits() {
        assert_eq!(RoomKey::from_bits(0).as_str(), "aaaaaaaaaa");
        assert_eq!(RoomKey::from_bits(u64::MAX).as_str(), "7777777777");
        // Only the top 50 bits count: the low 14 bits never change the key.
        assert_eq!(RoomKey::from_bits(0x3fff).as_str(), "aaaaaaaaaa");
        // The lowest of the 50 bits is the last character.
        assert_eq!(RoomKey::from_bits(1 << 14).as_str(), "aaaaaaaaab");
        // The highest 5 bits are the first character.
        assert_eq!(RoomKey::from_bits(0b11111 << 59).as_str(), "7aaaaaaaaa");
    }

    #[test]
    fn a_random_key_parses_back_and_case_is_ignored() {
        let key = RoomKey::random().unwrap();
        assert_eq!(RoomKey::parse(key.as_str()).unwrap(), key);
        assert_eq!(RoomKey::parse(&key.as_str().to_ascii_uppercase()).unwrap(), key);
    }

    #[test]
    fn random_keys_differ() {
        let keys: std::collections::BTreeSet<_> =
            (0..100).map(|_| RoomKey::random().unwrap()).collect();
        assert_eq!(keys.len(), 100, "50 random bits: 100 draws never repeat in practice");
    }

    #[test]
    fn a_room_is_idle_only_with_no_connection_open() {
        let a = Activity::new(Duration::from_secs(60));
        assert!(a.idle_for().unwrap() >= Duration::from_secs(60));
        let one = a.enter();
        let two = a.enter();
        assert_eq!(a.idle_for(), None);
        drop(one);
        assert_eq!(a.idle_for(), None, "one is still open");
        drop(two);
        assert!(a.idle_for().unwrap() < Duration::from_secs(60), "the clock restarts on close");
    }

    #[test]
    fn a_room_with_a_connection_open_was_last_used_at_the_moment_asked() {
        let a = Activity::new(Duration::from_secs(3_600));
        let asked = Moment::now().minus(Duration::from_secs(60));
        let last = a.last_used(asked);
        assert!(asked.since(last) >= Duration::from_secs(3_540), "closed: its last connection");
        let open = a.enter();
        assert_eq!(a.last_used(asked), asked, "open: in use at that moment");
        drop(open);
        assert!(a.last_used(asked) > asked, "closed again: the close just now");
    }

    #[test]
    fn a_listed_room_is_found_by_its_id_and_nothing_else_is() {
        let mut rooms = Rooms::default();
        assert!(rooms.is_empty());
        let (key, id) = (RoomKey::from_bits(1 << 14), crate::room_id("alpha"));
        let activity = Activity::new(Duration::ZERO);
        rooms
            .insert(key.clone(), Listing { id, seed: "alpha".into(), room: None, activity })
            .unwrap();
        assert!(!rooms.is_empty());
        assert_eq!(rooms.key_of(id), Some(&key));
        assert_eq!(rooms.key_of(crate::room_id("beta")), None);
    }

    /// A room idle for longer than the machine has been up (a restart after a reboot) keeps
    /// its idle time; an `Instant` would have clamped it to zero.
    #[test]
    fn an_idle_time_longer_than_the_uptime_is_kept() {
        let years = Duration::from_secs(100 * 365 * 86_400);
        let idle = Activity::new(years).idle_for().unwrap();
        assert!(idle >= years && idle < years + Duration::from_secs(60), "{idle:?}");
        let now = Moment::now();
        assert_eq!(now.since(now.minus(years)), Duration::from_secs(100 * 365 * 86_400));
        assert_eq!(now.minus(years).since(now), Duration::ZERO, "never negative");
    }

    #[test]
    fn text_that_is_not_a_key_is_refused() {
        for bad in
            ["", "aaaaaaaaa", "aaaaaaaaaaa", "aaaaaaaaa1", "aaaaaaaaa8", "aaaa-aaaaa", "ääääääää"]
        {
            assert!(RoomKey::parse(bad).is_err(), "{bad:?}");
        }
    }
}
