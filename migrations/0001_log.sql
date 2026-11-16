-- The room log (doc 02 section 5.5, D-98), cut to what the log store and the compatible server
-- use today. Applied by `sqlx::migrate!` when the server starts. Deviations from the sketch:
-- no foreign keys (the writer's hot path does not pay for them), event bodies are the
-- serialized `proto::Event` (no encryption yet), only the latest snapshot is kept, and applied
-- cursors live in their own table until the `slots` table exists.

CREATE TABLE rooms (
    room_id     bigint PRIMARY KEY,
    seed_name   text        NOT NULL,
    -- SHA-256 of the seed bundle the room was made from; a restart with another bundle is refused.
    bundle_hash bytea       NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- The fence (doc 02 D5). Every write locks this row FOR SHARE and checks the epoch.
CREATE TABLE room_owner (
    room_id     bigint PRIMARY KEY,
    epoch       bigint NOT NULL CHECK (epoch >= 0),
    -- Written by the directory once there is more than one node (Stage 0 and 1 run one process).
    node_id     text,
    lease_until timestamptz
);

CREATE TABLE events (
    room_id bigint NOT NULL,
    seq     bigint NOT NULL CHECK (seq > 0),
    body    bytea  NOT NULL,
    PRIMARY KEY (room_id, seq)
);

CREATE TABLE slot_cursors (
    room_id bigint NOT NULL,
    slot_no bigint NOT NULL CHECK (slot_no >= 0),
    applied bigint NOT NULL CHECK (applied >= 0),
    PRIMARY KEY (room_id, slot_no)
);

CREATE TABLE snapshots (
    room_id  bigint PRIMARY KEY,
    upto_seq bigint NOT NULL CHECK (upto_seq >= 0),
    body     bytea  NOT NULL
);
