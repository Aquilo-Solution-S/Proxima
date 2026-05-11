CREATE TABLE proxima_code.workspace_review_v1 (
    memory_id                   uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    workspace_run_memory_id     uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    execution_request_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    verdict                     text NOT NULL,
    round_index                 integer NOT NULL,
    summary                     text NOT NULL,
    findings_json               jsonb NOT NULL,
    correction_instructions     text,
    verification_summary        text,
    reviewed_at                 timestamptz NOT NULL DEFAULT now(),
    created_at                  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT workspace_review_v1_verdict_chk
        CHECK (verdict IN ('approved', 'rejected', 'needs_user')),
    CONSTRAINT workspace_review_v1_round_chk
        CHECK (round_index >= 0),
    CONSTRAINT workspace_review_v1_summary_chk
        CHECK (char_length(summary) BETWEEN 1 AND 4000),
    CONSTRAINT workspace_review_v1_correction_chk
        CHECK (
            correction_instructions IS NULL
            OR char_length(correction_instructions) BETWEEN 1 AND 12000
        ),
    CONSTRAINT workspace_review_v1_verification_chk
        CHECK (
            verification_summary IS NULL
            OR char_length(verification_summary) BETWEEN 1 AND 4000
        )
);

CREATE INDEX idx_workspace_review_v1_run
    ON proxima_code.workspace_review_v1 (workspace_run_memory_id, created_at DESC);

CREATE INDEX idx_workspace_review_v1_request
    ON proxima_code.workspace_review_v1 (execution_request_memory_id, round_index);
