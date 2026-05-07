CREATE TABLE IF NOT EXISTS proxima_code.commit_summarizer_self_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    display_name text NOT NULL,
    purpose text NOT NULL
);

CREATE TABLE IF NOT EXISTS proxima_code.engineer_self_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    display_name text NOT NULL,
    purpose text NOT NULL
);
