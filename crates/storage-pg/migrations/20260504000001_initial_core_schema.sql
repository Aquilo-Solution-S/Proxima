-- M2.3 — substrate tables for Postgres + outbox.
-- See docs/07-storage.md §"Core tables — abstract".

CREATE SCHEMA proxima_core;

----------------------------------------------------------
-- source_batches — F→A consolidation episode handle (01, 04).
----------------------------------------------------------
CREATE TABLE proxima_core.source_batches (
    id                       uuid PRIMARY KEY,
    source_id                text NOT NULL,
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    opened_at                timestamptz NOT NULL DEFAULT now(),
    closed_at                timestamptz,
    CONSTRAINT source_batches_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT source_batches_unique_per_source
        UNIQUE (source_id, owner_principal_kind, owner_principal_id, owner_org_id, id)
);
CREATE INDEX idx_source_batches_owner
    ON proxima_core.source_batches
       (owner_principal_kind, owner_principal_id, owner_org_id);

----------------------------------------------------------
-- cited_objects — bibliographic anchor (11). Idempotent
-- per (Owner, schema_id, content_hash).
----------------------------------------------------------
CREATE TABLE proxima_core.cited_objects (
    cited_object_id          uuid PRIMARY KEY,
    schema_id                text NOT NULL,
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    content_hash             bytea NOT NULL,
    created_at               timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT cited_objects_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT cited_objects_unique_per_owner
        UNIQUE (owner_principal_kind, owner_principal_id, owner_org_id,
                schema_id, content_hash)
);

----------------------------------------------------------
-- citation_mappings — Fact-only; one per Fact (18, 11).
----------------------------------------------------------
CREATE TABLE proxima_core.citation_mappings (
    citation_mapping_id      uuid PRIMARY KEY,
    schema_id                text NOT NULL,
    memory_id                uuid NOT NULL,
    cited_object_id          uuid NOT NULL REFERENCES proxima_core.cited_objects(cited_object_id),
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    created_at               timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT citation_mappings_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT citation_mappings_one_per_fact UNIQUE (memory_id)
);

----------------------------------------------------------
-- events — the membrane (01). event_id is ContentHash
-- BLAKE3 of (source_id, owner, payload); collisions on
-- INSERT are silent drops per docs/07 §"Content-hash dedup".
----------------------------------------------------------
CREATE TABLE proxima_core.events (
    event_id                 bytea PRIMARY KEY,
    source_id                text NOT NULL,
    source_batch_id          uuid NOT NULL REFERENCES proxima_core.source_batches(id),
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    schema_id                text NOT NULL,
    schema_version           int NOT NULL,
    observed_at              timestamptz NOT NULL,
    occurred_at              timestamptz NOT NULL,
    payload_ref              uuid,
    CONSTRAINT events_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group'))
);
CREATE INDEX idx_events_owner_observed
    ON proxima_core.events
       (owner_principal_kind, owner_principal_id, owner_org_id, observed_at DESC);
CREATE INDEX idx_events_source_batch
    ON proxima_core.events (source_batch_id);

----------------------------------------------------------
-- memories — F/A/P with kind-discriminating columns (07).
-- Strict CHECK enforces Fact-vs-Derived shape (07
-- §"Core tables — abstract").
----------------------------------------------------------
CREATE TABLE proxima_core.memories (
    memory_id                uuid PRIMARY KEY,
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    schema_id                text NOT NULL,
    created_at               timestamptz NOT NULL DEFAULT now(),

    -- Fact variant:
    event_id                 bytea REFERENCES proxima_core.events(event_id),
    citation_mapping_id      uuid REFERENCES proxima_core.citation_mappings(citation_mapping_id),

    -- Derived (Abstraction | Perspective) variant:
    kind                     text,
    text                     text,
    operator_kind            text,
    model_id                 text,
    prompt_version           text,
    personality_id           text,
    personality_state_hash   bytea,

    supersedes               uuid REFERENCES proxima_core.memories(memory_id),

    CONSTRAINT memories_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT memories_kind_values_chk
        CHECK (kind IS NULL OR kind IN ('Abstraction', 'Perspective')),
    CONSTRAINT memories_operator_kind_values_chk
        CHECK (operator_kind IS NULL OR operator_kind IN ('FtoA', 'AtoP')),
    CONSTRAINT memories_variant_chk CHECK (
        (
            -- Fact: immutable per the trauma test (02 §Re-derivation
            -- and supersession). `supersedes IS NULL` enforces that
            -- "current state" projections route through head-by-natural-key
            -- queries on the schema sidecar (03 §Stateful Fact schemas),
            -- never via lineage replacement.
            event_id IS NOT NULL AND citation_mapping_id IS NOT NULL
            AND kind IS NULL AND text IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND personality_id IS NULL AND personality_state_hash IS NULL
            AND supersedes IS NULL
        ) OR (
            -- Derived:
            kind IS NOT NULL AND text IS NOT NULL AND operator_kind IS NOT NULL
            AND model_id IS NOT NULL AND prompt_version IS NOT NULL
            AND personality_id IS NOT NULL AND personality_state_hash IS NOT NULL
            AND event_id IS NULL AND citation_mapping_id IS NULL
        )
    ),
    CONSTRAINT memories_one_fact_per_event UNIQUE (event_id)
);
CREATE INDEX idx_memories_owner_kind
    ON proxima_core.memories
       (owner_principal_kind, owner_principal_id, owner_org_id, kind);
