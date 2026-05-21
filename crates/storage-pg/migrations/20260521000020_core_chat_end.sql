CREATE TABLE proxima_core.chat_end_requested_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    thread_key text NOT NULL,
    target_personality_instance_id uuid NOT NULL REFERENCES proxima_core.personality(personality_instance_id),
    target_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    requested_by_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    reason text,
    idempotency_key text NOT NULL,
    requested_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT chat_end_requested_v1_thread_key_chk CHECK (char_length(thread_key) BETWEEN 1 AND 240),
    CONSTRAINT chat_end_requested_v1_reason_chk CHECK (reason IS NULL OR char_length(reason) BETWEEN 1 AND 4000),
    CONSTRAINT chat_end_requested_v1_idempotency_key_chk CHECK (char_length(idempotency_key) BETWEEN 1 AND 240)
);

CREATE TABLE proxima_core.chat_ended_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    thread_key text NOT NULL,
    request_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    ended_by_personality_instance_id uuid NOT NULL REFERENCES proxima_core.personality(personality_instance_id),
    ended_by_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    summary_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id) DEFERRABLE INITIALLY DEFERRED,
    idempotency_key text NOT NULL,
    ended_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT chat_ended_v1_thread_key_chk CHECK (char_length(thread_key) BETWEEN 1 AND 240),
    CONSTRAINT chat_ended_v1_idempotency_key_chk CHECK (char_length(idempotency_key) BETWEEN 1 AND 240)
);

CREATE TABLE proxima_core.chat_summary_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    thread_key text NOT NULL,
    request_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    ended_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    summarized_by_personality_instance_id uuid NOT NULL REFERENCES proxima_core.personality(personality_instance_id),
    summarized_by_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    summary text NOT NULL,
    included_memory_ids uuid[] NOT NULL DEFAULT '{}',
    context_memory_ids_used uuid[] NOT NULL DEFAULT '{}',
    idempotency_key text NOT NULL,
    summarized_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT chat_summary_v1_thread_key_chk CHECK (char_length(thread_key) BETWEEN 1 AND 240),
    CONSTRAINT chat_summary_v1_summary_chk CHECK (char_length(summary) BETWEEN 1 AND 20000),
    CONSTRAINT chat_summary_v1_idempotency_key_chk CHECK (char_length(idempotency_key) BETWEEN 1 AND 240)
);

CREATE UNIQUE INDEX idx_chat_ended_v1_thread
    ON proxima_core.chat_ended_v1 (thread_key);

CREATE INDEX idx_chat_end_requested_v1_thread
    ON proxima_core.chat_end_requested_v1 (thread_key, requested_at DESC);

CREATE INDEX idx_chat_end_requested_v1_target
    ON proxima_core.chat_end_requested_v1 (target_personality_instance_id, requested_at DESC);

CREATE INDEX idx_chat_summary_v1_thread
    ON proxima_core.chat_summary_v1 (thread_key, summarized_at DESC);

CREATE INDEX idx_chat_summary_v1_request
    ON proxima_core.chat_summary_v1 (request_memory_id);
