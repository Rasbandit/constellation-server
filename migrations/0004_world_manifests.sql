-- Uploaded data-only world manifests live in the database, once per content hash (the same
-- rule as game_packages: world data is not kept in files, so every server instance sees it).
--
-- id is the SHA-256 of the whole manifest (its bytes column); the server checks it on every put
-- and every read. The listing columns (name, game, version, size, items, locations) are filled
-- at upload so a listing never reads or parses a manifest. A row whose bytes do not hash to its
-- id (torn) is replaced by the next put of that manifest.
--
-- Additive only. Dropping the table loses the manifests:
-- DROP TABLE world_manifests; DELETE FROM _sqlx_migrations WHERE version = 4;
CREATE TABLE world_manifests (
    id         bytea PRIMARY KEY CHECK (length(id) = 32),
    name       text NOT NULL,
    game       text NOT NULL,
    version    text NOT NULL,
    size       bigint NOT NULL,
    items      integer NOT NULL,
    locations  integer NOT NULL,
    bytes      bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Keyset pages of one game's manifests (the primary key pages all of them).
CREATE INDEX world_manifests_game_idx ON world_manifests (game, id);
