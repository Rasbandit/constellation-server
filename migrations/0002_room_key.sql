-- The room key (doc 01 D1.1, D-12): a room's public address, 10 characters of lowercase base32
-- drawn at random by the server the first time the room opens, then kept so a restart serves
-- the room under the same key. Unique: a key names one room. Nullable only for rooms stored
-- before this migration; the next open fills it.
ALTER TABLE rooms ADD COLUMN room_key text UNIQUE;
