ALTER TABLE proxima_code.repos
    ADD COLUMN target_branch text;

CREATE TABLE proxima_code.workspace_run_v1 (
    memory_id          uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    wake_invocation_id uuid NOT NULL UNIQUE,
    repo_id            uuid NOT NULL,
    target_branch      text NOT NULL,
    worktree_path      text NOT NULL,
    branch_name        text NOT NULL,
    parent_sha         text NOT NULL,
    head_sha           text NOT NULL,
    diff_stat_json     jsonb NOT NULL,
    exit_code          integer,
    stdout_tail        text,
    stderr_tail        text,
    duration_ms        bigint,
    created_at         timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_workspace_run_v1_repo
    ON proxima_code.workspace_run_v1 (repo_id);

CREATE TABLE proxima_code.workspace_decision_v1 (
    memory_id               uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    workspace_run_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    decision                text NOT NULL,
    decided_at              timestamptz NOT NULL DEFAULT now(),
    reason_text             text,
    decided_by_owner_id     uuid NOT NULL,
    CONSTRAINT workspace_decision_v1_decision_chk
        CHECK (decision IN ('rejected', 'accepted', 'merged'))
);

CREATE INDEX idx_workspace_decision_v1_run
    ON proxima_code.workspace_decision_v1 (workspace_run_memory_id);
