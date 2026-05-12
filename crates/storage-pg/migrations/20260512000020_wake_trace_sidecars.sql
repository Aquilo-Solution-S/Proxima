-- Wake-trace persistence sidecars.
-- See docs/superpowers/specs/2026-05-12-proxima-harness-design.md
-- §"Observability: three layers".

CREATE TABLE proxima_core.wake_trace_v1 (
    memory_id                   uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    invocation_id               uuid NOT NULL,
    wake_entry_id               uuid NOT NULL,
    personality_instance_id     uuid NOT NULL,
    model_target_ref            text NOT NULL,
    model_id                    text NOT NULL,
    started_at                  timestamptz NOT NULL,
    finished_at                 timestamptz NOT NULL,
    outcome_kind                text NOT NULL,
    failure_reason              text,
    rounds_used                 integer NOT NULL,
    finish_reason               text,
    total_prompt_tokens         bigint,
    total_completion_tokens     bigint,
    tool_call_count             integer NOT NULL,
    jsonl_truncated             boolean NOT NULL
);

CREATE INDEX wake_trace_v1_invocation_idx
    ON proxima_core.wake_trace_v1 (invocation_id);

CREATE INDEX wake_trace_v1_personality_idx
    ON proxima_core.wake_trace_v1 (personality_instance_id, started_at DESC);

CREATE TABLE proxima_core.cited_wake_trace_jsonl_v1 (
    cited_object_id             uuid PRIMARY KEY REFERENCES proxima_core.cited_objects(cited_object_id),
    byte_len                    bigint NOT NULL,
    line_count                  bigint NOT NULL,
    truncated                   boolean NOT NULL,
    storage_path                text,
    body                        bytea NOT NULL
);

CREATE TABLE proxima_core.citation_wake_trace_v1 (
    citation_mapping_id         uuid PRIMARY KEY REFERENCES proxima_core.citation_mappings(citation_mapping_id),
    byte_range_start            bigint,
    byte_range_end              bigint
);
