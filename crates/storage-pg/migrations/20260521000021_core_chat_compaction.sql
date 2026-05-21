CREATE TABLE proxima_core.chat_compaction_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    thread_key text NOT NULL,
    compacted_by_personality_instance_id uuid NOT NULL REFERENCES proxima_core.personality(personality_instance_id),
    compacted_by_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    summary text NOT NULL,
    included_memory_ids uuid[] NOT NULL DEFAULT '{}',
    context_memory_ids_used uuid[] NOT NULL DEFAULT '{}',
    idempotency_key text NOT NULL,
    compacted_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT chat_compaction_v1_thread_key_chk CHECK (char_length(thread_key) BETWEEN 1 AND 240),
    CONSTRAINT chat_compaction_v1_summary_chk CHECK (char_length(summary) BETWEEN 1 AND 20000),
    CONSTRAINT chat_compaction_v1_idempotency_key_chk CHECK (char_length(idempotency_key) BETWEEN 1 AND 240)
);

CREATE INDEX idx_chat_compaction_v1_thread
    ON proxima_core.chat_compaction_v1 (thread_key, compacted_at DESC);
