-- M3.B — proxima-code flavor sidecars.
-- See docs/03 §"Stateful Fact schemas — head-by-natural-key" and
-- docs/02 §"Re-derivation and supersession": Facts have no supersedes.
-- Stateful sidecars (file_revision_v1, code_chunk_v1) project current
-- state via head-by-natural-key queries on memories.created_at.

CREATE SCHEMA proxima_code;

----------------------------------------------------------
-- commit_v1 — append-only event sidecar (no head concept;
-- one Fact per commit). NK = none.
----------------------------------------------------------
CREATE TABLE proxima_code.commit_v1 (
    memory_id        uuid PRIMARY KEY
                       REFERENCES proxima_core.memories(memory_id),
    repo_id          uuid NOT NULL,
    sha              text NOT NULL,
    parents          text[] NOT NULL,
    author_name      text NOT NULL,
    author_email     text NOT NULL,
    author_time      timestamptz NOT NULL,
    committer_name   text NOT NULL,
    committer_email  text NOT NULL,
    committer_time   timestamptz NOT NULL,
    message          text NOT NULL
);
CREATE INDEX idx_commit_v1_repo_sha
    ON proxima_code.commit_v1 (repo_id, sha);

----------------------------------------------------------
-- file_revision_v1 — stateful Fact sidecar.
-- NK = (repo_id, file_path). Head = latest by memories.created_at
-- per NK tuple (docs/03 §Stateful Fact schemas).
----------------------------------------------------------
CREATE TABLE proxima_code.file_revision_v1 (
    memory_id            uuid PRIMARY KEY
                           REFERENCES proxima_core.memories(memory_id),
    repo_id              uuid NOT NULL,
    file_path            text NOT NULL,
    language             text,
    content_sha256       bytea NOT NULL,
    size_bytes           bigint NOT NULL,
    indexed_commit_sha   text NOT NULL,
    state                text NOT NULL,
    CONSTRAINT file_revision_v1_state_chk
        CHECK (state IN ('Present', 'Tombstone'))
);
CREATE INDEX idx_file_revision_v1_nk
    ON proxima_code.file_revision_v1 (repo_id, file_path);

----------------------------------------------------------
-- code_chunk_v1 — stateful Fact sidecar.
-- NK = (repo_id, file_path, chunk_index).
-- parent_file_revision_id is the FK to a file-revision-v1 Fact
-- emitted in the SAME indexing pass (D4 in M3-PLAN).
----------------------------------------------------------
CREATE TABLE proxima_code.code_chunk_v1 (
    memory_id                  uuid PRIMARY KEY
                                 REFERENCES proxima_core.memories(memory_id),
    repo_id                    uuid NOT NULL,
    file_path                  text NOT NULL,
    chunk_index                int NOT NULL,
    parent_file_revision_id    uuid NOT NULL
                                 REFERENCES proxima_core.memories(memory_id),
    text                       text NOT NULL,
    language                   text,
    chunk_type                 text NOT NULL,
    byte_range_start           bigint NOT NULL,
    byte_range_end             bigint NOT NULL,
    line_range_start           bigint NOT NULL,
    line_range_end             bigint NOT NULL,
    state                      text NOT NULL,
    CONSTRAINT code_chunk_v1_chunk_index_chk CHECK (chunk_index >= 0),
    CONSTRAINT code_chunk_v1_state_chk
        CHECK (state IN ('Present', 'Tombstone'))
);
CREATE INDEX idx_code_chunk_v1_nk
    ON proxima_code.code_chunk_v1 (repo_id, file_path, chunk_index);
CREATE INDEX idx_code_chunk_v1_parent
    ON proxima_code.code_chunk_v1 (parent_file_revision_id);
