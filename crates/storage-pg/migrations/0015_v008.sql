-- Proxima core schema — v0.0.8 edge lane (version 15).
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline (sqlx checksum-pinned, NEVER
-- edit). 0008..0014 are the prior append-only lanes.
--
-- This lane RESETS the edge layer in the spirit of the v0.0.4 reset: the
-- edges table is replaced, not evolved (docs/16-edges.md §Storage Migration).
-- Nothing is carried over. Origin rows come back the moment a node write
-- declares what it was made from; reference rows come back with re-ingest.
-- MIN_CORE_MIGRATION_VERSION bumps to 15, so a database one lane behind the
-- binary fails at boot rather than at first query.
--
-- The thesis this implements: an edge carries no information beyond its
-- existence — its endpoints, its direction, its creation time, and its kind.
-- All content lives in nodes.

-- ---------------------------------------------------------------------------
-- Out: the two-layered relation model.
--
-- The old table carried an id, a relation string, a relation class, an
-- authorship kind, an authorship owner, three endpoint columns per side and a
-- typed sidecar. Every one of them was either content that belongs in a node
-- or metadata that belongs on a row. `agent_link_v1` — the one core edge
-- sidecar — held a reason and a confidence, which is a judgment, which is a
-- Perspective: it comes back as `interpretation_v1` below.
-- ---------------------------------------------------------------------------
DROP TABLE IF EXISTS proxima_core.agent_link_v1;
DROP TABLE IF EXISTS proxima_core.compliance_edge_target_redactions;
DROP TABLE IF EXISTS proxima_core.edges;
DROP FUNCTION IF EXISTS proxima_core.validate_edge_invariants();
DROP TYPE IF EXISTS proxima_core.relation_class;
DROP TYPE IF EXISTS proxima_core.edge_authorship_kind;

-- ---------------------------------------------------------------------------
-- In: two kinds, and the enum is not extensible.
--
-- A feature that seems to need a third kind fails the node-home test and is
-- missing a node, not a kind.
-- ---------------------------------------------------------------------------
CREATE TYPE proxima_core.edge_kind AS ENUM (
    'origin',
    'reference'
);

COMMENT ON TYPE proxima_core.edge_kind IS
  'What an edge IS. origin: a node declared what it was made from. reference: a schema-declared payload field points at another node. The kind is a consequence of the write that produced the row, never a choice the writer makes.';

-- The endpoint kind is also the address form, which is also the binding: a
-- FactEntityHead endpoint follows the head as it is re-observed, every other
-- endpoint pins one row. That is where the old descriptor's FollowHead/Pin
-- cell went — into the address itself, so the two cannot disagree.
CREATE TYPE proxima_core.edge_endpoint_kind AS ENUM (
    'Fact',
    'Abstraction',
    'Perspective',
    'Goal',
    'FactEntityHead'
);

COMMENT ON TYPE proxima_core.edge_endpoint_kind IS
  'One end of an edge: the entity kind and, in the same value, the address form. Fact/Abstraction/Perspective address a memories row, Goal a goals row, FactEntityHead a fact_entities row (follow-head). Superset of entity_kind because the address form is part of what the endpoint is.';

CREATE FUNCTION proxima_core.edge_endpoint_layer(kind proxima_core.edge_endpoint_kind)
RETURNS integer
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$
    SELECT CASE kind
        WHEN 'Fact'::proxima_core.edge_endpoint_kind THEN 0
        WHEN 'FactEntityHead'::proxima_core.edge_endpoint_kind THEN 0
        WHEN 'Abstraction'::proxima_core.edge_endpoint_kind THEN 1
        WHEN 'Perspective'::proxima_core.edge_endpoint_kind THEN 2
        ELSE NULL
    END;
$$;

COMMENT ON FUNCTION proxima_core.edge_endpoint_layer(proxima_core.edge_endpoint_kind) IS
  'F/A/P layer index; NULL for Goal, which sits outside the layer comparison (docs/16 §Direction and layering).';

