-- Phase 5.1 group-ownership access model:
-- cross-owner edge authoring is source-governed. Keep the legacy edge owner
-- stamp equal to the source owner, but allow the target owner to differ.

CREATE OR REPLACE FUNCTION proxima_core.validate_edge_invariants() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    source_actual_kind proxima_core.entity_kind;
    source_owner_kind proxima_core.owner_principal_kind;
    source_owner_id uuid;
    target_actual_kind proxima_core.entity_kind;
    target_owner_kind proxima_core.owner_principal_kind;
    target_owner_id uuid;
    source_layer int;
    target_layer int;
BEGIN
    IF NEW.source_memory_id IS NOT NULL THEN
        SELECT proxima_core.memory_entity_kind(kind),
               owner_principal_kind,
               owner_principal_id
          INTO source_actual_kind,
               source_owner_kind,
               source_owner_id
         FROM proxima_core.memories
         WHERE memory_id = NEW.source_memory_id;
    ELSIF NEW.source_goal_id IS NOT NULL THEN
        SELECT 'Goal'::proxima_core.entity_kind,
               owner_principal_kind,
               owner_principal_id
          INTO source_actual_kind,
               source_owner_kind,
               source_owner_id
          FROM proxima_core.goals
         WHERE goal_id = NEW.source_goal_id;
    ELSE
        SELECT 'Fact'::proxima_core.entity_kind,
               owner_principal_kind,
               owner_principal_id
          INTO source_actual_kind,
               source_owner_kind,
               source_owner_id
          FROM proxima_core.fact_entities
         WHERE fact_entity_id = NEW.source_fact_entity_id;
    END IF;

    IF NEW.target_memory_id IS NOT NULL THEN
        SELECT proxima_core.memory_entity_kind(kind),
               owner_principal_kind,
               owner_principal_id
          INTO target_actual_kind,
               target_owner_kind,
               target_owner_id
         FROM proxima_core.memories
         WHERE memory_id = NEW.target_memory_id;
    ELSIF NEW.target_goal_id IS NOT NULL THEN
        SELECT 'Goal'::proxima_core.entity_kind,
               owner_principal_kind,
               owner_principal_id
          INTO target_actual_kind,
               target_owner_kind,
               target_owner_id
          FROM proxima_core.goals
         WHERE goal_id = NEW.target_goal_id;
    ELSE
        SELECT 'Fact'::proxima_core.entity_kind,
               owner_principal_kind,
               owner_principal_id
          INTO target_actual_kind,
               target_owner_kind,
               target_owner_id
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

    IF source_owner_kind <> NEW.owner_principal_kind
       OR source_owner_id <> NEW.owner_principal_id THEN
        RAISE EXCEPTION 'edge: source crosses Owner boundary';
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
