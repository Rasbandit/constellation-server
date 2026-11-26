-- Prepared worlds (the engine's ready form of an uploaded manifest: its compiled package and
-- the tables every seed of it shares) are kept as blobs in game_packages, by content hash. This
-- table finds one: the manifest it was made from and the engine layout it was made at, to the
-- blob's key. A world prepared at another layout has no row for this one, so the server
-- prepares it again from the stored manifest; so does a row whose blob is missing or torn.
--
-- Rows of older layouts and their blobs stay (nothing deletes from game_packages either).
--
-- Additive only. Dropping the table only makes the next generation of each world prepare it
-- again:
-- DROP TABLE prepared_worlds; DELETE FROM _sqlx_migrations WHERE version = 10;
CREATE TABLE prepared_worlds (
    manifest   bytea NOT NULL CHECK (length(manifest) = 32),
    layout     bigint NOT NULL,
    package    bytea NOT NULL CHECK (length(package) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (manifest, layout)
);
