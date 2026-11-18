-- The room directory: everything the server needs to list a room and to load it on its first
-- connect (doc 02 section 4.3: Cold, then Hydrating), so rooms come from the database and a
-- restart serves every room that has not finished.
--
-- room_key: the room's public address (doc 01 D1.1, D-12), 10 characters of lowercase base32
-- drawn at random by the server the first time the room opens, then kept so a restart serves
-- the room under the same key. Unique: a key names one room.
--
-- bundle: the seed bundle file the room was made from, as read (its hash is bundle_hash).
--
-- last_active: when the room last had a connection; a room left alone for 30 days ends as
-- abandoned (D-30, design value).
--
-- finished_at, finished_reason: set once when the room ends (doc 02 section 4.3, Finished;
-- only 'abandoned' so far). A finished room is never served again; its rows are kept.
--
-- The new columns are nullable for rooms stored before this migration; the next open fills
-- room_key and bundle.
ALTER TABLE rooms
    ADD COLUMN room_key        text UNIQUE,
    ADD COLUMN bundle          bytea,
    ADD COLUMN last_active     timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN finished_at     timestamptz,
    ADD COLUMN finished_reason text;
