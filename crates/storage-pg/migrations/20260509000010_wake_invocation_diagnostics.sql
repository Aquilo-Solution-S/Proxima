-- Bounded wake-invocation diagnostics for local Goose/MCP debugging.
ALTER TABLE proxima_core.personality_wake_invocations
    ADD COLUMN IF NOT EXISTS exit_code integer NULL;
ALTER TABLE proxima_core.personality_wake_invocations
    ADD COLUMN IF NOT EXISTS duration_ms bigint NULL
        CHECK (duration_ms IS NULL OR duration_ms >= 0);
ALTER TABLE proxima_core.personality_wake_invocations
    ADD COLUMN IF NOT EXISTS stdout_tail text NULL;
ALTER TABLE proxima_core.personality_wake_invocations
    ADD COLUMN IF NOT EXISTS stderr_tail text NULL;
ALTER TABLE proxima_core.personality_wake_invocations
    ADD COLUMN IF NOT EXISTS stdout_truncated boolean NOT NULL DEFAULT false;
ALTER TABLE proxima_core.personality_wake_invocations
    ADD COLUMN IF NOT EXISTS stderr_truncated boolean NOT NULL DEFAULT false;

CREATE TABLE IF NOT EXISTS proxima_core.personality_wake_invocation_logs (
    log_seq serial PRIMARY KEY,
    owner_principal_kind text NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    personality_instance_id uuid NOT NULL,
    wake_entry_id uuid NOT NULL,
    change_event_seq uuid NOT NULL,
    at timestamptz NOT NULL DEFAULT now(),
    phase text NOT NULL,
    tool_id text,
    status text NOT NULL,
    duration_ms bigint NULL CHECK (duration_ms IS NULL OR duration_ms >= 0),
    message_tail text,
    CONSTRAINT personality_wake_invocation_logs_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT personality_wake_invocation_logs_status_chk
        CHECK (status IN ('started', 'succeeded', 'failed')),
    FOREIGN KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id,
        wake_entry_id,
        change_event_seq
    ) REFERENCES proxima_core.personality_wake_invocations (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id,
        wake_entry_id,
        change_event_seq
    ) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS personality_wake_invocation_logs_invocation_idx
ON proxima_core.personality_wake_invocation_logs (
    owner_principal_kind,
    owner_principal_id,
    owner_org_id,
    personality_instance_id,
    wake_entry_id,
    change_event_seq,
    log_seq
);

CREATE INDEX IF NOT EXISTS personality_wake_invocations_instance_started_idx
ON proxima_core.personality_wake_invocations (
    owner_principal_kind,
    owner_principal_id,
    owner_org_id,
    personality_instance_id,
    started_at DESC
);