CREATE FUNCTION proxima_core.edge_endpoint_entity_kind(kind proxima_core.edge_endpoint_kind)
RETURNS proxima_core.entity_kind
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$
    SELECT CASE kind
        WHEN 'FactEntityHead'::proxima_core.edge_endpoint_kind
            THEN 'Fact'::proxima_core.entity_kind
        ELSE kind::text::proxima_core.entity_kind
    END;
$$;

-- ---------------------------------------------------------------------------
-- The edge table is an index.
--
-- No edge_id: rows have no identity beyond their content, so idempotency is
-- structural — replaying any write re-asserts the same primary key. The
-- v0.0.7 identity-hash scheme (BLAKE3-derived v8 ids under a NULLS NOT
-- DISTINCT partial unique index) existed to approximate what this table has
-- by construction.
--
-- No payload, no sidecar, no citation, no status. A connection that needs to
-- say more than "these two, this way, since then" is a node.
--
-- Multiplicity collapses: ten call sites from chunk A to chunk B are one row
-- here and ten entries in A's payload.
-- ---------------------------------------------------------------------------
CREATE TABLE proxima_core.edges (
    source_kind proxima_core.edge_endpoint_kind NOT NULL,
    source_id uuid NOT NULL,
    target_kind proxima_core.edge_endpoint_kind NOT NULL,
    target_id uuid NOT NULL,
    kind proxima_core.edge_kind NOT NULL,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT edges_pkey PRIMARY KEY (source_kind, source_id, target_kind, target_id, kind),
    CONSTRAINT edges_no_self_loop_chk
        CHECK (NOT (source_kind = target_kind AND source_id = target_id)),
    CONSTRAINT edges_layering_chk CHECK (
        proxima_core.edge_endpoint_layer(source_kind) IS NULL
        OR proxima_core.edge_endpoint_layer(target_kind) IS NULL
        OR proxima_core.edge_endpoint_layer(source_kind)
           >= proxima_core.edge_endpoint_layer(target_kind)
    ),
    CONSTRAINT edges_owner_ref_shape_chk CHECK (
        (owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL)
        OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind,
                           'group'::proxima_core.owner_ref_kind)
            AND owner_id IS NOT NULL)
    ),
    CONSTRAINT edges_world_not_write_owner_chk
        CHECK (owner_kind <> 'world'::proxima_core.owner_ref_kind)
);

COMMENT ON TABLE proxima_core.edges IS
  'The connection index. One row per (source, target, kind); the row IS its identity, so a replayed write re-asserts it instead of minting a duplicate. Owned by the source owner, always. Rebuildable: dropping this table and re-deriving it from node content yields the same set — that is the master invariant, and every other guarantee is a corollary. See docs/16-edges.md.';

COMMENT ON COLUMN proxima_core.edges.kind IS
  'origin (the source declared what it was made from) or reference (a schema-declared payload field of the source points here). Consequent, never chosen.';

CREATE INDEX idx_edges_owner_created ON proxima_core.edges
    USING btree (owner_kind, owner_id, created_at DESC);
CREATE INDEX idx_edges_source ON proxima_core.edges
    USING btree (source_id, source_kind);
CREATE INDEX idx_edges_target ON proxima_core.edges
    USING btree (target_id, target_kind);
CREATE INDEX idx_edges_origin_target ON proxima_core.edges
    USING btree (target_id) WHERE (kind = 'origin'::proxima_core.edge_kind);

