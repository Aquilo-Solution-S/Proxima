-- #488: forgetting becomes a state on the Memory row. Memories forgotten
-- before this release (cooled rows and their cold/ objects) are not
-- migrated, so a used store refuses before changing the schema.
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
            'fresh store required: Memory, Goal, cooled memory, or retained identity data exists'
            USING ERRCODE = '55000',
                  HINT = 'Create a fresh store before starting this release.';
    END IF;
END;
$$;

-- The language a Memory's words were projected in, so the projection can be
-- rebuilt from the Memory alone (restore). Writes stamp the draft's language;
-- one that names none, or asks for the deployment configuration, gets it.
ALTER TABLE proxima_core.memory
    ADD COLUMN lexical_language regconfig NOT NULL DEFAULT proxima_core.lexical_config();
