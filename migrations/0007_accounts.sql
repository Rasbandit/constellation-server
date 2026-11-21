-- Accounts (issue 126): who may host rooms, their logins, and which room each made.
--
-- users: name is the lowercase login name and the account's identity; hash is the argon2id
-- PHC string (salt and cost inside), never the password. Accounts are made by the admin only.
--
-- sessions: a login. token_hash is the SHA-256 of the random id in the browser's cookie, so a
-- copy of this table cannot be used to sign in. Past expires_at a session is dead (and swept).
--
-- rooms.owner: the provider-neutral subject of the host who made the room (NULL: made before
-- accounts or by the admin token; only the admin sees those). rooms.world_id: the generation
-- the web UI opens it by.
--
-- Additive only. Rolling back to a binary without this migration:
-- ALTER TABLE rooms DROP COLUMN owner, DROP COLUMN world_id; DROP TABLE sessions, users;
-- DELETE FROM _sqlx_migrations WHERE version = 7;
CREATE TABLE users (
    name       text PRIMARY KEY CHECK (name = lower(name) AND name <> ''),
    hash       text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE sessions (
    token_hash bytea PRIMARY KEY CHECK (length(token_hash) = 32),
    username   text NOT NULL REFERENCES users (name) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL
);

CREATE INDEX sessions_expires_idx ON sessions (expires_at);

ALTER TABLE rooms
    ADD COLUMN owner    text,
    ADD COLUMN world_id text;

CREATE INDEX rooms_owner_idx ON rooms (owner) WHERE owner IS NOT NULL;
