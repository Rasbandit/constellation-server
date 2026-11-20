-- Uploaded legacy world files (.apworld) and player files (.yaml) live in the database, once per
-- content hash (issue 119), as manifests do: every server instance sees them, and nothing is
-- kept in the data folder. A generation writes the files it chose to its own job folder, which
-- it removes when it ends.
--
-- id is the SHA-256 of the whole file (its bytes column); the server checks it on every put and
-- every read. kind keeps the two upload kinds apart, as the folders did. meta is the record the
-- upload API answers with (JSON text the server made after its sandboxed inspect; text, not
-- jsonb, so it is kept exactly as written), game the game folder name it used to go under. A row
-- whose bytes do not hash to its id (torn) is replaced by the next put of that file.
--
-- Additive only. Dropping the table loses the uploads:
-- DROP TABLE world_uploads; DELETE FROM _sqlx_migrations WHERE version = 5;
CREATE TABLE world_uploads (
    kind       text NOT NULL CHECK (kind IN ('apworld', 'yaml')),
    id         bytea NOT NULL CHECK (length(id) = 32),
    game       text NOT NULL,
    size       bigint NOT NULL,
    meta       text NOT NULL,
    bytes      bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (kind, id)
);