CREATE INDEX idx_memories_supersedes
    ON proxima_core.memories (supersedes)
       WHERE supersedes IS NOT NULL;

-- Now backfill the citation_mappings.memory_id FK (forward ref).
ALTER TABLE proxima_core.citation_mappings
    ADD CONSTRAINT citation_mappings_memory_fk
    FOREIGN KEY (memory_id)
    REFERENCES proxima_core.memories(memory_id);

----------------------------------------------------------
-- goals — distinct entity (06, 11). Authorship
-- discrimination per docs/07.
----------------------------------------------------------
CREATE TABLE proxima_core.goals (
    goal_id                  uuid PRIMARY KEY,
    schema_id                text NOT NULL,
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    text                     text NOT NULL,
    state                    text NOT NULL,
    supersedes               uuid REFERENCES proxima_core.goals(goal_id),
    authorship_kind          text NOT NULL,
    authorship_origin        text,
    authorship_operator_id   uuid,
    authorship_tool_id       text,
    operator_kind            text,
    model_id                 text,
    prompt_version           text,
    personality_id           text,
    personality_state_hash   bytea,
    created_at               timestamptz NOT NULL DEFAULT now(),
    request_id               text NOT NULL,

    CONSTRAINT goals_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT goals_state_chk
        CHECK (state IN ('Active', 'Paused', 'Achieved', 'Abandoned')),
    CONSTRAINT goals_authorship_kind_chk
        CHECK (authorship_kind IN ('User', 'System', 'External')),
    CONSTRAINT goals_authorship_origin_chk
        CHECK (authorship_origin IS NULL OR authorship_origin IN ('Operator', 'Tool')),
    CONSTRAINT goals_operator_kind_chk
        CHECK (operator_kind IS NULL OR operator_kind = 'AtoGoal'),
    CONSTRAINT goals_authorship_shape_chk CHECK (
        (
            -- User:
            authorship_kind = 'User'
            AND authorship_origin IS NULL AND authorship_operator_id IS NULL
            AND authorship_tool_id IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND personality_id IS NULL AND personality_state_hash IS NULL
        ) OR (
            -- System / Operator:
            authorship_kind = 'System' AND authorship_origin = 'Operator'
            AND authorship_operator_id IS NOT NULL
            AND operator_kind IS NOT NULL AND model_id IS NOT NULL
            AND prompt_version IS NOT NULL AND personality_id IS NOT NULL
            AND personality_state_hash IS NOT NULL
            AND authorship_tool_id IS NULL
        ) OR (
            -- System / Tool:
            authorship_kind = 'System' AND authorship_origin = 'Tool'
            AND authorship_tool_id IS NOT NULL
            AND authorship_operator_id IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND personality_id IS NULL AND personality_state_hash IS NULL
        ) OR (
            -- External:
            authorship_kind = 'External'
            AND authorship_origin IS NULL AND authorship_operator_id IS NULL
            AND authorship_tool_id IS NULL AND operator_kind IS NULL
            AND model_id IS NULL AND prompt_version IS NULL
            AND personality_id IS NULL AND personality_state_hash IS NULL
        )
    ),
    CONSTRAINT goals_request_id_idem UNIQUE
        (owner_principal_kind, owner_principal_id, owner_org_id, request_id)
);
CREATE INDEX idx_goals_owner_state
    ON proxima_core.goals
       (owner_principal_kind, owner_principal_id, owner_org_id, state);
