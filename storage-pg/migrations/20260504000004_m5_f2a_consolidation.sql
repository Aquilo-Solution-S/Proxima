-- M5 — F→A consolidation surfaces.
--
-- See docs/04 §"Source-batch lifecycle" (per-(batch, operator) tracking),
-- docs/02 §"Edges" (provenance edges), docs/07 §"Vector store"
-- (embeddings).
--
-- Embeddings stored as float4[] in M5; a future migration swaps to
-- pgvector's vector(N) plus an HNSW index when A→P retrieval lands.
-- Table shape is stable across that swap — only the column type
-- changes.

----------------------------------------------------------
-- source_batch_f2a — per-(batch, operator) F→A run gate.
-- docs/04 §"Source-batch lifecycle".
--
-- prompt_version + head_memory_id are M5 ergonomic additions on top
-- of the doc'd PK shape — they let the caller observe which prompt
-- last ran and walk the supersession lineage without an extra query.
-- A prompt_version bump turning into a supersession-emitting re-run
-- is M6 dispatcher work; M5 returns `already_consolidated = true`
-- on any (batch_id, operator_id) collision.
----------------------------------------------------------
CREATE TABLE proxima_core.source_batch_f2a (
    batch_id        uuid NOT NULL REFERENCES proxima_core.source_batches(id),
    operator_id     text NOT NULL,
    prompt_version  text NOT NULL,
    head_memory_id  uuid REFERENCES proxima_core.memories(memory_id),
    run_at          timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (batch_id, operator_id)
);

----------------------------------------------------------
-- edges — provenance + structural + interpretive (docs/02
-- §"Edges"). M5 lights up the Provenance class only (A→F,
-- authored by F→A operators). Other classes follow as their
-- operators land. The table shape is final at v1.
--
-- authorship_owner_memory_id is the source A/P that "owns"
-- the edge top-down for OperatorFtoA / OperatorAtoP /
-- PerspectiveLink. NULL for User / EventSource / Engine.
----------------------------------------------------------
CREATE TABLE proxima_core.edges (
    edge_id              uuid PRIMARY KEY,
    relation             text NOT NULL,
    relation_class       text NOT NULL,

    source_kind          text NOT NULL,
    source_memory_id     uuid REFERENCES proxima_core.memories(memory_id),
    source_goal_id       uuid REFERENCES proxima_core.goals(goal_id),

    target_kind          text NOT NULL,
    target_memory_id     uuid REFERENCES proxima_core.memories(memory_id),
    target_goal_id       uuid REFERENCES proxima_core.goals(goal_id),

    authorship_kind             text NOT NULL,
    authorship_owner_memory_id  uuid REFERENCES proxima_core.memories(memory_id),

    owner_principal_kind text NOT NULL,
    owner_principal_id   uuid NOT NULL,
    owner_org_id         uuid NOT NULL,
    created_at           timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT edges_principal_kind_chk
        CHECK (owner_principal_kind IN ('User','Group')),
    CONSTRAINT edges_source_kind_chk
        CHECK (source_kind IN ('Fact','Abstraction','Perspective','Goal')),
    CONSTRAINT edges_target_kind_chk
        CHECK (target_kind IN ('Fact','Abstraction','Perspective','Goal')),
    CONSTRAINT edges_relation_class_chk
        CHECK (relation_class IN (
            'Provenance','Structural','Causal','Interpretive','Supersession'
        )),
    CONSTRAINT edges_authorship_kind_chk
        CHECK (authorship_kind IN (
            'EventSource','OperatorFtoA','OperatorAtoP','OperatorAtoGoal',
            'PerspectiveLink','User','Engine'
        )),
    CONSTRAINT edges_source_endpoint_chk
        CHECK ((source_memory_id IS NOT NULL) <> (source_goal_id IS NOT NULL)),
    CONSTRAINT edges_target_endpoint_chk
        CHECK ((target_memory_id IS NOT NULL) <> (target_goal_id IS NOT NULL))
);
CREATE INDEX idx_edges_source_memory ON proxima_core.edges (source_memory_id)
    WHERE source_memory_id IS NOT NULL;
CREATE INDEX idx_edges_target_memory ON proxima_core.edges (target_memory_id)
    WHERE target_memory_id IS NOT NULL;
CREATE INDEX idx_edges_relation ON proxima_core.edges (relation);
CREATE INDEX idx_edges_owner ON proxima_core.edges
    (owner_principal_kind, owner_principal_id, owner_org_id);

----------------------------------------------------------
-- embeddings — vector store (docs/07 §"Vector store").
-- M5 ships float4[] without a similarity index; M6 swaps the
-- vec column to pgvector's vector(dim) and adds HNSW.
----------------------------------------------------------
CREATE TABLE proxima_core.embeddings (
    entity_kind          text NOT NULL,
    entity_id            uuid NOT NULL,
    embedding_version    int NOT NULL DEFAULT 1,
    model_id             text NOT NULL,
    vec                  float4[] NOT NULL,
    dim                  int NOT NULL,
    owner_principal_kind text NOT NULL,
    owner_principal_id   uuid NOT NULL,
    owner_org_id         uuid NOT NULL,
    created_at           timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (entity_kind, entity_id, embedding_version, model_id),
    CONSTRAINT embeddings_principal_kind_chk
        CHECK (owner_principal_kind IN ('User','Group')),
    CONSTRAINT embeddings_entity_kind_chk
        CHECK (entity_kind IN ('Fact','Abstraction','Perspective','Goal'))
);
CREATE INDEX idx_embeddings_owner ON proxima_core.embeddings
    (owner_principal_kind, owner_principal_id, owner_org_id);
