-- Room admins (issue 129): people besides the owner who may manage one room.
--
-- A room's owner is rooms.owner (never listed here, never removable). A row here makes the
-- account an admin of that room only: it may add other accounts as admins and remove itself;
-- the owner may remove any admin. granted_by is the account that added it (NULL: the admin
-- token). Deleting an account or a room deletes its rows.
--
-- Additive only. Rolling back to a binary without this migration:
-- DROP TABLE room_admins;
-- DELETE FROM _sqlx_migrations WHERE version = 8;
CREATE TABLE room_admins (
    room_id    bigint NOT NULL REFERENCES rooms (room_id) ON DELETE CASCADE,
    username   text NOT NULL REFERENCES users (name) ON DELETE CASCADE,
    granted_by text,
    granted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, username)
);

CREATE INDEX room_admins_username_idx ON room_admins (username);