CREATE INDEX idx_goals_supersedes
    ON proxima_core.goals (supersedes)
       WHERE supersedes IS NOT NULL;

----------------------------------------------------------
-- goal_parents — DAG (06).
----------------------------------------------------------
CREATE TABLE proxima_core.goal_parents (
    goal_id        uuid NOT NULL REFERENCES proxima_core.goals(goal_id),
    parent_goal_id uuid NOT NULL REFERENCES proxima_core.goals(goal_id),
    PRIMARY KEY (goal_id, parent_goal_id),
    CONSTRAINT goal_parents_no_self CHECK (goal_id <> parent_goal_id)
);

----------------------------------------------------------
-- change_event — protocol-level outbox (14 §Consistency,
-- 14 §Subscribe).
----------------------------------------------------------
CREATE TABLE proxima_core.change_event (
    seq                       uuid PRIMARY KEY,
    owner_principal_kind      text NOT NULL,
    owner_principal_id        uuid NOT NULL,
    owner_org_id              uuid NOT NULL,
    kind                      text NOT NULL,

    -- EntityAppend columns:
    entity_kind               text,
    entity_memory_id          uuid REFERENCES proxima_core.memories(memory_id),
    entity_goal_id            uuid REFERENCES proxima_core.goals(goal_id),
    entity_schema_id          text,
    entity_schema_version     int,
    entity_personality_id     text,
    supersedes_memory_id      uuid,
    supersedes_goal_id        uuid,

    -- EdgeAppend columns (set when M3+ adds edges; NULL in M2):
    edge_id                   uuid,
    edge_relation             text,
    edge_source_kind          text,
    edge_source_memory_id     uuid,
    edge_source_goal_id       uuid,
    edge_target_kind          text,
    edge_target_memory_id     uuid,
    edge_target_goal_id       uuid,

    CONSTRAINT change_event_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT change_event_kind_chk
        CHECK (kind IN ('EntityAppend', 'EdgeAppend')),
    CONSTRAINT change_event_entity_kind_chk
        CHECK (entity_kind IS NULL OR entity_kind IN ('Fact', 'Abstraction', 'Perspective', 'Goal')),
    CONSTRAINT change_event_edge_source_kind_chk
        CHECK (edge_source_kind IS NULL OR edge_source_kind IN ('Fact', 'Abstraction', 'Perspective', 'Goal')),
    CONSTRAINT change_event_edge_target_kind_chk
        CHECK (edge_target_kind IS NULL OR edge_target_kind IN ('Fact', 'Abstraction', 'Perspective', 'Goal')),
    CONSTRAINT change_event_shape_chk CHECK (
        (
            -- EntityAppend:
            kind = 'EntityAppend'
            AND entity_kind IS NOT NULL AND entity_schema_id IS NOT NULL
            AND ( (entity_memory_id IS NOT NULL) <> (entity_goal_id IS NOT NULL) )
            AND edge_id IS NULL AND edge_relation IS NULL
            AND edge_source_kind IS NULL AND edge_source_memory_id IS NULL AND edge_source_goal_id IS NULL
            AND edge_target_kind IS NULL AND edge_target_memory_id IS NULL AND edge_target_goal_id IS NULL
        ) OR (
            -- EdgeAppend (M3+; constraint is here so the column shape is locked):
            kind = 'EdgeAppend'
            AND edge_id IS NOT NULL AND edge_relation IS NOT NULL
            AND edge_source_kind IS NOT NULL AND edge_target_kind IS NOT NULL
            AND ( (edge_source_memory_id IS NOT NULL) <> (edge_source_goal_id IS NOT NULL) )
            AND ( (edge_target_memory_id IS NOT NULL) <> (edge_target_goal_id IS NOT NULL) )
            AND entity_kind IS NULL AND entity_memory_id IS NULL AND entity_goal_id IS NULL
            AND entity_schema_id IS NULL AND entity_schema_version IS NULL
            AND entity_personality_id IS NULL
            AND supersedes_memory_id IS NULL AND supersedes_goal_id IS NULL
        )
    )
);
CREATE INDEX idx_change_event_owner_seq
    ON proxima_core.change_event
       (owner_principal_kind, owner_principal_id, owner_org_id, seq);
