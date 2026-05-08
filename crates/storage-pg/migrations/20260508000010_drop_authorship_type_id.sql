-- Phase 2 Step 4: drop authorship type_id columns. Personality identity
-- is now (owner, instance_id) only; external authorship is encoded by
-- nil-uuid on the instance_id column. Pre-v1: no shipped deployments.

-- memories ---------------------------------------------------------------
DROP INDEX IF EXISTS proxima_core.idx_memories_personality_instance;

ALTER TABLE proxima_core.memories
    DROP COLUMN IF EXISTS personality_type_id;

CREATE INDEX IF NOT EXISTS idx_memories_personality_instance
    ON proxima_core.memories (personality_instance_id);

-- change_event -----------------------------------------------------------
ALTER TABLE proxima_core.change_event
    DROP COLUMN IF EXISTS entity_personality_type_id;

-- goals ------------------------------------------------------------------
ALTER TABLE proxima_core.goals
    DROP CONSTRAINT IF EXISTS goals_authorship_shape_chk;

ALTER TABLE proxima_core.goals
    DROP COLUMN IF EXISTS personality_type_id;

ALTER TABLE proxima_core.goals
    ADD CONSTRAINT goals_authorship_shape_chk CHECK (
        (
            authorship_kind = 'User'
            AND authorship_origin IS NULL AND authorship_operator_id IS NULL
            AND authorship_tool_id IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND personality_instance_id IS NULL
        ) OR (
            authorship_kind = 'System' AND authorship_origin = 'Operator'
            AND authorship_operator_id IS NOT NULL
            AND operator_kind IS NOT NULL AND model_id IS NOT NULL
            AND prompt_version IS NOT NULL
            AND personality_instance_id IS NOT NULL
            AND authorship_tool_id IS NULL
        ) OR (
            authorship_kind = 'System' AND authorship_origin = 'Tool'
            AND authorship_tool_id IS NOT NULL
            AND authorship_operator_id IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND personality_instance_id IS NULL
        ) OR (
            authorship_kind = 'External'
            AND authorship_origin IS NULL AND authorship_operator_id IS NULL
            AND authorship_tool_id IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND personality_instance_id IS NULL
        )
    );

-- source_batch_f2a -------------------------------------------------------
ALTER TABLE proxima_core.source_batch_f2a
    DROP CONSTRAINT IF EXISTS source_batch_f2a_pkey;

ALTER TABLE proxima_core.source_batch_f2a
    DROP COLUMN IF EXISTS personality_type_id;

ALTER TABLE proxima_core.source_batch_f2a
    ADD PRIMARY KEY (
        batch_id,
        operator_id,
        prompt_version,
        model_id,
        personality_instance_id
    );

-- a2p_invocations --------------------------------------------------------
ALTER TABLE proxima_core.a2p_invocations
    DROP CONSTRAINT IF EXISTS a2p_invocations_pkey;

ALTER TABLE proxima_core.a2p_invocations
    DROP COLUMN IF EXISTS personality_type_id;

ALTER TABLE proxima_core.a2p_invocations
    ADD PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        operator_id,
        prompt_version,
        model_id,
        personality_instance_id,
        context_hash,
        input_hash
    );
