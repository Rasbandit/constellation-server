-- The game catalogue (issue 187, design in issue 173): games, their owners and their versions.
--
-- A game is found by its key, its name in lowercase (lowered by the server, never by SQL, so
-- both stores agree on every name); the name keeps the case it was made with. Keys sort
-- bytewise (COLLATE "C"), as the memory store does.
--
-- An owner is an account that may post versions of a game, in the pattern of room_admins
-- (migration 0008). Deleting an account or a game deletes its rows.
--
-- A version is one uploaded world file (world_uploads, kind 'apworld'), a version number
-- unique within its game, a note, and the generator version it was (or is being) checked
-- with. It starts as checking, then is live or failed; an owner may withdraw it, which is
-- final; a generator upgrade marks one that fails its re-check as needs_update. seq is upload
-- order: a new room uses the newest live version checked with the server's generator.
--
-- Additive only. Rolling back to a binary without this migration:
-- DROP TABLE catalogue_versions, catalogue_owners, catalogue_games;
-- DELETE FROM _sqlx_migrations WHERE version = 9;
CREATE TABLE catalogue_games (
    key         text COLLATE "C" PRIMARY KEY CHECK (key <> ''),
    name        text NOT NULL CHECK (name <> ''),
    description text NOT NULL,
    created_by  text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE catalogue_owners (
    game       text COLLATE "C" NOT NULL REFERENCES catalogue_games (key) ON DELETE CASCADE,
    username   text NOT NULL REFERENCES users (name) ON DELETE CASCADE,
    granted_by text NOT NULL,
    granted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (game, username)
);

CREATE INDEX catalogue_owners_username_idx ON catalogue_owners (username);

CREATE TABLE catalogue_versions (
    seq          bigserial PRIMARY KEY,
    game         text COLLATE "C" NOT NULL REFERENCES catalogue_games (key) ON DELETE CASCADE,
    version      text NOT NULL CHECK (version <> ''),
    note         text NOT NULL,
    apworld_kind text NOT NULL DEFAULT 'apworld' CHECK (apworld_kind = 'apworld'),
    apworld      bytea NOT NULL,
    generator    text NOT NULL,
    state        text NOT NULL
                 CHECK (state IN ('checking', 'live', 'failed', 'withdrawn', 'needs_update')),
    error        text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (game, version),
    FOREIGN KEY (apworld_kind, apworld) REFERENCES world_uploads (kind, id)
);

-- A new room's lookup: a game's newest live version for one generator.
CREATE INDEX catalogue_versions_live_idx ON catalogue_versions (game, generator, seq DESC)
    WHERE state = 'live';