-- Resolve one endpoint address to (its actual kind, its owner). No row means
-- the endpoint does not exist, which is how the trigger below spells E1.
CREATE FUNCTION proxima_core.edge_endpoint_row(
    endpoint_kind proxima_core.edge_endpoint_kind,
    endpoint_id uuid
)
RETURNS TABLE (
    actual_kind proxima_core.edge_endpoint_kind,
    owner_kind proxima_core.owner_ref_kind,
    owner_id uuid
)
    LANGUAGE sql STABLE
    AS $$
    SELECT CASE
               WHEN m.kind IS NULL THEN 'Fact'::proxima_core.edge_endpoint_kind
               ELSE m.kind::text::proxima_core.edge_endpoint_kind
           END,
           m.owner_kind,
           m.owner_id
      FROM proxima_core.memories m
     WHERE endpoint_kind <> 'Goal'::proxima_core.edge_endpoint_kind
       AND endpoint_kind <> 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND m.memory_id = endpoint_id
    UNION ALL
    SELECT 'Goal'::proxima_core.edge_endpoint_kind, g.owner_kind, g.owner_id
      FROM proxima_core.goals g
     WHERE endpoint_kind = 'Goal'::proxima_core.edge_endpoint_kind
       AND g.goal_id = endpoint_id
    UNION ALL
    SELECT 'FactEntityHead'::proxima_core.edge_endpoint_kind, fe.owner_kind, fe.owner_id
      FROM proxima_core.fact_entities fe
     WHERE endpoint_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND fe.fact_entity_id = endpoint_id
$$;

-- Existence, ownership and endpoint-kind agreement (Lean E1/E2). Layering
-- (E3) and the self-loop refusal are CHECK constraints above — they read only
-- the row. This trigger is what needs the endpoint tables.
CREATE FUNCTION proxima_core.validate_edge_invariants() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    source_actual proxima_core.edge_endpoint_kind;
    source_owner_kind proxima_core.owner_ref_kind;
    source_owner_id uuid;
    target_actual proxima_core.edge_endpoint_kind;
BEGIN
    SELECT actual_kind, owner_kind, owner_id
      INTO source_actual, source_owner_kind, source_owner_id
      FROM proxima_core.edge_endpoint_row(NEW.source_kind, NEW.source_id);
    IF source_actual IS NULL THEN
        RAISE EXCEPTION 'edge: source endpoint not found';
    END IF;
    IF source_actual <> NEW.source_kind THEN
        RAISE EXCEPTION 'edge: source kind % does not match endpoint kind %',
            NEW.source_kind, source_actual;
    END IF;

    SELECT actual_kind INTO target_actual
      FROM proxima_core.edge_endpoint_row(NEW.target_kind, NEW.target_id);
    IF target_actual IS NULL THEN
        RAISE EXCEPTION 'edge: target endpoint not found';
    END IF;
    IF target_actual <> NEW.target_kind THEN
        RAISE EXCEPTION 'edge: target kind % does not match endpoint kind %',
            NEW.target_kind, target_actual;
    END IF;

    IF source_owner_kind <> NEW.owner_kind
       OR source_owner_id IS DISTINCT FROM NEW.owner_id THEN
        RAISE EXCEPTION 'edge: owner is not the source owner';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER edges_invariant_check
    BEFORE INSERT OR UPDATE ON proxima_core.edges
    FOR EACH ROW EXECUTE FUNCTION proxima_core.validate_edge_invariants();

-- ---------------------------------------------------------------------------
-- Supersession is a pointer, not a connection.
--
-- It is the same thing persisting through revision, so it lives on the rows:
-- the successor's `supersedes` (already there since the baseline) and the
-- predecessor's `superseded_by`. Both are written in the successor's own
-- transaction, and NO edge row is written for a supersession.
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.memories
    ADD COLUMN superseded_by uuid REFERENCES proxima_core.memories(memory_id);

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_superseded_by_not_self_chk
        CHECK (superseded_by IS DISTINCT FROM memory_id);

CREATE UNIQUE INDEX idx_memories_superseded_by_uq
    ON proxima_core.memories USING btree (superseded_by)
    WHERE (superseded_by IS NOT NULL);

