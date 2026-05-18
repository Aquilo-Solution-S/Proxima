CREATE TYPE proxima_code.acceptance_verifier_kind AS ENUM (
    'file_exists',
    'command',
    'browser_smoke',
    'diff_scope',
    'reviewer_only'
);

CREATE TYPE proxima_code.verification_evidence_status AS ENUM (
    'passed',
    'failed',
    'skipped'
);

CREATE TABLE proxima_code.acceptance_criteria_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    execution_request_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    criteria_json jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT acceptance_criteria_v1_nonempty_chk CHECK (
        jsonb_typeof(criteria_json) = 'array'
        AND jsonb_array_length(criteria_json) > 0
    )
);

CREATE INDEX idx_acceptance_criteria_v1_request
    ON proxima_code.acceptance_criteria_v1 (execution_request_memory_id);

CREATE TABLE proxima_code.verification_evidence_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    workspace_run_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    execution_request_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    criterion_key text NOT NULL,
    status proxima_code.verification_evidence_status NOT NULL,
    summary text NOT NULL,
    artifact_refs_json jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT verification_evidence_v1_criterion_key_chk CHECK (
        char_length(criterion_key) >= 1
        AND char_length(criterion_key) <= 80
    ),
    CONSTRAINT verification_evidence_v1_summary_chk CHECK (
        char_length(summary) >= 1
        AND char_length(summary) <= 4000
    ),
    CONSTRAINT verification_evidence_v1_artifacts_object_chk CHECK (
        jsonb_typeof(artifact_refs_json) = 'object'
    )
);

CREATE INDEX idx_verification_evidence_v1_request
    ON proxima_code.verification_evidence_v1 (
        execution_request_memory_id,
        criterion_key,
        status
    );

CREATE INDEX idx_verification_evidence_v1_run
    ON proxima_code.verification_evidence_v1 (
        workspace_run_memory_id,
        criterion_key,
        status
    );
