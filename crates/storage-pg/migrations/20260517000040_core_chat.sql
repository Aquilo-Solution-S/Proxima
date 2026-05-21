CREATE TABLE proxima_core.chat_started_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    thread_key text NOT NULL,
    started_by_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    target_personality_instance_id uuid NOT NULL REFERENCES proxima_core.personality(personality_instance_id),
    target_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    title text,
    idempotency_key text NOT NULL,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT chat_started_v1_thread_key_chk CHECK (char_length(thread_key) BETWEEN 1 AND 240),
    CONSTRAINT chat_started_v1_title_chk CHECK (title IS NULL OR char_length(title) BETWEEN 1 AND 240),
    CONSTRAINT chat_started_v1_idempotency_key_chk CHECK (char_length(idempotency_key) BETWEEN 1 AND 240)
);

CREATE TABLE proxima_core.chat_message_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    thread_key text NOT NULL,
    message text NOT NULL,
    target_personality_instance_id uuid NOT NULL REFERENCES proxima_core.personality(personality_instance_id),
    target_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    sent_by_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    parent_message_memory_id uuid REFERENCES proxima_core.memories(memory_id),
    context_memory_ids uuid[] NOT NULL DEFAULT '{}',
    context_goal_ids uuid[] NOT NULL DEFAULT '{}',
    idempotency_key text NOT NULL,
    sent_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT chat_message_v1_thread_key_chk CHECK (char_length(thread_key) BETWEEN 1 AND 240),
    CONSTRAINT chat_message_v1_message_chk CHECK (char_length(message) BETWEEN 1 AND 8000),
    CONSTRAINT chat_message_v1_idempotency_key_chk CHECK (char_length(idempotency_key) BETWEEN 1 AND 240)
);

CREATE TABLE proxima_core.chat_reply_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    message_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    thread_key text NOT NULL,
    reply text NOT NULL,
    replied_by_personality_instance_id uuid NOT NULL REFERENCES proxima_core.personality(personality_instance_id),
    replied_by_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    context_memory_ids_used uuid[] NOT NULL DEFAULT '{}',
    idempotency_key text NOT NULL,
    replied_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT chat_reply_v1_thread_key_chk CHECK (char_length(thread_key) BETWEEN 1 AND 240),
    CONSTRAINT chat_reply_v1_reply_chk CHECK (char_length(reply) BETWEEN 1 AND 12000),
    CONSTRAINT chat_reply_v1_idempotency_key_chk CHECK (char_length(idempotency_key) BETWEEN 1 AND 240)
);

CREATE INDEX idx_chat_started_v1_thread
    ON proxima_core.chat_started_v1 (thread_key, started_at DESC);

CREATE INDEX idx_chat_message_v1_target
    ON proxima_core.chat_message_v1 (target_personality_instance_id, sent_at DESC);

CREATE INDEX idx_chat_message_v1_thread
    ON proxima_core.chat_message_v1 (thread_key, sent_at DESC);

CREATE INDEX idx_chat_reply_v1_message
    ON proxima_core.chat_reply_v1 (message_memory_id, replied_at DESC);