COMMENT ON COLUMN proxima_core.memories.superseded_by IS
  'The revision that replaced this row, when one has. The inverse of supersedes, kept on the row so "is this the head?" is a column read rather than an index traversal. Facts are never superseded.';

ALTER TABLE proxima_core.goals
    ADD COLUMN superseded_by uuid REFERENCES proxima_core.goals(goal_id);

ALTER TABLE proxima_core.goals
    ADD CONSTRAINT goals_superseded_by_not_self_chk
        CHECK (superseded_by IS DISTINCT FROM goal_id);

CREATE UNIQUE INDEX idx_goals_superseded_by_uq
    ON proxima_core.goals USING btree (superseded_by)
    WHERE (superseded_by IS NOT NULL);

-- ---------------------------------------------------------------------------
-- Authorship is node metadata.
--
-- "Emitted by Perspective P" is known at write time and answered by the row,
-- so it is a column, not an edge with an authorship mask.
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.memories
    ADD COLUMN authoring_perspective_id uuid REFERENCES proxima_core.memories(memory_id);

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_authoring_perspective_not_self_chk
        CHECK (authoring_perspective_id IS DISTINCT FROM memory_id);

CREATE INDEX idx_memories_authoring_perspective
    ON proxima_core.memories USING btree (authoring_perspective_id)
    WHERE (authoring_perspective_id IS NOT NULL);

COMMENT ON COLUMN proxima_core.memories.authoring_perspective_id IS
  'The Perspective that emitted this memory, when one did. Replaces the core/authored edge and its authorship mask: authorship of a node is a property of the node.';

-- ---------------------------------------------------------------------------
-- Goal topology is what the Goal row says it is.
--
-- The Goal knows the Perspective it inspires, the Goals it waits on, and the
-- evidence it rests on. Those three declarations are the home of the
-- statement; the reference rows in `edges` are derived from them, which is
-- what makes the goal side of the index rebuildable (E7).
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.goals
    ADD COLUMN assignment_perspective_id uuid REFERENCES proxima_core.memories(memory_id);

ALTER TABLE proxima_core.goals
    ADD COLUMN dependency_goal_ids uuid[] NOT NULL DEFAULT '{}';

ALTER TABLE proxima_core.goals
    ADD COLUMN evidence_memory_ids uuid[] NOT NULL DEFAULT '{}';

COMMENT ON COLUMN proxima_core.goals.assignment_perspective_id IS
  'The self Perspective this Goal inspires (was the core/inspires edge). One reference row is derived from it.';

COMMENT ON COLUMN proxima_core.goals.dependency_goal_ids IS
  'Goals this one waits on (was core/depends-on). One reference row per entry.';

COMMENT ON COLUMN proxima_core.goals.evidence_memory_ids IS
  'Memories this Goal rests on (was core/wake-motivated-by). One reference row per entry.';

-- ---------------------------------------------------------------------------
-- A computed score is an Abstraction, and an Abstraction may cite.
--
-- docs/16 §Computed Scores Are Abstractions amends docs/11 §Multiplicity:
-- citation_mapping_id becomes optional for Fact AND Abstraction. Perspectives
-- still never cite directly — an interpretation grounds through references.
-- Multiplicity stays 0..1 per memory.
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.memories
    DROP CONSTRAINT memories_variant_chk;

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_variant_chk CHECK (
        (kind IS NULL
         AND operator_kind IS NULL AND operator_id IS NULL
         AND input_contract_id IS NULL AND source_batch_id IS NULL
         AND model_id IS NULL AND prompt_version IS NULL AND supersedes IS NULL)
        OR (kind IS NOT NULL
            AND text IS NOT NULL
            AND operator_kind IS NOT NULL
            AND operator_id IS NOT NULL
            AND input_contract_id IS NOT NULL
            AND (
                (operator_kind = 'FtoA'::proxima_core.memory_operator_kind
                 AND kind = 'Abstraction'::proxima_core.entity_kind
                 AND source_batch_id IS NOT NULL)
                OR (operator_kind = 'AtoA'::proxima_core.memory_operator_kind
                    AND kind = 'Abstraction'::proxima_core.entity_kind
                    AND source_batch_id IS NULL)
                OR (operator_kind = 'AtoP'::proxima_core.memory_operator_kind
                    AND kind = 'Perspective'::proxima_core.entity_kind
                    AND source_batch_id IS NULL)
            )
            AND model_id IS NOT NULL
            AND prompt_version IS NOT NULL
            AND receipt_id IS NULL
            AND (citation_mapping_id IS NULL
                 OR kind = 'Abstraction'::proxima_core.entity_kind))
    );

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_superseded_by_not_a_fact_chk
        CHECK (superseded_by IS NULL OR kind IS NOT NULL);

