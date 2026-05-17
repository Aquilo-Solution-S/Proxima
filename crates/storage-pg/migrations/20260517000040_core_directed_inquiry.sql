CREATE TABLE proxima_core.directed_question_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    thread_key text NOT NULL,
    question text NOT NULL,
    target_personality_instance_id uuid NOT NULL REFERENCES proxima_core.personality(personality_instance_id),
    target_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    asked_by_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    parent_question_memory_id uuid REFERENCES proxima_core.memories(memory_id),
    context_memory_ids uuid[] NOT NULL DEFAULT '{}',
    context_goal_ids uuid[] NOT NULL DEFAULT '{}',
    idempotency_key text NOT NULL,
    asked_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT directed_question_v1_thread_key_chk CHECK (char_length(thread_key) BETWEEN 1 AND 240),
    CONSTRAINT directed_question_v1_question_chk CHECK (char_length(question) BETWEEN 1 AND 8000),
    CONSTRAINT directed_question_v1_idempotency_key_chk CHECK (char_length(idempotency_key) BETWEEN 1 AND 240)
);

CREATE TABLE proxima_core.directed_answer_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    question_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    thread_key text NOT NULL,
    answer text NOT NULL,
    answered_by_personality_instance_id uuid NOT NULL REFERENCES proxima_core.personality(personality_instance_id),
    answered_by_self_perspective_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    context_memory_ids_used uuid[] NOT NULL DEFAULT '{}',
    idempotency_key text NOT NULL,
    answered_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT directed_answer_v1_thread_key_chk CHECK (char_length(thread_key) BETWEEN 1 AND 240),
    CONSTRAINT directed_answer_v1_answer_chk CHECK (char_length(answer) BETWEEN 1 AND 12000),
    CONSTRAINT directed_answer_v1_idempotency_key_chk CHECK (char_length(idempotency_key) BETWEEN 1 AND 240)
);

CREATE INDEX idx_directed_question_v1_target
    ON proxima_core.directed_question_v1 (target_personality_instance_id, asked_at DESC);

CREATE INDEX idx_directed_question_v1_thread
    ON proxima_core.directed_question_v1 (thread_key, asked_at DESC);

CREATE INDEX idx_directed_answer_v1_question
    ON proxima_core.directed_answer_v1 (question_memory_id, answered_at DESC);
