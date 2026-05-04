-- M5 — Code's first F→A output schema sidecar.
-- One CommitSummaryV1 Abstraction per closed source-batch (per-commit batch
-- shape per docs/01 §"The contract"). Provenance edges in
-- proxima_core.edges link this Abstraction to all Facts in its batch.

CREATE TABLE proxima_code.commit_summary_v1 (
    memory_id   uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    repo_id     uuid NOT NULL,
    commit_sha  text NOT NULL,
    summary     text NOT NULL,
    key_files   text[] NOT NULL,
    change_kind text NOT NULL
);
CREATE INDEX idx_commit_summary_v1_repo_sha
    ON proxima_code.commit_summary_v1 (repo_id, commit_sha);
