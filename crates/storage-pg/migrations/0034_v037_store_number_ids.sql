-- Every identity from an earlier release belongs to the old layout, including
-- cooled rows and payload-free history. Refuse before changing the schema.
SELECT set_config('app.proxima_scope', 'platform', true);
LOCK TABLE proxima_core.memory_head, proxima_core.memory,
    proxima_core.goal_head, proxima_core.goal, proxima_core.cooled,
    proxima_core.erased_pin_target, proxima_core.closed_handle,
    proxima_core.ingest_keys, proxima_core.sketch, proxima_core.embeddings,
    proxima_core.embedding_heads, proxima_core.embedding_jobs,
    proxima_core.publication_origin, proxima_core.publication_outbox,
    proxima_core.memory_external_origin IN SHARE MODE;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM proxima_core.memory)
       OR EXISTS (SELECT 1 FROM proxima_core.goal)
       OR EXISTS (SELECT 1 FROM proxima_core.cooled)
       OR EXISTS (SELECT 1 FROM proxima_core.erased_pin_target)
       OR EXISTS (SELECT 1 FROM proxima_core.memory_head)
       OR EXISTS (SELECT 1 FROM proxima_core.goal_head)
       OR EXISTS (SELECT 1 FROM proxima_core.closed_handle)
       OR EXISTS (SELECT 1 FROM proxima_core.ingest_keys)
       OR EXISTS (SELECT 1 FROM proxima_core.sketch)
       OR EXISTS (SELECT 1 FROM proxima_core.embeddings)
       OR EXISTS (SELECT 1 FROM proxima_core.embedding_heads)
       OR EXISTS (SELECT 1 FROM proxima_core.embedding_jobs)
       OR EXISTS (SELECT 1 FROM proxima_core.publication_origin)
       OR EXISTS (SELECT 1 FROM proxima_core.publication_outbox)
       OR EXISTS (SELECT 1 FROM proxima_core.memory_external_origin)
    THEN
        RAISE EXCEPTION
            'fresh store required: Memory, Goal, or retained identity data exists'
            USING ERRCODE = '55000',
                  HINT = 'Create a fresh store before starting this release.';
    END IF;
END;
$$;

-- UUIDv8 memory identities carry their immutable store number.
CREATE TABLE proxima_core.store_identity (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    store_number integer NOT NULL CHECK (store_number BETWEEN 0 AND 65535)
);

-- Memory and Goal writes supply every identity through the Rust constructor.
ALTER TABLE proxima_core.memory ALTER COLUMN t DROP DEFAULT;
ALTER TABLE proxima_core.goal ALTER COLUMN t DROP DEFAULT;

-- Pre-1.0 stores start fresh; remote origins have one identity, not a name.
DROP TABLE proxima_core.memory_external_origin;
CREATE TABLE proxima_core.memory_external_origin (
    t uuid NOT NULL,
    memory uuid NOT NULL,
    PRIMARY KEY (t, memory)
);
