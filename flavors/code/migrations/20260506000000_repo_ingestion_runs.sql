-- M6.S3 - code flavor ingestion runs.
-- Persisted run lifecycle for repo ingestion. One row per attempted
-- ingest. At most one (queued, running) row per (owner, repo_id);
-- enforced by the partial unique index below.

CREATE TABLE proxima_code.repo_ingestion_runs (
    run_id                    uuid PRIMARY KEY,
    owner_principal_kind      text NOT NULL,
    owner_principal_id        uuid NOT NULL,
    owner_org_id              uuid NOT NULL,
    repo_id                   uuid NOT NULL,

    status                    text NOT NULL,
    stage                     text NOT NULL,

    commits_emitted           integer NOT NULL DEFAULT 0,
    files_emitted             integer NOT NULL DEFAULT 0,
    chunks_emitted            integer NOT NULL DEFAULT 0,
    chunks_reused             integer NOT NULL DEFAULT 0,
    chunks_tombstoned         integer NOT NULL DEFAULT 0,
    ast_edges_emitted         integer NOT NULL DEFAULT 0,
    abstractions_emitted      integer NOT NULL DEFAULT 0,
    embeddings_landed         integer NOT NULL DEFAULT 0,
    citations_emitted         integer NOT NULL DEFAULT 0,

    error_message             text,

    started_at                timestamptz NOT NULL DEFAULT now(),
    updated_at                timestamptz NOT NULL DEFAULT now(),
    finished_at               timestamptz,

    CONSTRAINT runs_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT runs_status_chk
        CHECK (status IN ('queued', 'running', 'succeeded', 'failed')),
    CONSTRAINT runs_stage_chk
        CHECK (stage IN ('starting', 'facts', 'ast_edges', 'f2a', 'embeddings', 'done')),
    CONSTRAINT runs_finished_when_terminal_chk
        CHECK (
            (status IN ('succeeded', 'failed') AND finished_at IS NOT NULL)
            OR
            (status IN ('queued', 'running') AND finished_at IS NULL)
        ),
    CONSTRAINT runs_repo_fk
        FOREIGN KEY (owner_principal_kind, owner_principal_id, owner_org_id, repo_id)
        REFERENCES proxima_code.repos
            (owner_principal_kind, owner_principal_id, owner_org_id, repo_id)
        ON DELETE CASCADE
);

CREATE INDEX repo_ingestion_runs_by_repo
    ON proxima_code.repo_ingestion_runs
    (owner_principal_kind, owner_principal_id, owner_org_id, repo_id, started_at DESC);

-- Concurrency guard for repo_ingest_start.
CREATE UNIQUE INDEX repo_ingestion_runs_one_active
    ON proxima_code.repo_ingestion_runs
    (owner_principal_kind, owner_principal_id, owner_org_id, repo_id)
    WHERE status IN ('queued', 'running');
