CREATE TABLE proxima_code.test_request_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    repo_id uuid NOT NULL,
    title text NOT NULL,
    instructions text NOT NULL,
    test_key text NOT NULL,
    criteria_json jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT test_request_v1_title_chk CHECK (
        char_length(title) >= 1
        AND char_length(title) <= 240
    ),
    CONSTRAINT test_request_v1_instructions_chk CHECK (
        char_length(instructions) >= 1
        AND char_length(instructions) <= 20000
    ),
    CONSTRAINT test_request_v1_test_key_chk CHECK (
        char_length(test_key) >= 1
        AND char_length(test_key) <= 240
    ),
    CONSTRAINT test_request_v1_criteria_nonempty_chk CHECK (
        jsonb_typeof(criteria_json) = 'array'
        AND jsonb_array_length(criteria_json) > 0
    )
);

CREATE INDEX idx_test_request_v1_repo
    ON proxima_code.test_request_v1 (repo_id);

CREATE UNIQUE INDEX idx_test_request_v1_repo_key
    ON proxima_code.test_request_v1 (repo_id, test_key);
