-- Existing development databases may have the former personality_state_hash
-- carrier columns. Keep prior migrations checksum-stable and perform the
-- compatibility cleanup here.

ALTER TABLE proxima_core.memories
    DROP CONSTRAINT IF EXISTS memories_variant_chk;
ALTER TABLE proxima_core.memories
    DROP COLUMN IF EXISTS personality_state_hash;
ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_variant_chk CHECK (
        (
            event_id IS NOT NULL AND citation_mapping_id IS NOT NULL
            AND kind IS NULL AND text IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND personality_id IS NULL
            AND supersedes IS NULL
        ) OR (
            kind IS NOT NULL AND text IS NOT NULL AND operator_kind IS NOT NULL
            AND model_id IS NOT NULL AND prompt_version IS NOT NULL
            AND personality_id IS NOT NULL
            AND event_id IS NULL AND citation_mapping_id IS NULL
        )
    );

ALTER TABLE proxima_core.goals
    DROP CONSTRAINT IF EXISTS goals_authorship_shape_chk;
ALTER TABLE proxima_core.goals
    DROP COLUMN IF EXISTS personality_state_hash;
ALTER TABLE proxima_core.goals
    ADD CONSTRAINT goals_authorship_shape_chk CHECK (
        (
            authorship_kind = 'User'
            AND authorship_origin IS NULL AND authorship_operator_id IS NULL
            AND authorship_tool_id IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND personality_id IS NULL
        ) OR (
            authorship_kind = 'System' AND authorship_origin = 'Operator'
            AND authorship_operator_id IS NOT NULL
            AND operator_kind IS NOT NULL AND model_id IS NOT NULL
            AND prompt_version IS NOT NULL AND personality_id IS NOT NULL
            AND authorship_tool_id IS NULL
        ) OR (
            authorship_kind = 'System' AND authorship_origin = 'Tool'
            AND authorship_tool_id IS NOT NULL
            AND authorship_operator_id IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL AND personality_id IS NULL
        ) OR (
            authorship_kind = 'External'
            AND authorship_origin IS NULL AND authorship_operator_id IS NULL
            AND authorship_tool_id IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL AND personality_id IS NULL
        )
    );

ALTER TABLE proxima_core.source_batch_f2a
    DROP CONSTRAINT IF EXISTS source_batch_f2a_pkey;
ALTER TABLE proxima_core.source_batch_f2a
    DROP CONSTRAINT IF EXISTS source_batch_f2a_personality_state_hash_chk;
ALTER TABLE proxima_core.source_batch_f2a
    DROP COLUMN IF EXISTS personality_state_hash;
ALTER TABLE proxima_core.source_batch_f2a
    ADD PRIMARY KEY (
        batch_id,
        operator_id,
        prompt_version,
        model_id,
        personality_id
    );

ALTER TABLE proxima_core.a2p_invocations
    DROP CONSTRAINT IF EXISTS a2p_invocations_pkey;
ALTER TABLE proxima_core.a2p_invocations
    DROP CONSTRAINT IF EXISTS a2p_invocations_personality_hash_chk;
ALTER TABLE proxima_core.a2p_invocations
    DROP COLUMN IF EXISTS personality_state_hash;
ALTER TABLE proxima_core.a2p_invocations
    ADD PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        operator_id,
        prompt_version,
        model_id,
        personality_id,
        context_hash,
        input_hash
    );

ALTER TABLE proxima_core.change_event
    DROP COLUMN IF EXISTS entity_personality_state_hash;
