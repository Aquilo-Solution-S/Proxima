-- v0.0.39: a series' publication position outlives its head (issue #476).
--
--   publication_counter    one row per Memory series that captured a
--                          listenable event: the last position it handed out.
--                          Capture increments it under the series lock and
--                          writes the new value into the event as `sequence`.
--                          It never decreases.
--
-- Forget deletes `memory_head` when the last hot row of a series cools, and
-- hydrate recreates it at 0. The position used to live on that row, so the
-- next write after a forget and hydrate reused positions consumers had
-- already seen. This table is not tied to the head: no FK, and forget does
-- not touch it. Owner erase deletes it with the series; transfer moves it
-- with the head.
--
-- memory_head.publication_sequence stays (dropping it is not additive). The
-- backfill copies it once; nothing reads or writes it afterwards.
--
-- Additive and idempotent: IF NOT EXISTS / ON CONFLICT throughout. The
-- forward-only trigger and the owner RLS live in definitions/ and are
-- installed by the definitions pass, which runs after this migration.

SELECT set_config('app.proxima_scope', 'platform', true);

CREATE TABLE IF NOT EXISTS proxima_core.publication_counter (
    handle uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    sequence bigint NOT NULL
        CONSTRAINT publication_counter_sequence_chk CHECK (sequence >= 0)
);

CREATE INDEX IF NOT EXISTS publication_counter_owner_idx
    ON proxima_core.publication_counter (owner_id, handle);

INSERT INTO proxima_core.publication_counter (handle, owner_id, sequence)
SELECT handle, owner_id, publication_sequence
  FROM proxima_core.memory_head
 WHERE publication_sequence > 0
ON CONFLICT (handle) DO UPDATE
   SET sequence = GREATEST(proxima_core.publication_counter.sequence, EXCLUDED.sequence);
