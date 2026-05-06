CREATE SCHEMA IF NOT EXISTS proxima_mcp;

CREATE TABLE proxima_mcp.agent_note_v1 (
    memory_id        uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    note_id          uuid NOT NULL,
    title            text NOT NULL,
    body             text NOT NULL,
    tags             text[] NOT NULL,
    idempotency_key  text,
    CONSTRAINT agent_note_v1_title_nonempty CHECK (length(btrim(title)) > 0),
    CONSTRAINT agent_note_v1_body_nonempty  CHECK (length(btrim(body))  > 0)
);
CREATE INDEX idx_agent_note_v1_note_id
    ON proxima_mcp.agent_note_v1 (note_id);
CREATE INDEX idx_agent_note_v1_search
    ON proxima_mcp.agent_note_v1
    USING gin (to_tsvector('simple', title || ' ' || body));

CREATE TABLE proxima_mcp.agent_derivation_v1 (
    memory_id          uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    title              text NOT NULL,
    body               text NOT NULL,
    tags               text[] NOT NULL,
    idempotency_key    text,
    source_memory_ids  uuid[] NOT NULL,
    model_id           text NOT NULL,
    client_name        text NOT NULL,
    client_version     text NOT NULL,
    CONSTRAINT agent_derivation_v1_title_nonempty CHECK (length(btrim(title)) > 0),
    CONSTRAINT agent_derivation_v1_body_nonempty  CHECK (length(btrim(body))  > 0)
);
CREATE INDEX idx_agent_derivation_v1_search
    ON proxima_mcp.agent_derivation_v1
    USING gin (to_tsvector('simple', title || ' ' || body));

CREATE TABLE proxima_mcp.agent_link_v1 (
    edge_id     uuid PRIMARY KEY REFERENCES proxima_core.edges(edge_id),
    reason      text NOT NULL,
    confidence  smallint NOT NULL,
    CONSTRAINT agent_link_v1_reason_nonempty CHECK (length(btrim(reason)) > 0),
    CONSTRAINT agent_link_v1_confidence_chk  CHECK (confidence BETWEEN 0 AND 100)
);
