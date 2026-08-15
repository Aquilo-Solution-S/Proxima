-- Slice 5: reusable wake_config (the one UPDATE table).

CREATE TYPE proxima_core.wake_trigger_kind AS ENUM (
    'fact_schema',
    'fact_memory'
);

CREATE TABLE proxima_core.wake_config (
    wake_id uuid PRIMARY KEY DEFAULT uuidv7(),
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    trigger_kind proxima_core.wake_trigger_kind NOT NULL,
    trigger_schema_id text,
    trigger_t uuid,
    tool_ids text[] NOT NULL,
    prompt text NOT NULL,
    hard_memory_t uuid[] NOT NULL DEFAULT '{}',
    CONSTRAINT wake_trigger_xor_chk CHECK (
        (trigger_kind = 'fact_schema' AND trigger_schema_id IS NOT NULL AND trigger_t IS NULL)
        OR (trigger_kind = 'fact_memory' AND trigger_t IS NOT NULL AND trigger_schema_id IS NULL)
    ),
    CONSTRAINT wake_tools_chk CHECK (
        array_length(tool_ids, 1) >= 1 AND array_position(tool_ids, NULL) IS NULL
    ),
    CONSTRAINT wake_prompt_chk CHECK (length(btrim(prompt)) > 0),
    CONSTRAINT wake_hard_no_null_chk CHECK (array_position(hard_memory_t, NULL) IS NULL)
);

ALTER TABLE proxima_core.goal
    ADD CONSTRAINT goal_wake_fk
    FOREIGN KEY (wake_id) REFERENCES proxima_core.wake_config (wake_id)
    ON DELETE RESTRICT;
