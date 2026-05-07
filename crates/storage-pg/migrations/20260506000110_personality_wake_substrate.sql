-- Personality wake/decide/write substrate.
-- Pre-v1 compatibility migration: retain legacy columns where present,
-- but move new writes to split personality identity.

ALTER TABLE proxima_core.memories
    DROP CONSTRAINT IF EXISTS memories_variant_chk;
ALTER TABLE proxima_core.memories
    DROP CONSTRAINT IF EXISTS memories_operator_kind_values_chk;
ALTER TABLE proxima_core.memories
    ADD COLUMN IF NOT EXISTS personality_type_id text,
    ADD COLUMN IF NOT EXISTS personality_instance_id uuid,
    ADD COLUMN IF NOT EXISTS wake_chain_depth smallint NOT NULL DEFAULT 0;

UPDATE proxima_core.memories
SET personality_type_id = COALESCE(personality_type_id, personality_id, 'external/event-source'),
    personality_instance_id = COALESCE(personality_instance_id, '00000000-0000-0000-0000-000000000000'::uuid)
WHERE personality_type_id IS NULL
   OR personality_instance_id IS NULL;

ALTER TABLE proxima_core.memories
    ALTER COLUMN personality_type_id SET NOT NULL,
    ALTER COLUMN personality_instance_id SET NOT NULL;

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_operator_kind_values_chk
        CHECK (operator_kind IS NULL OR operator_kind IN ('FtoA', 'AtoP', 'ExternalAgent', 'Wake')),
    ADD CONSTRAINT memories_wake_chain_depth_chk
        CHECK (wake_chain_depth >= 0),
    ADD CONSTRAINT memories_variant_chk CHECK (
        (
            event_id IS NOT NULL AND citation_mapping_id IS NOT NULL
            AND kind IS NULL AND text IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND supersedes IS NULL
        ) OR (
            kind IS NOT NULL AND text IS NOT NULL AND operator_kind IS NOT NULL
            AND model_id IS NOT NULL AND prompt_version IS NOT NULL
            AND event_id IS NULL AND citation_mapping_id IS NULL
        )
    );

CREATE INDEX IF NOT EXISTS idx_memories_personality_instance
    ON proxima_core.memories
       (personality_type_id, personality_instance_id);

ALTER TABLE proxima_core.change_event
    ADD COLUMN IF NOT EXISTS entity_personality_type_id text,
    ADD COLUMN IF NOT EXISTS entity_personality_instance_id uuid,
    ADD COLUMN IF NOT EXISTS wake_chain_depth smallint NOT NULL DEFAULT 0;

UPDATE proxima_core.change_event
SET entity_personality_type_id = COALESCE(
        entity_personality_type_id,
        entity_personality_id,
        CASE WHEN entity_kind = 'Fact' THEN 'external/event-source' END
    ),
    entity_personality_instance_id = COALESCE(
        entity_personality_instance_id,
        CASE WHEN entity_kind = 'Fact'
             THEN '00000000-0000-0000-0000-000000000000'::uuid
        END
    )
WHERE kind = 'EntityAppend';

ALTER TABLE proxima_core.source_batch_f2a
    ADD COLUMN IF NOT EXISTS personality_type_id text,
    ADD COLUMN IF NOT EXISTS personality_instance_id uuid;

UPDATE proxima_core.source_batch_f2a
SET personality_type_id = COALESCE(personality_type_id, personality_id),
    personality_instance_id = COALESCE(personality_instance_id, '00000000-0000-0000-0000-000000000000'::uuid);

ALTER TABLE proxima_core.a2p_invocations
    ADD COLUMN IF NOT EXISTS personality_type_id text,
    ADD COLUMN IF NOT EXISTS personality_instance_id uuid;

UPDATE proxima_core.a2p_invocations
SET personality_type_id = COALESCE(personality_type_id, personality_id),
    personality_instance_id = COALESCE(personality_instance_id, '00000000-0000-0000-0000-000000000000'::uuid);

CREATE TABLE IF NOT EXISTS proxima_core.personality_wake_config (
    owner_principal_kind                 text NOT NULL,
    owner_principal_id                   uuid NOT NULL,
    owner_org_id                         uuid NOT NULL,
    personality_type_id                  text NOT NULL,
    personality_instance_id              uuid NOT NULL,
    current_self_perspective_memory_id   uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    wake_filters                         jsonb NOT NULL,
    status                               text NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'needs_repair')),
    created_at                           timestamptz NOT NULL DEFAULT now(),
    updated_at                           timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT personality_wake_config_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_type_id,
        personality_instance_id
    )
);

CREATE TABLE IF NOT EXISTS proxima_core.personality_wake_cursor (
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    personality_type_id      text NOT NULL,
    personality_instance_id  uuid NOT NULL,
    last_considered_seq      uuid NOT NULL,
    updated_at               timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT personality_wake_cursor_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_type_id,
        personality_instance_id
    )
);

CREATE TABLE IF NOT EXISTS proxima_core.personality_wake_invocations (
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    personality_type_id      text NOT NULL,
    personality_instance_id  uuid NOT NULL,
    change_event_seq         uuid NOT NULL,
    status                   text NOT NULL
        CHECK (status IN ('running', 'succeeded', 'truncated', 'failed')),
    started_at               timestamptz NOT NULL,
    finished_at              timestamptz,
    turn_count               smallint NOT NULL DEFAULT 0,
    cost_usd                 numeric(10,6) NOT NULL DEFAULT 0,
    CONSTRAINT personality_wake_invocations_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    UNIQUE (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_type_id,
        personality_instance_id,
        change_event_seq
    )
);
