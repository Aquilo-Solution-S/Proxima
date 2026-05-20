ALTER TABLE proxima_core.personality_wake_entries
    ADD COLUMN workspace_binding jsonb;

ALTER TABLE proxima_core.personality_wake_entries
    ADD CONSTRAINT personality_wake_entries_workspace_binding_mode_chk
    CHECK (workspace_binding IS NULL OR execution_mode = 'workspace');

CREATE TABLE proxima_core.workspace_run_v1 (
    memory_id uuid NOT NULL,
    wake_invocation_id uuid NOT NULL,
    wake_entry_id uuid NOT NULL,
    personality_instance_id uuid NOT NULL,
    binding_kind text NOT NULL,
    finalize text NOT NULL,
    repo_path text NOT NULL,
    base_ref text NOT NULL,
    worktree_path text NOT NULL,
    branch_name text NOT NULL,
    parent_sha text NOT NULL,
    head_sha text NOT NULL,
    committed boolean NOT NULL,
    diff_stat_json jsonb NOT NULL,
    exit_code integer,
    stdout_tail text,
    stderr_tail text,
    duration_ms bigint,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT workspace_run_v1_binding_kind_chk CHECK (char_length(binding_kind) >= 1),
    CONSTRAINT workspace_run_v1_finalize_chk CHECK (char_length(finalize) >= 1),
    CONSTRAINT workspace_run_v1_repo_path_chk CHECK (char_length(repo_path) >= 1),
    CONSTRAINT workspace_run_v1_base_ref_chk CHECK (char_length(base_ref) >= 1),
    CONSTRAINT workspace_run_v1_worktree_path_chk CHECK (char_length(worktree_path) >= 1),
    CONSTRAINT workspace_run_v1_branch_name_chk CHECK (char_length(branch_name) >= 1),
    CONSTRAINT workspace_run_v1_parent_sha_chk CHECK (char_length(parent_sha) >= 1),
    CONSTRAINT workspace_run_v1_head_sha_chk CHECK (char_length(head_sha) >= 1),
    CONSTRAINT workspace_run_v1_duration_chk CHECK (duration_ms IS NULL OR duration_ms >= 0)
);

ALTER TABLE ONLY proxima_core.workspace_run_v1
    ADD CONSTRAINT workspace_run_v1_pkey PRIMARY KEY (memory_id);

ALTER TABLE ONLY proxima_core.workspace_run_v1
    ADD CONSTRAINT workspace_run_v1_wake_invocation_id_key UNIQUE (wake_invocation_id);

ALTER TABLE ONLY proxima_core.workspace_run_v1
    ADD CONSTRAINT workspace_run_v1_memory_id_fkey
    FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);

CREATE INDEX workspace_run_v1_personality_idx
    ON proxima_core.workspace_run_v1 USING btree (personality_instance_id, created_at DESC);