COMMENT ON COLUMN proxima_core.memories.citation_mapping_id IS
  'Optional outside-proof for a Fact or an Abstraction (-> citation_mappings). An Abstraction cites the record of the computation that produced it. Forbidden on Perspectives, which ground through their references.';

-- ---------------------------------------------------------------------------
-- The interpretation Perspective.
--
-- core_link stored a reason and a confidence on an edge. A claim with a
-- reason and a confidence is a judgment, and judgments are Perspectives — the
-- edge was a Perspective hiding in a cheaper container. The subjects live in
-- the payload as schema-declared reference fields, so the reference rows that
-- connect an interpretation to what it interprets are re-derivable from this
-- row alone.
-- ---------------------------------------------------------------------------

-- A subject kind is a closed vocabulary, so it is an enum and not text. It is
-- deliberately NOT entity_kind: that enum carries 'Goal', and a Goal is not a
-- memory and cannot be an interpretation subject on this payload. Reusing it
-- would let the column hold a value `InterpretationSubjectKind` cannot
-- represent, which is the widening this type exists to refuse.
CREATE TYPE proxima_core.interpretation_subject_kind AS ENUM (
    'Fact',
    'Abstraction',
    'Perspective'
);

COMMENT ON TYPE proxima_core.interpretation_subject_kind IS
  'Memory layer of an interpretation subject. F/A/P only — a Goal is not a memory and cannot be a subject here. A Perspective may interpret any layer: the layering rule is satisfied because the Perspective, not the subject, is the edge source.';

CREATE TABLE proxima_core.interpretation_v1 (
    memory_id uuid NOT NULL,
    claim text NOT NULL,
    confidence smallint NOT NULL,
    subject_memory_ids uuid[] NOT NULL,
    subject_kinds proxima_core.interpretation_subject_kind[] NOT NULL,
    model_id text NOT NULL,
    client_name text NOT NULL,
    client_version text NOT NULL,
    CONSTRAINT interpretation_v1_pkey PRIMARY KEY (memory_id),
    CONSTRAINT interpretation_v1_memory_id_fkey
        FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id),
    CONSTRAINT interpretation_v1_claim_nonempty CHECK (length(btrim(claim)) > 0),
    CONSTRAINT interpretation_v1_confidence_range CHECK (confidence BETWEEN 0 AND 100),
    CONSTRAINT interpretation_v1_subjects_aligned
        CHECK (cardinality(subject_memory_ids) = cardinality(subject_kinds))
);

COMMENT ON TABLE proxima_core.interpretation_v1 IS
  'An agent claim about existing nodes (core/interpretation-v1). Successor to the agent_link_v1 edge sidecar: the reason became the claim, the confidence stayed, and the two endpoints became subject references the ingest turns into reference rows.';

CREATE TRIGGER interpretation_v1_append_only BEFORE UPDATE ON proxima_core.interpretation_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();

