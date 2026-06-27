ALTER TABLE proxima_core.goals ADD COLUMN idempotency_key text;

UPDATE proxima_core.goals
   SET idempotency_key = md5(owner_principal_kind::text || ':' || owner_principal_id::text || ':' || request_id);

ALTER TABLE proxima_core.goals ALTER COLUMN idempotency_key SET NOT NULL;

ALTER TABLE proxima_core.goals DROP CONSTRAINT goals_request_id_idem;

ALTER TABLE proxima_core.goals
    ADD CONSTRAINT goals_idempotency_key UNIQUE (idempotency_key);

DROP INDEX IF EXISTS proxima_core.idx_edges_owner;
DROP INDEX IF EXISTS proxima_core.idx_goals_owner_state;
DROP INDEX IF EXISTS proxima_core.idx_memories_owner_kind;
DROP INDEX IF EXISTS proxima_core.idx_memories_owner_created;
DROP INDEX IF EXISTS proxima_core.idx_memories_retention_due;

CREATE INDEX idx_memories_retention_due
    ON proxima_core.memories USING btree (created_at)
    WHERE event_id IS NOT NULL
      AND citation_mapping_id IS NOT NULL
      AND tombstoned_at IS NULL;

CREATE OR REPLACE FUNCTION proxima_core.validate_edge_invariants() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    source_actual_kind proxima_core.entity_kind;
    target_actual_kind proxima_core.entity_kind;
    source_layer int;
    target_layer int;
BEGIN
    IF NEW.source_memory_id IS NOT NULL THEN
        SELECT proxima_core.memory_entity_kind(kind)
          INTO source_actual_kind
         FROM proxima_core.memories
         WHERE memory_id = NEW.source_memory_id;
    ELSIF NEW.source_goal_id IS NOT NULL THEN
        SELECT 'Goal'::proxima_core.entity_kind
          INTO source_actual_kind
          FROM proxima_core.goals
         WHERE goal_id = NEW.source_goal_id;
    ELSE
        SELECT 'Fact'::proxima_core.entity_kind
          INTO source_actual_kind
          FROM proxima_core.fact_entities
         WHERE fact_entity_id = NEW.source_fact_entity_id;
    END IF;

    IF NEW.target_memory_id IS NOT NULL THEN
        SELECT proxima_core.memory_entity_kind(kind)
          INTO target_actual_kind
         FROM proxima_core.memories
         WHERE memory_id = NEW.target_memory_id;
    ELSIF NEW.target_goal_id IS NOT NULL THEN
        SELECT 'Goal'::proxima_core.entity_kind
          INTO target_actual_kind
          FROM proxima_core.goals
         WHERE goal_id = NEW.target_goal_id;
    ELSE
        SELECT 'Fact'::proxima_core.entity_kind
          INTO target_actual_kind
          FROM proxima_core.fact_entities
         WHERE fact_entity_id = NEW.target_fact_entity_id;
    END IF;

    IF source_actual_kind IS NULL THEN
        RAISE EXCEPTION 'edge: source endpoint not found';
    END IF;
    IF target_actual_kind IS NULL THEN
        RAISE EXCEPTION 'edge: target endpoint not found';
    END IF;
    IF NEW.source_kind <> source_actual_kind THEN
        RAISE EXCEPTION 'edge: source kind % does not match endpoint kind %',
            NEW.source_kind, source_actual_kind;
    END IF;
    IF NEW.target_kind <> target_actual_kind THEN
        RAISE EXCEPTION 'edge: target kind % does not match endpoint kind %',
            NEW.target_kind, target_actual_kind;
    END IF;

    source_layer := proxima_core.edge_layer(NEW.source_kind);
    target_layer := proxima_core.edge_layer(NEW.target_kind);
    IF source_layer IS NOT NULL
       AND target_layer IS NOT NULL
       AND source_layer < target_layer THEN
        RAISE EXCEPTION 'edge: F/A/P layer violation % -> %',
            NEW.source_kind, NEW.target_kind;
    END IF;

    IF NEW.source_kind = 'Fact'
       AND NEW.target_kind = 'Fact'
       AND NEW.relation_class IN ('Causal', 'Interpretive') THEN
        RAISE EXCEPTION 'edge: semantic Fact-to-Fact edges are forbidden';
    END IF;

    IF NEW.relation_class = 'Supersession' THEN
        IF NEW.source_kind = 'Fact' OR NEW.target_kind = 'Fact' THEN
            RAISE EXCEPTION 'edge: Facts cannot be superseded';
        END IF;
        IF NEW.source_kind <> NEW.target_kind THEN
            RAISE EXCEPTION 'edge: supersession requires matching endpoint kinds';
        END IF;
    END IF;

    RETURN NEW;
END;
$$;

ALTER TABLE proxima_core.memories
    DROP COLUMN owner_principal_kind,
    DROP COLUMN owner_principal_id;

ALTER TABLE proxima_core.edges
    DROP COLUMN owner_principal_kind,
    DROP COLUMN owner_principal_id;

ALTER TABLE proxima_core.goals
    DROP COLUMN owner_principal_kind,
    DROP COLUMN owner_principal_id;
