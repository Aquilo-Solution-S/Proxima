-- Drop the legacy single-string personality_id columns now that personality
-- identity is a (personality_type_id, personality_instance_id) pair.
-- Pre-v1 schema: no shipped deployments to migrate.
--
-- Source-batch F2A and A2P invocation rows additionally gain a
-- wake_chain_depth column so the wake-cycle floor (docs §Goals) is
-- visible alongside the operator-side bookkeeping.

-- memories ----------------------------------------------------------------
ALTER TABLE proxima_core.memories
    DROP COLUMN IF EXISTS personality_id;

-- change_event -----------------------------------------------------------
ALTER TABLE proxima_core.change_event
    DROP COLUMN IF EXISTS entity_personality_id;

-- goals ------------------------------------------------------------------
ALTER TABLE proxima_core.goals
    DROP CONSTRAINT IF EXISTS goals_authorship_shape_chk;

ALTER TABLE proxima_core.goals
    ADD COLUMN IF NOT EXISTS personality_type_id text,
    ADD COLUMN IF NOT EXISTS personality_instance_id uuid;

UPDATE proxima_core.goals
SET personality_type_id = COALESCE(personality_type_id, personality_id),
    personality_instance_id = COALESCE(personality_instance_id, '00000000-0000-0000-0000-000000000000'::uuid)
WHERE authorship_kind = 'System'
  AND authorship_origin = 'Operator';

ALTER TABLE proxima_core.goals
    DROP COLUMN IF EXISTS personality_id;

ALTER TABLE proxima_core.goals
    ADD CONSTRAINT goals_authorship_shape_chk CHECK (
        (
            authorship_kind = 'User'
            AND authorship_origin IS NULL AND authorship_operator_id IS NULL
            AND authorship_tool_id IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND personality_type_id IS NULL AND personality_instance_id IS NULL
        ) OR (
            authorship_kind = 'System' AND authorship_origin = 'Operator'
            AND authorship_operator_id IS NOT NULL
            AND operator_kind IS NOT NULL AND model_id IS NOT NULL
            AND prompt_version IS NOT NULL
            AND personality_type_id IS NOT NULL AND personality_instance_id IS NOT NULL
            AND authorship_tool_id IS NULL
        ) OR (
            authorship_kind = 'System' AND authorship_origin = 'Tool'
            AND authorship_tool_id IS NOT NULL
            AND authorship_operator_id IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND personality_type_id IS NULL AND personality_instance_id IS NULL
        ) OR (
            authorship_kind = 'External'
            AND authorship_origin IS NULL AND authorship_operator_id IS NULL
            AND authorship_tool_id IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND personality_type_id IS NULL AND personality_instance_id IS NULL
        )
    );

-- source_batch_f2a -------------------------------------------------------
ALTER TABLE proxima_core.source_batch_f2a
    DROP CONSTRAINT IF EXISTS source_batch_f2a_pkey;

ALTER TABLE proxima_core.source_batch_f2a
    DROP COLUMN IF EXISTS personality_id;

ALTER TABLE proxima_core.source_batch_f2a
    ALTER COLUMN personality_type_id SET NOT NULL,
    ALTER COLUMN personality_instance_id SET NOT NULL;

ALTER TABLE proxima_core.source_batch_f2a
    ADD COLUMN IF NOT EXISTS wake_chain_depth smallint NOT NULL DEFAULT 0
        CHECK (wake_chain_depth >= 0);

ALTER TABLE proxima_core.source_batch_f2a
    ADD PRIMARY KEY (
        batch_id,
        operator_id,
        prompt_version,
        model_id,
        personality_type_id,
        personality_instance_id
    );

-- a2p_invocations ---------------------------------------------------------
ALTER TABLE proxima_core.a2p_invocations
    DROP CONSTRAINT IF EXISTS a2p_invocations_pkey;

ALTER TABLE proxima_core.a2p_invocations
    DROP COLUMN IF EXISTS personality_id;

ALTER TABLE proxima_core.a2p_invocations
    ALTER COLUMN personality_type_id SET NOT NULL,
    ALTER COLUMN personality_instance_id SET NOT NULL;

ALTER TABLE proxima_core.a2p_invocations
    ADD COLUMN IF NOT EXISTS wake_chain_depth smallint NOT NULL DEFAULT 0
        CHECK (wake_chain_depth >= 0);

ALTER TABLE proxima_core.a2p_invocations
    ADD PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        operator_id,
        prompt_version,
        model_id,
        personality_type_id,
        personality_instance_id,
        context_hash,
        input_hash
    );
