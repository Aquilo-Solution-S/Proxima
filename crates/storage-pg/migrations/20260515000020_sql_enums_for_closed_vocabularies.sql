-- Closed DB vocabularies are SQL enums, not text + membership CHECKs.
-- Keep open identifiers as text: schema_id, relation, model_id, target_ref,
-- vendor, tool ids, paths, handles, and payload text.

CREATE TYPE proxima_core.owner_principal_kind AS ENUM ('User', 'Group');
CREATE TYPE proxima_core.entity_kind AS ENUM ('Fact', 'Abstraction', 'Perspective', 'Goal');
CREATE TYPE proxima_core.memory_operator_kind AS ENUM ('FtoA', 'AtoP', 'ExternalAgent', 'Wake');
CREATE TYPE proxima_core.goal_state AS ENUM ('Proposed', 'Active', 'Paused', 'Achieved', 'Abandoned', 'Rejected');
CREATE TYPE proxima_core.goal_authorship_kind AS ENUM ('User', 'System', 'External');
CREATE TYPE proxima_core.goal_authorship_origin AS ENUM ('Operator', 'Tool');
CREATE TYPE proxima_core.goal_operator_kind AS ENUM ('AtoGoal');
CREATE TYPE proxima_core.change_event_kind AS ENUM ('EntityAppend', 'EdgeAppend');
CREATE TYPE proxima_core.relation_class AS ENUM ('Provenance', 'Structural', 'Causal', 'Interpretive', 'Supersession');
CREATE TYPE proxima_core.edge_authorship_kind AS ENUM (
    'EventSource', 'OperatorFtoA', 'OperatorAtoP', 'OperatorAtoGoal',
    'PerspectiveLink', 'User', 'Engine', 'ExternalAgent'
);
CREATE TYPE proxima_core.personality_status AS ENUM ('active', 'needs_repair', 'tombstoned');
CREATE TYPE proxima_core.wake_trigger_kind AS ENUM ('on_memory', 'on_edge');
CREATE TYPE proxima_core.wake_execution_mode AS ENUM ('substrate_only', 'workspace');
CREATE TYPE proxima_core.wake_authored_by AS ENUM ('any', 'self', 'other');
CREATE TYPE proxima_core.wake_goal_scope AS ENUM ('none', 'trigger_goal_assigned');
CREATE TYPE proxima_core.model_tier AS ENUM ('fast', 'standard', 'deep');
CREATE TYPE proxima_core.wake_invocation_status AS ENUM ('running', 'succeeded', 'truncated', 'failed');
CREATE TYPE proxima_core.wake_invocation_log_status AS ENUM ('started', 'succeeded', 'failed');
CREATE TYPE proxima_core.inference_target_kind AS ENUM (
    'mistral_chat', 'openai_chat', 'openai_responses', 'chatgpt_codex'
);
CREATE TYPE proxima_core.wake_trace_outcome_kind AS ENUM ('succeeded', 'truncated', 'failed');

CREATE TEMP TABLE proxima_enum_saved_fks AS
SELECT conrelid::regclass::text AS table_name,
       conname,
       pg_get_constraintdef(oid) AS definition
  FROM pg_constraint
 WHERE contype = 'f'
   AND connamespace = 'proxima_core'::regnamespace;

DO $$
DECLARE
    fk record;
BEGIN
    FOR fk IN SELECT * FROM proxima_enum_saved_fks LOOP
        EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', fk.table_name, fk.conname);
    END LOOP;
END
$$;

CREATE OR REPLACE FUNCTION proxima_core.__cast_enum_column(
    p_table_name text,
    p_column_name text,
    p_enum_type text,
    p_default_value text DEFAULT NULL
) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    IF to_regclass(p_table_name) IS NULL THEN
        RETURN;
    END IF;

    IF NOT EXISTS (
        SELECT 1
          FROM information_schema.columns
         WHERE table_schema = split_part(p_table_name, '.', 1)
           AND table_name = split_part(p_table_name, '.', 2)
           AND column_name = p_column_name
    ) THEN
        RETURN;
    END IF;

    EXECUTE format('ALTER TABLE %s ALTER COLUMN %I DROP DEFAULT', p_table_name, p_column_name);
    EXECUTE format(
        'ALTER TABLE %s ALTER COLUMN %I TYPE %s USING %I::text::%s',
        p_table_name,
        p_column_name,
        p_enum_type,
        p_column_name,
        p_enum_type
    );

    IF p_default_value IS NOT NULL THEN
        EXECUTE format(
            'ALTER TABLE %s ALTER COLUMN %I SET DEFAULT %L::%s',
            p_table_name,
            p_column_name,
            p_default_value,
            p_enum_type
        );
    END IF;
