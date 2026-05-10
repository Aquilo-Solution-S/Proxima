CREATE TABLE proxima_code.execution_request_v1 (
    memory_id    uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    repo_id      uuid NOT NULL,
    title        text NOT NULL,
    instructions text NOT NULL,
    request_key  text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT execution_request_v1_title_chk
        CHECK (char_length(title) BETWEEN 1 AND 240),
    CONSTRAINT execution_request_v1_instructions_chk
        CHECK (char_length(instructions) BETWEEN 1 AND 20000),
    CONSTRAINT execution_request_v1_request_key_chk
        CHECK (char_length(request_key) BETWEEN 1 AND 240)
);

CREATE UNIQUE INDEX idx_execution_request_v1_repo_key
    ON proxima_code.execution_request_v1 (repo_id, request_key);

CREATE INDEX idx_execution_request_v1_repo
    ON proxima_code.execution_request_v1 (repo_id);
