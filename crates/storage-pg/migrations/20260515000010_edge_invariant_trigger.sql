CREATE OR REPLACE FUNCTION proxima_core.edge_layer(kind text)
RETURNS int LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE kind
        WHEN 'Fact' THEN 0
        WHEN 'Abstraction' THEN 1
        WHEN 'Perspective' THEN 2
        ELSE NULL
    END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.validate_edge_invariants()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    source_actual_kind text;
    source_owner_kind text;
    source_owner_id uuid;
    source_owner_org_id uuid;
    target_actual_kind text;
    target_owner_kind text;
    target_owner_id uuid;
    target_owner_org_id uuid;
    source_layer int;
    target_layer int;
BEGIN
    IF NEW.source_memory_id IS NOT NULL THEN
        SELECT COALESCE(kind, 'Fact'),
               owner_principal_kind,
               owner_principal_id,
               owner_org_id
          INTO source_actual_kind,
               source_owner_kind,
               source_owner_id,
               source_owner_org_id
          FROM proxima_core.memories
         WHERE memory_id = NEW.source_memory_id;
    ELSE
        SELECT 'Goal',
               owner_principal_kind,
               owner_principal_id,
               owner_org_id
          INTO source_actual_kind,
               source_owner_kind,
               source_owner_id,
               source_owner_org_id
          FROM proxima_core.goals
         WHERE goal_id = NEW.source_goal_id;
    END IF;

    IF NEW.target_memory_id IS NOT NULL THEN
        SELECT COALESCE(kind, 'Fact'),
               owner_principal_kind,
               owner_principal_id,
               owner_org_id
          INTO target_actual_kind,
               target_owner_kind,
               target_owner_id,
               target_owner_org_id
          FROM proxima_core.memories
         WHERE memory_id = NEW.target_memory_id;
    ELSE
        SELECT 'Goal',
               owner_principal_kind,
               owner_principal_id,
               owner_org_id
          INTO target_actual_kind,
               target_owner_kind,
               target_owner_id,
               target_owner_org_id
          FROM proxima_core.goals
         WHERE goal_id = NEW.target_goal_id;
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
       OR source_owner_id <> NEW.owner_principal_id
       OR source_owner_org_id <> NEW.owner_org_id THEN
        RAISE EXCEPTION 'edge: source crosses Owner boundary';
    END IF;
    IF target_owner_kind <> NEW.owner_principal_kind
       OR target_owner_id <> NEW.owner_principal_id
       OR target_owner_org_id <> NEW.owner_org_id THEN
        RAISE EXCEPTION 'edge: target crosses Owner boundary';
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

DROP TRIGGER IF EXISTS edges_invariant_check ON proxima_core.edges;

CREATE TRIGGER edges_invariant_check
    BEFORE INSERT OR UPDATE ON proxima_core.edges
    FOR EACH ROW EXECUTE FUNCTION proxima_core.validate_edge_invariants();