-- ---------------------------------------------------------------------------
-- Compliance: an edge target redaction is keyed by the edge, and the edge is
-- its own key.
-- ---------------------------------------------------------------------------
CREATE TABLE proxima_core.compliance_edge_target_redactions (
    operation_id uuid NOT NULL,
    source_kind proxima_core.edge_endpoint_kind NOT NULL,
    source_id uuid NOT NULL,
    target_kind proxima_core.edge_endpoint_kind NOT NULL,
    target_id uuid NOT NULL,
    kind proxima_core.edge_kind NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT compliance_edge_target_redactions_pkey
        PRIMARY KEY (operation_id, source_kind, source_id, target_kind, target_id, kind),
    CONSTRAINT compliance_edge_target_redactions_operation_id_fkey
        FOREIGN KEY (operation_id) REFERENCES proxima_core.compliance_audit_log(operation_id)
);

CREATE INDEX idx_compliance_edge_target_redactions_edge
    ON proxima_core.compliance_edge_target_redactions
    USING btree (source_kind, source_id, target_kind, target_id, kind);

-- ---------------------------------------------------------------------------
-- change_event carries the edge, not a handle to it.
--
-- The old row carried edge_id + edge_relation + three endpoint columns per
-- side, and the reader hydrated endpoint kinds with a second query. The
-- endpoints are now one (kind, id) pair each, which is the whole edge, so the
-- read is one query and the projection needs nothing it does not already
-- have.
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.change_event
    DROP CONSTRAINT change_event_endpoint_chk;

ALTER TABLE proxima_core.change_event
    DROP COLUMN edge_id,
    DROP COLUMN edge_relation,
    DROP COLUMN edge_source_memory_id,
    DROP COLUMN edge_source_goal_id,
    DROP COLUMN edge_source_fact_entity_id,
    DROP COLUMN edge_target_memory_id,
    DROP COLUMN edge_target_goal_id,
    DROP COLUMN edge_target_fact_entity_id;

ALTER TABLE proxima_core.change_event
    ADD COLUMN edge_kind proxima_core.edge_kind,
    ADD COLUMN edge_source_kind proxima_core.edge_endpoint_kind,
    ADD COLUMN edge_source_id uuid,
    ADD COLUMN edge_target_kind proxima_core.edge_endpoint_kind,
    ADD COLUMN edge_target_id uuid;

ALTER TABLE proxima_core.change_event
    ADD CONSTRAINT change_event_endpoint_chk CHECK (
        CASE
            WHEN kind IN ('EdgeAppend', 'EdgeDelete') THEN
                entity_kind IS NULL
                AND entity_memory_id IS NULL AND entity_goal_id IS NULL
                AND entity_schema_id IS NULL AND entity_schema_version IS NULL
                AND supersedes_memory_id IS NULL AND supersedes_goal_id IS NULL
                AND edge_kind IS NOT NULL
                AND edge_source_kind IS NOT NULL AND edge_source_id IS NOT NULL
                AND edge_target_kind IS NOT NULL AND edge_target_id IS NOT NULL
            ELSE
                num_nonnulls(entity_memory_id, entity_goal_id) = 1
                AND entity_kind IS NOT NULL
                AND entity_schema_id IS NOT NULL
                AND entity_schema_version IS NOT NULL
                AND edge_kind IS NULL
                AND edge_source_kind IS NULL AND edge_source_id IS NULL
                AND edge_target_kind IS NULL AND edge_target_id IS NULL
                AND NOT (supersedes_memory_id IS NOT NULL AND supersedes_goal_id IS NOT NULL)
        END
    );

COMMENT ON CONSTRAINT change_event_endpoint_chk ON proxima_core.change_event IS
  'Guards the pull-read decode (change_event.rs). EdgeAppend/EdgeDelete rows carry the whole edge — source (kind, id), target (kind, id), edge kind — and no entity/supersedes columns. EntityAppend/EntityDelete rows carry exactly one of entity_memory_id/entity_goal_id plus entity_kind/schema, at most one supersedes endpoint, and no edge columns. Keeps a raw INSERT from persisting an undecodable row.';
