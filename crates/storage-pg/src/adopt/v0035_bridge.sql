-- v0.0.36: retire host scheduling, the change log, and built-in call recording.
-- Stop older binaries before upgrading. Existing call records and their I/O
-- are erased; ordinary admissions, Goals, and publications are retained.
-- External object deletion is durable debt for the existing purge drain.

-- Every table below is FORCE RLS, and only the platform scope sees every
-- owner's rows. The migration runner binds it; binding it here too means a
-- run without it erases what it should rather than matching nothing and
-- dropping the sidecar table anyway. Transaction-local, like the runner's.
SELECT set_config('app.proxima_scope', 'platform', true);

ALTER TABLE proxima_core.goal
    DROP CONSTRAINT goal_wake_id_fkey,
    DROP COLUMN wake_id;
DROP TABLE proxima_core.wake_config;
DROP TABLE proxima_core.announce;
DROP FUNCTION proxima_core.wake_pin_target_checks();
DROP TYPE proxima_core.wake_trigger_kind;
DROP TYPE proxima_core.announce_op;
DROP TYPE proxima_core.announce_entity;

-- A Goal command's stored declaration still names the retired `wake` field:
-- at the top level for modify and decompose children, under `draft` for
-- create. Replay compares a retry's declaration exactly, so a retry of such a
-- write would conflict instead of replaying. The table is append-only; this
-- one statement runs with that guard off, under the ACCESS EXCLUSIVE lock
-- DISABLE TRIGGER takes.
ALTER TABLE proxima_core.goal_replay_declaration
    DISABLE TRIGGER goal_replay_declaration_append_only;
UPDATE proxima_core.goal_replay_declaration
   SET declaration = (declaration - 'wake') #- '{draft,wake}'
 WHERE declaration ? 'wake'
    OR (declaration -> 'draft') ? 'wake';
ALTER TABLE proxima_core.goal_replay_declaration
    ENABLE TRIGGER goal_replay_declaration_append_only;

-- Owner-pinned call sidecars survive cooling and transfer, so their t values
-- also find fully cooled records whose memory_head no longer exists.
CREATE TEMP TABLE retired_call_admissions ON COMMIT DROP AS
    SELECT t FROM proxima_core.mcp_call_logged_v1
    UNION
    -- FORGET-POLICY: includes-forgotten (retiring a schema removes forgotten rows too)
    SELECT t FROM proxima_core.memory WHERE schema_id = 'core/mcp-call-logged-v1'
    UNION
    SELECT t FROM proxima_core.cooled WHERE source_id = 'core/mcp-call'
    UNION
    SELECT c.t FROM proxima_core.cooled c
      JOIN proxima_core.content p USING (content_id)
     WHERE p.schema_id = 'core/mcp-call-logged-v1'
    UNION
    SELECT t FROM proxima_core.publication_outbox
     WHERE schema_id = 'core/mcp-call-logged-v1';

CREATE TEMP TABLE retired_call_handles ON COMMIT DROP AS
    SELECT handle FROM proxima_core.memory_head
     WHERE schema_id = 'core/mcp-call-logged-v1'
    UNION
    -- FORGET-POLICY: includes-forgotten (retiring a schema removes forgotten rows too)
    SELECT handle FROM proxima_core.memory
     WHERE t IN (SELECT t FROM pg_temp.retired_call_admissions)
    UNION
    SELECT handle FROM proxima_core.cooled
     WHERE t IN (SELECT t FROM pg_temp.retired_call_admissions);

INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id)
    SELECT object_key, owner_id FROM proxima_core.cooled
     WHERE t IN (SELECT t FROM pg_temp.retired_call_admissions)
    ON CONFLICT (object_key) DO UPDATE SET enqueued_at = now();

DELETE FROM proxima_core.memory_external_origin
 WHERE t IN (SELECT t FROM pg_temp.retired_call_admissions);
DELETE FROM proxima_core.sketch
 WHERE t IN (SELECT t FROM pg_temp.retired_call_admissions);
DELETE FROM proxima_core.embeddings
 WHERE entity_id IN (SELECT t FROM pg_temp.retired_call_admissions);
DELETE FROM proxima_core.embedding_heads
 WHERE entity_id IN (SELECT t FROM pg_temp.retired_call_admissions);
DELETE FROM proxima_core.embedding_jobs
 WHERE entity_id IN (SELECT t FROM pg_temp.retired_call_admissions);
DELETE FROM proxima_core.publication_origin
 WHERE t IN (SELECT t FROM pg_temp.retired_call_admissions);
-- publication_delivery cascades from the outbox record.
DELETE FROM proxima_core.publication_outbox
 WHERE t IN (SELECT t FROM pg_temp.retired_call_admissions);
DELETE FROM proxima_core.ingest_keys
 WHERE source_id = 'core/mcp-call'
    OR t IN (SELECT t FROM pg_temp.retired_call_admissions);

-- Keep the normal DELETE triggers: erased pin targets retain their
-- payload-free identity witness; projection rows cascade with hot admissions.
-- FORGET-POLICY: includes-forgotten (retiring a schema removes forgotten rows too)
DELETE FROM proxima_core.memory
 WHERE t IN (SELECT t FROM pg_temp.retired_call_admissions);
DELETE FROM proxima_core.cooled
 WHERE t IN (SELECT t FROM pg_temp.retired_call_admissions);
DELETE FROM proxima_core.memory_head
 WHERE handle IN (SELECT handle FROM pg_temp.retired_call_handles);
DELETE FROM proxima_core.closed_handle
 WHERE handle IN (SELECT handle FROM pg_temp.retired_call_handles);
DELETE FROM proxima_core.content c
 WHERE schema_id = 'core/mcp-call-logged-v1'
   -- FORGET-POLICY: includes-forgotten (a forgotten memory still cites its content)
   AND NOT EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.content_id = c.content_id)
   AND NOT EXISTS (SELECT 1 FROM proxima_core.cooled m WHERE m.content_id = c.content_id);

-- A retained admission citing retired I/O is not a released call-record
-- shape. Refuse atomically rather than erase other data or leave old I/O.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM proxima_core.blob b
         WHERE b.schema_id = 'core/mcp-call-io-v1'
           -- FORGET-POLICY: includes-forgotten (a forgotten memory still cites its content)
           AND (EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.blob_id = b.blob_id)
             OR EXISTS (SELECT 1 FROM proxima_core.cooled m WHERE m.blob_id = b.blob_id))
    ) THEN
        RAISE EXCEPTION 'retired call I/O is cited by a retained admission; resolve those citations before upgrading';
    END IF;
END;
$$;

-- Call I/O uses inline citation identities, not uploaded object locators.
DELETE FROM proxima_core.blob WHERE schema_id = 'core/mcp-call-io-v1';

DROP TABLE proxima_core.mcp_call_logged_v1;
DELETE FROM proxima_core.flavor_surface
 WHERE table_name = 'proxima_core.mcp_call_logged_v1';
