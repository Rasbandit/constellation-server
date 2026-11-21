-- Each world generation's status lives in the database (issue 119), not in a status.json file
-- under the data folder, so every server instance can answer for it.
--
-- id is the world id (64 lowercase hex). status is the JSON the status API answers with (text,
-- kept exactly as written), state its status field, kept apart so the sweep below never parses.
-- A queued or running job's process marks it alive by setting updated_at every
-- gen_heartbeat_secs. One not marked for gen_stale_secs reads as stalled (worked out on read,
-- never stored, as it may come back); one not marked for gen_lost_secs is failed by any
-- instance's sweep, as its process is gone. The partial index keeps that sweep to the few
-- unfinished jobs.
--
-- Additive only. Dropping the table loses the statuses:
-- DROP TABLE world_jobs; DROP FUNCTION world_jobs_status_object(text); DELETE FROM _sqlx_migrations WHERE version = 6;
CREATE TABLE world_jobs (
    id         text PRIMARY KEY CHECK (id ~ '^[0-9a-f]{64}$'),
    state      text NOT NULL CHECK (state IN ('queued', 'running', 'done', 'failed')),
    status     text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX world_jobs_unfinished_idx ON world_jobs (updated_at)
    WHERE state IN ('queued', 'running');

-- A status as a JSON object, or {} for text that is not one (not JSON, or holding a \u0000
-- escape, which jsonb refuses): the sweep that fails lost jobs must never abort on one bad row.
CREATE FUNCTION world_jobs_status_object(t text) RETURNS jsonb
LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN
    IF jsonb_typeof(t::jsonb) = 'object' THEN
        RETURN t::jsonb;
    END IF;
    RETURN '{}'::jsonb;
EXCEPTION WHEN OTHERS THEN
    RETURN '{}'::jsonb;
END
$$;