END;
$$;

-- Drop membership-only CHECKs. Shape/range/FK constraints remain.
ALTER TABLE IF EXISTS proxima_core.source_batches DROP CONSTRAINT IF EXISTS source_batches_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.cited_objects DROP CONSTRAINT IF EXISTS cited_objects_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.citation_mappings DROP CONSTRAINT IF EXISTS citation_mappings_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.events DROP CONSTRAINT IF EXISTS events_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.memories DROP CONSTRAINT IF EXISTS memories_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.memories DROP CONSTRAINT IF EXISTS memories_kind_values_chk;
ALTER TABLE IF EXISTS proxima_core.memories DROP CONSTRAINT IF EXISTS memories_operator_kind_values_chk;
ALTER TABLE IF EXISTS proxima_core.goals DROP CONSTRAINT IF EXISTS goals_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.goals DROP CONSTRAINT IF EXISTS goals_state_chk;
ALTER TABLE IF EXISTS proxima_core.goals DROP CONSTRAINT IF EXISTS goals_authorship_kind_chk;
ALTER TABLE IF EXISTS proxima_core.goals DROP CONSTRAINT IF EXISTS goals_authorship_origin_chk;
ALTER TABLE IF EXISTS proxima_core.goals DROP CONSTRAINT IF EXISTS goals_operator_kind_chk;
ALTER TABLE IF EXISTS proxima_core.change_event DROP CONSTRAINT IF EXISTS change_event_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.change_event DROP CONSTRAINT IF EXISTS change_event_kind_chk;
ALTER TABLE IF EXISTS proxima_core.change_event DROP CONSTRAINT IF EXISTS change_event_entity_kind_chk;
ALTER TABLE IF EXISTS proxima_core.change_event DROP CONSTRAINT IF EXISTS change_event_edge_source_kind_chk;
ALTER TABLE IF EXISTS proxima_core.change_event DROP CONSTRAINT IF EXISTS change_event_edge_target_kind_chk;
ALTER TABLE IF EXISTS proxima_core.edges DROP CONSTRAINT IF EXISTS edges_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.edges DROP CONSTRAINT IF EXISTS edges_source_kind_chk;
ALTER TABLE IF EXISTS proxima_core.edges DROP CONSTRAINT IF EXISTS edges_target_kind_chk;
ALTER TABLE IF EXISTS proxima_core.edges DROP CONSTRAINT IF EXISTS edges_relation_class_chk;
ALTER TABLE IF EXISTS proxima_core.edges DROP CONSTRAINT IF EXISTS edges_authorship_kind_chk;
ALTER TABLE IF EXISTS proxima_core.embeddings DROP CONSTRAINT IF EXISTS embeddings_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.embeddings DROP CONSTRAINT IF EXISTS embeddings_entity_kind_chk;
ALTER TABLE IF EXISTS proxima_core.a2p_invocations DROP CONSTRAINT IF EXISTS a2p_invocations_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.personality DROP CONSTRAINT IF EXISTS personality_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.personality DROP CONSTRAINT IF EXISTS personality_status_chk;
ALTER TABLE IF EXISTS proxima_core.personality_wake_entries DROP CONSTRAINT IF EXISTS personality_wake_entries_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.personality_wake_entries DROP CONSTRAINT IF EXISTS personality_wake_entries_trigger_kind_chk;
ALTER TABLE IF EXISTS proxima_core.personality_wake_entries DROP CONSTRAINT IF EXISTS personality_wake_entries_execution_mode_chk;
ALTER TABLE IF EXISTS proxima_core.personality_wake_entries DROP CONSTRAINT IF EXISTS personality_wake_entries_authored_by_chk;
ALTER TABLE IF EXISTS proxima_core.personality_wake_entries DROP CONSTRAINT IF EXISTS personality_wake_entries_model_tier_chk;
ALTER TABLE IF EXISTS proxima_core.personality_wake_entries DROP CONSTRAINT IF EXISTS personality_wake_entries_goal_scope_chk;
ALTER TABLE IF EXISTS proxima_core.personality_wake_cursor DROP CONSTRAINT IF EXISTS personality_wake_cursor_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.personality_wake_invocations DROP CONSTRAINT IF EXISTS personality_wake_invocations_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.personality_wake_invocations DROP CONSTRAINT IF EXISTS personality_wake_invocations_status_chk;
ALTER TABLE IF EXISTS proxima_core.personality_wake_invocation_logs DROP CONSTRAINT IF EXISTS personality_wake_invocation_logs_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.personality_wake_invocation_logs DROP CONSTRAINT IF EXISTS personality_wake_invocation_logs_status_chk;
ALTER TABLE IF EXISTS proxima_core.inference_targets DROP CONSTRAINT IF EXISTS inference_targets_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.inference_targets DROP CONSTRAINT IF EXISTS inference_targets_kind_chk;
ALTER TABLE IF EXISTS proxima_core.inference_tier_bindings DROP CONSTRAINT IF EXISTS inference_tier_bindings_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_core.inference_tier_bindings DROP CONSTRAINT IF EXISTS inference_tier_bindings_tier_chk;
ALTER TABLE IF EXISTS proxima_core.master_token_personality DROP CONSTRAINT IF EXISTS master_token_personality_principal_kind_chk;

