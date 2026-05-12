-- Greenfield hard migration: existing personality wake state is discarded.
DROP TABLE IF EXISTS proxima_core.personality_wake_invocations;
DROP TABLE IF EXISTS proxima_core.personality_wake_cursor;
DROP TABLE IF EXISTS proxima_core.personality_wake_config;

CREATE TABLE proxima_core.root_personality_perspective_v1 (
    memory_id uuid PRIMARY KEY
        REFERENCES proxima_core.memories(memory_id),
    display_name text NOT NULL,
    purpose text NOT NULL,
    system_prompt text NOT NULL,
    CONSTRAINT root_personality_perspective_display_name_chk
        CHECK (length(trim(display_name)) > 0),
    CONSTRAINT root_personality_perspective_purpose_chk
        CHECK (length(trim(purpose)) > 0),
    CONSTRAINT root_personality_perspective_system_prompt_chk
        CHECK (length(trim(system_prompt)) > 0)
);

CREATE TABLE proxima_core.personality (
    owner_principal_kind text NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    personality_instance_id uuid NOT NULL,
    current_root_perspective_memory_id uuid NOT NULL
        REFERENCES proxima_core.memories(memory_id),
    max_wake_chain_depth integer NOT NULL DEFAULT 10,
    status text NOT NULL DEFAULT 'active',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    tombstoned_at timestamptz,
    CONSTRAINT personality_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT personality_status_chk
        CHECK (status IN ('active', 'needs_repair', 'tombstoned')),
    CONSTRAINT personality_depth_chk
        CHECK (max_wake_chain_depth >= 0),
    CONSTRAINT personality_tombstoned_at_chk
        CHECK ((status = 'tombstoned') = (tombstoned_at IS NOT NULL)),
    PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id
    )
);

CREATE TABLE proxima_core.personality_wake_entries (
    owner_principal_kind text NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    personality_instance_id uuid NOT NULL,
    wake_entry_id uuid NOT NULL,
    trigger_kind text NOT NULL,
    trigger_id text NOT NULL,
    label text NOT NULL,
    enabled boolean NOT NULL DEFAULT true,
    execution_mode text NOT NULL DEFAULT 'substrate_only',
    authored_by text NOT NULL DEFAULT 'any',
    probability_promille integer NOT NULL DEFAULT 1000,
    model_tier text NOT NULL DEFAULT 'standard',
    inference_target_ref text,
    substrate_tool_palette text[] NOT NULL DEFAULT '{}',
    workspace_tool_palette text[] NOT NULL DEFAULT '{}',
    max_rounds integer NOT NULL DEFAULT 4,
    disabled_reason text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    tombstoned_at timestamptz,
    CONSTRAINT personality_wake_entries_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT personality_wake_entries_trigger_kind_chk
        CHECK (trigger_kind IN ('on_memory', 'on_edge')),
    CONSTRAINT personality_wake_entries_execution_mode_chk
        CHECK (execution_mode IN ('substrate_only', 'workspace')),
    CONSTRAINT personality_wake_entries_authored_by_chk
        CHECK (authored_by IN ('any', 'self', 'other')),
    CONSTRAINT personality_wake_entries_model_tier_chk
        CHECK (model_tier IN ('fast', 'standard', 'deep')),
    CONSTRAINT personality_wake_entries_probability_chk
        CHECK (probability_promille BETWEEN 0 AND 1000),
    CONSTRAINT personality_wake_entries_rounds_chk
        CHECK (max_rounds > 0),
    PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id,
        wake_entry_id
    ),
    FOREIGN KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id
    ) REFERENCES proxima_core.personality (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id
    )
);

CREATE UNIQUE INDEX personality_wake_entries_active_trigger_uq
ON proxima_core.personality_wake_entries (
    owner_principal_kind,
    owner_principal_id,
    owner_org_id,
    personality_instance_id,
    trigger_kind,
    trigger_id
)
WHERE tombstoned_at IS NULL;

CREATE INDEX personality_wake_entries_trigger_idx
ON proxima_core.personality_wake_entries (trigger_kind, trigger_id)
WHERE enabled AND tombstoned_at IS NULL;

CREATE TABLE proxima_core.personality_wake_cursor (
    owner_principal_kind text NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    personality_instance_id uuid NOT NULL,
    last_considered_seq uuid NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000',
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id
    ),
    FOREIGN KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id
    ) REFERENCES proxima_core.personality (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id
    )
);

CREATE TABLE proxima_core.personality_wake_invocations (
    owner_principal_kind text NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    personality_instance_id uuid NOT NULL,
    wake_entry_id uuid NOT NULL,
    change_event_seq uuid NOT NULL,
    status text NOT NULL,
    started_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    turn_count integer NOT NULL DEFAULT 0,
    cost_usd numeric(10,6) NOT NULL DEFAULT 0,
    CONSTRAINT personality_wake_invocations_status_chk
        CHECK (status IN ('running', 'succeeded', 'truncated', 'failed')),
    CONSTRAINT personality_wake_invocations_turn_count_chk
        CHECK (turn_count >= 0),
    CONSTRAINT personality_wake_invocations_cost_chk
        CHECK (cost_usd >= 0),
    PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id,
        wake_entry_id,
        change_event_seq
    ),
    FOREIGN KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id,
        wake_entry_id
    ) REFERENCES proxima_core.personality_wake_entries (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id,
        wake_entry_id
    )
);
