-- v0.0.33: a Fact keeps when it occurred (issue #466).
--
-- A Fact's write time is its `t` (UUIDv7) and orders supersession, recency
-- and cursors. When the observation itself happened is a second, independent
-- time: a writer may backdate it (`FactWriteCommand.occurred_at`, the
-- `observed_at` argument of `core_remember`). Every Fact now stores it; storage
-- fills the write time of `t` when the writer gives none, so the column is
-- never NULL on a Fact. Abstractions and Perspectives are conclusions, not
-- observations, and carry none.
ALTER TABLE proxima_core.memory ADD COLUMN IF NOT EXISTS occurred_at timestamptz;

-- Backfill. A Fact written before this migration had no occurrence time other
-- than its write time, so that is what it occurred at; the epoch fallback is
-- the value every read gives a `t` that carries no timestamp.
--
-- `memory` is append-only, so the one statement allowed to fill the new
-- column runs with that guard off, as in 0004. sqlx runs a migration in one
-- transaction and DISABLE TRIGGER takes ACCESS EXCLUSIVE, so no concurrent
-- write sees the gap.
ALTER TABLE proxima_core.memory DISABLE TRIGGER memory_append_only;

UPDATE proxima_core.memory
   SET occurred_at = COALESCE(uuid_extract_timestamp(t), TIMESTAMPTZ '1970-01-01 00:00:00+00')
 WHERE kind = 'fact' AND occurred_at IS NULL;

ALTER TABLE proxima_core.memory ENABLE TRIGGER memory_append_only;

-- Every Fact carries the time, nothing else does.
ALTER TABLE proxima_core.memory DROP CONSTRAINT IF EXISTS memory_fact_occurred_at_chk;
ALTER TABLE proxima_core.memory
    ADD CONSTRAINT memory_fact_occurred_at_chk
    CHECK ((kind = 'fact') = (occurred_at IS NOT NULL));

-- `occurred_at` joins the append-only set: an observation's time is part of
-- the observation, and a later correction is a new Fact, not an UPDATE.
CREATE OR REPLACE FUNCTION proxima_core.memory_owner_or_append_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.t IS DISTINCT FROM OLD.t
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
       OR NEW.source_id IS DISTINCT FROM OLD.source_id
       OR NEW.ingest_key IS DISTINCT FROM OLD.ingest_key
       OR NEW.origins IS DISTINCT FROM OLD.origins
       OR NEW.refs IS DISTINCT FROM OLD.refs
       OR NEW.goal_refs IS DISTINCT FROM OLD.goal_refs
       OR NEW.occurred_at IS DISTINCT FROM OLD.occurred_at
       OR NEW.sidecar_tables IS DISTINCT FROM OLD.sidecar_tables THEN
        RAISE EXCEPTION 'append-only: % does not accept UPDATE', TG_TABLE_NAME
            USING ERRCODE = '25006';
    END IF;
    IF NEW.blob_id IS DISTINCT FROM OLD.blob_id
       AND NOT EXISTS (
               SELECT 1
                 FROM proxima_core.blob old_blob
                 JOIN proxima_core.blob new_blob
                   ON new_blob.schema_id = old_blob.schema_id
                  AND new_blob.content_hash = old_blob.content_hash
                WHERE old_blob.blob_id = OLD.blob_id
                  AND new_blob.blob_id = NEW.blob_id
           ) THEN
        RAISE EXCEPTION
            'append-only: %.blob_id may only be repointed at a blob row naming the same '
            'schema_id and content_hash', TG_TABLE_NAME
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;
