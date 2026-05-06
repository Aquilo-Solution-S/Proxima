CREATE TABLE proxima_code.development_perspective_v1 (
    memory_id            uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    repo_id              uuid,
    summary              text NOT NULL,
    pattern              text NOT NULL,
    risk                 text NOT NULL,
    recommended_posture  text NOT NULL,
    confidence           real NOT NULL CHECK (confidence >= 0 AND confidence <= 1)
);

CREATE INDEX idx_development_perspective_v1_repo
    ON proxima_code.development_perspective_v1 (repo_id);