-- Owner scope.
SELECT proxima_core.__cast_enum_column('proxima_core.source_batches', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.cited_objects', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.citation_mappings', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.events', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.memories', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.goals', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.change_event', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.edges', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.embeddings', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.a2p_invocations', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.personality', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.personality_wake_entries', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.personality_wake_cursor', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.personality_wake_invocations', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.personality_wake_invocation_logs', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.inference_targets', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.inference_tier_bindings', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.master_token_personality', 'owner_principal_kind', 'proxima_core.owner_principal_kind');

-- Core cognitive/runtime enums.
SELECT proxima_core.__cast_enum_column('proxima_core.memories', 'kind', 'proxima_core.entity_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.memories', 'operator_kind', 'proxima_core.memory_operator_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.goals', 'state', 'proxima_core.goal_state');
SELECT proxima_core.__cast_enum_column('proxima_core.goals', 'authorship_kind', 'proxima_core.goal_authorship_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.goals', 'authorship_origin', 'proxima_core.goal_authorship_origin');
SELECT proxima_core.__cast_enum_column('proxima_core.goals', 'operator_kind', 'proxima_core.goal_operator_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.change_event', 'kind', 'proxima_core.change_event_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.change_event', 'entity_kind', 'proxima_core.entity_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.change_event', 'edge_source_kind', 'proxima_core.entity_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.change_event', 'edge_target_kind', 'proxima_core.entity_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.edges', 'source_kind', 'proxima_core.entity_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.edges', 'target_kind', 'proxima_core.entity_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.edges', 'relation_class', 'proxima_core.relation_class');
SELECT proxima_core.__cast_enum_column('proxima_core.edges', 'authorship_kind', 'proxima_core.edge_authorship_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.embeddings', 'entity_kind', 'proxima_core.entity_kind');

-- Personality/wake/inference enums.
SELECT proxima_core.__cast_enum_column('proxima_core.personality', 'status', 'proxima_core.personality_status', 'active');
SELECT proxima_core.__cast_enum_column('proxima_core.personality_wake_entries', 'trigger_kind', 'proxima_core.wake_trigger_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.personality_wake_entries', 'execution_mode', 'proxima_core.wake_execution_mode', 'substrate_only');
SELECT proxima_core.__cast_enum_column('proxima_core.personality_wake_entries', 'authored_by', 'proxima_core.wake_authored_by', 'any');
SELECT proxima_core.__cast_enum_column('proxima_core.personality_wake_entries', 'model_tier', 'proxima_core.model_tier', 'standard');
SELECT proxima_core.__cast_enum_column('proxima_core.personality_wake_entries', 'goal_scope', 'proxima_core.wake_goal_scope', 'none');
SELECT proxima_core.__cast_enum_column('proxima_core.personality_wake_invocations', 'status', 'proxima_core.wake_invocation_status');
SELECT proxima_core.__cast_enum_column('proxima_core.personality_wake_invocation_logs', 'status', 'proxima_core.wake_invocation_log_status');
SELECT proxima_core.__cast_enum_column('proxima_core.inference_targets', 'kind', 'proxima_core.inference_target_kind');
SELECT proxima_core.__cast_enum_column('proxima_core.inference_tier_bindings', 'tier', 'proxima_core.model_tier');
SELECT proxima_core.__cast_enum_column('proxima_core.wake_trace_v1', 'outcome_kind', 'proxima_core.wake_trace_outcome_kind');

ALTER TABLE IF EXISTS proxima_core.memories
    ADD CONSTRAINT memories_kind_values_chk
    CHECK (kind IS NULL OR kind IN ('Abstraction', 'Perspective'));

CREATE OR REPLACE FUNCTION proxima_core.goals_pair_allowed(
    prior_state proxima_core.goal_state,
    next_state proxima_core.goal_state,
    authorship_kind proxima_core.goal_authorship_kind
) RETURNS boolean LANGUAGE sql IMMUTABLE AS $$
    SELECT (prior_state, next_state, authorship_kind) IN (
        ('Proposed'::proxima_core.goal_state, 'Active'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind),
        ('Proposed'::proxima_core.goal_state, 'Rejected'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind),
        ('Active'::proxima_core.goal_state, 'Active'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind),
        ('Active'::proxima_core.goal_state, 'Paused'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind),
        ('Active'::proxima_core.goal_state, 'Achieved'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind),
        ('Active'::proxima_core.goal_state, 'Achieved'::proxima_core.goal_state, 'System'::proxima_core.goal_authorship_kind),
        ('Active'::proxima_core.goal_state, 'Abandoned'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind),
        ('Paused'::proxima_core.goal_state, 'Active'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind),
        ('Paused'::proxima_core.goal_state, 'Abandoned'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind)
    );
$$;

CREATE OR REPLACE FUNCTION proxima_core.goals_validate_transition()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    prior_state proxima_core.goal_state;
BEGIN
    IF NEW.supersedes IS NULL THEN
        IF NEW.state = 'Rejected' THEN
            RAISE EXCEPTION 'goal: cannot create directly with state=Rejected';
        END IF;
        IF NEW.state IN ('Active', 'Paused', 'Achieved', 'Abandoned')
           AND NEW.authorship_kind NOT IN ('User', 'System') THEN
            RAISE EXCEPTION 'goal: only User/System may seed state=%', NEW.state;
        END IF;
        RETURN NEW;
    END IF;

    SELECT state INTO prior_state
      FROM proxima_core.goals
     WHERE goal_id = NEW.supersedes;

    IF prior_state IS NULL THEN
        RAISE EXCEPTION 'goal: supersedes references unknown id';
    END IF;
    IF prior_state IN ('Achieved', 'Abandoned', 'Rejected') THEN
        RAISE EXCEPTION 'goal: state=% is terminal', prior_state;
    END IF;
    IF NOT proxima_core.goals_pair_allowed(prior_state, NEW.state, NEW.authorship_kind) THEN
        RAISE EXCEPTION 'goal: forbidden transition %->% under authorship=%',
            prior_state, NEW.state, NEW.authorship_kind;
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.edge_layer(kind proxima_core.entity_kind)
RETURNS int LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE kind
        WHEN 'Fact'::proxima_core.entity_kind THEN 0
        WHEN 'Abstraction'::proxima_core.entity_kind THEN 1
        WHEN 'Perspective'::proxima_core.entity_kind THEN 2
        ELSE NULL
    END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.memory_entity_kind(kind proxima_core.entity_kind)
RETURNS proxima_core.entity_kind LANGUAGE sql IMMUTABLE AS $$
    SELECT COALESCE(kind, 'Fact'::proxima_core.entity_kind);
$$;

CREATE OR REPLACE FUNCTION proxima_core.validate_edge_invariants()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    source_actual_kind proxima_core.entity_kind;
    source_owner_kind proxima_core.owner_principal_kind;
    source_owner_id uuid;
    source_owner_org_id uuid;
    target_actual_kind proxima_core.entity_kind;
    target_owner_kind proxima_core.owner_principal_kind;
    target_owner_id uuid;
    target_owner_org_id uuid;
    source_layer int;
    target_layer int;
BEGIN
    IF NEW.source_memory_id IS NOT NULL THEN
        SELECT proxima_core.memory_entity_kind(kind),
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
        SELECT 'Goal'::proxima_core.entity_kind,
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
        SELECT proxima_core.memory_entity_kind(kind),
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
        SELECT 'Goal'::proxima_core.entity_kind,
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

DO $$
DECLARE
    fk record;
BEGIN
    FOR fk IN SELECT * FROM proxima_enum_saved_fks LOOP
        EXECUTE format('ALTER TABLE %s ADD CONSTRAINT %I %s', fk.table_name, fk.conname, fk.definition);
    END LOOP;
END
$$;

DROP FUNCTION proxima_core.__cast_enum_column(text, text, text, text);
