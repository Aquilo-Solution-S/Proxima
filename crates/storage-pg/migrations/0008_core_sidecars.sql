-- Core typed sidecars keyed by memory.t.

CREATE TABLE proxima_core.mcp_call_logged_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    tool_name text NOT NULL,
    actor_oid text NOT NULL,
    actor_upn text NOT NULL,
    ok boolean NOT NULL,
    error text,
    latency_ms bigint NOT NULL,
    io_byte_len bigint NOT NULL,
    io_truncated boolean NOT NULL,
    io_content_hash bytea NOT NULL
);

CREATE TABLE proxima_core.embeddings (
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    embedding_version int NOT NULL DEFAULT 1,
    vec vector NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    PRIMARY KEY (entity_id, model_id, embedding_version)
);

CREATE TABLE proxima_core.embedding_heads (
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    embedding_version int NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    PRIMARY KEY (entity_id, model_id)
);

CREATE TABLE proxima_core.embedding_jobs (
    job_id uuid PRIMARY KEY DEFAULT uuidv7(),
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    status text NOT NULL DEFAULT 'pending',
    UNIQUE (owner_id, entity_id, model_id)
);
