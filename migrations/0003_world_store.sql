-- World data lives in the database, not in files (issue 68): game packages once per content
-- hash, and each room's seed (seed file v2, docs/seed-bundle-v2.md).
--
-- game_packages: key is the SHA-256 of the whole package file (its bytes column); the server
-- checks it on every put and every read. Written once and shared by every room of that game
-- version; a row whose bytes do not hash to its key (torn) is replaced by the next put.
--
-- room_seeds: the seed of a room, never replaced. A hosted room's seed is written with its
-- rooms row, in one transaction and only when rooms.bundle_hash is the seed's bundle; a seed
-- with no rooms row is a world generated but not hosted. Every load and export checks the
-- seed's bundle hash against the room's, so a room never plays another bundle's seed.
-- Rooms stored before this migration keep their v1 file in rooms.bundle. No foreign key: a
-- generated world's seed has no room.
--
-- Additive only. Rolling back the binary hides every room stored after this migration (an
-- older server lists only rooms with rooms.bundle). Dropping the tables loses them:
-- DROP TABLE room_seeds, game_packages; DELETE FROM _sqlx_migrations WHERE version = 3;
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
