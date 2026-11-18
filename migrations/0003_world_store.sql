-- World data lives in the database, not in files (issue 68): game packages once per content
-- hash, and each room's seed (seed file v2, docs/seed-bundle-v2.md).
--
-- game_packages: key is the SHA-256 of the package's game section content; the server hashes
-- every package it loads and refuses one that does not match its key. Immutable: written once
-- (ON CONFLICT DO NOTHING), shared by every room of that game version.
--
-- room_seeds: the seed of a room, written once before the room is listed and never replaced
-- (a different bundle under the same seed name keeps the first seed and is refused by
-- rooms.bundle_hash). Rooms stored before this migration keep their v1 file in rooms.bundle.
-- No foreign key: the seed is written before the room row, so a crash between the two leaves
-- only an unlisted seed, which the next open of that room reuses.
--
-- Additive only. Rolling back: DROP TABLE room_seeds, game_packages;
-- DELETE FROM _sqlx_migrations WHERE version = 3;
CREATE TABLE game_packages (
    key        bytea PRIMARY KEY CHECK (length(key) = 32),
    bytes      bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE room_seeds (
    room_id    bigint PRIMARY KEY,
    seed       bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
