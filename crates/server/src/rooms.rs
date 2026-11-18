//! Many rooms in one process, each found by its room key.
//!
//! A room key is the room's public address (doc 01 D1.1, D-12): 10 characters of lowercase
//! base32 (`a` to `z`, `2` to `7`), so it is also a valid DNS label for the hostname form
//! `{room_key}.{host}` (doc 01 section 3.8.1). It is not a secret.
//!
//! Deviation from D-12: the key is derived from the room id (the top 50 bits of the seed
//! name's SHA-256, see [`crate::room_id`]) instead of drawn at random, so a restart finds the
//! same key without a stored mapping. A random key needs a key table in the log store first.

use edge_compat::CompatRoom;
use proto::RoomId;
use runtime::RoomHandle;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Characters in a room key.
pub const ROOM_KEY_LEN: usize = 10;

/// RFC 4648 base32 in lowercase: valid in a DNS label and never case-sensitive.
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// A room's public address: 10 lowercase base32 characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomKey(String);

impl RoomKey {
    /// The key for a room id: its top 50 bits, 5 bits per character, most significant first.
    #[must_use]
    pub fn for_room(room: RoomId) -> Self {
        let bits = room.0 >> (64 - 5 * ROOM_KEY_LEN);
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

/// One running room: what its sessions share and its task's liveness.
#[derive(Debug, Clone)]
pub struct Room {
    /// The seed name it was made from.
    pub seed: String,
    /// Shared by every session of the room.
    pub compat: Arc<CompatRoom>,
    /// The room task's liveness.
    pub live: RoomHandle,
}

/// Every room this process runs, by key.
#[derive(Debug, Clone, Default)]
pub struct Rooms {
    by_key: BTreeMap<RoomKey, Room>,
}

impl Rooms {
    /// Add a room under its key.
    ///
    /// # Errors
    /// When another room already has the key (two bundles with the same seed name, or a
    /// collision of 50-bit prefixes).
    pub fn insert(&mut self, key: RoomKey, room: Room) -> Result<(), String> {
        if let Some(old) = self.by_key.get(&key) {
            return Err(format!(
                "room key {key} is taken by seed {:?}; seed {:?} cannot use it",
                old.seed, room.seed
            ));
        }
        self.by_key.insert(key, room);
        Ok(())
    }

    /// The room with this key.
    #[must_use]
    pub fn get(&self, key: &RoomKey) -> Option<&Room> {
        self.by_key.get(key)
    }

    /// The only room, when there is exactly one.
    #[must_use]
    pub fn only(&self) -> Option<&Room> {
        let mut all = self.by_key.values();
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
    pub fn iter(&self) -> impl Iterator<Item = (&RoomKey, &Room)> {
        self.by_key.iter()
    }

    /// True while every room's task runs.
    #[must_use]
    pub fn all_running(&self) -> bool {
        self.by_key.values().all(|r| r.live.is_running())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_ten_lowercase_base32_characters_from_the_top_50_bits() {
        assert_eq!(RoomKey::for_room(RoomId(0)).as_str(), "aaaaaaaaaa");
        assert_eq!(RoomKey::for_room(RoomId(u64::MAX)).as_str(), "7777777777");
        // Only the top 50 bits count: the low 14 bits never change the key.
        assert_eq!(RoomKey::for_room(RoomId(0x3fff)).as_str(), "aaaaaaaaaa");
        // The lowest of the 50 bits is the last character.
        assert_eq!(RoomKey::for_room(RoomId(1 << 14)).as_str(), "aaaaaaaaab");
        // The highest 5 bits are the first character.
        assert_eq!(RoomKey::for_room(RoomId(0b11111 << 59)).as_str(), "7aaaaaaaaa");
    }

    #[test]
    fn a_derived_key_parses_back_and_case_is_ignored() {
        let key = RoomKey::for_room(crate::room_id("abc"));
        assert_eq!(RoomKey::parse(key.as_str()).unwrap(), key);
        assert_eq!(RoomKey::parse(&key.as_str().to_ascii_uppercase()).unwrap(), key);
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
