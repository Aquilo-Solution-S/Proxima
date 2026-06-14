-- Proxima core schema — v0.0.1 single init.
-- Squashed 2026-06-13 from 15 dev migrations; proven byte-equivalent to applying
-- them in order (pg_dump --schema-only diff). Regenerate from a migrated DB if the
-- schema changes — do not hand-edit.

CREATE SCHEMA proxima_core;


--
-- Name: change_event_kind; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.change_event_kind AS ENUM (
    'EntityAppend',
    'EdgeAppend'
);


--
-- Name: cited_object_upload_status; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.cited_object_upload_status AS ENUM (
    'pending',
    'completed',
    'aborted',
    'expired'
);


--
-- Name: edge_authorship_kind; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.edge_authorship_kind AS ENUM (
    'EventSource',
    'OperatorFtoA',
    'OperatorAtoP',
    'OperatorAtoGoal',
    'PerspectiveLink',
    'User',
    'Engine',
    'ExternalAgent'
);


--
-- Name: entity_kind; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.entity_kind AS ENUM (
    'Fact',
    'Abstraction',
    'Perspective',
    'Goal'
);


--
-- Name: goal_authorship_kind; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.goal_authorship_kind AS ENUM (
    'User',
    'System',
    'External'
);


--
-- Name: goal_authorship_origin; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.goal_authorship_origin AS ENUM (
    'Operator',
    'Tool'
);


--
-- Name: goal_operator_kind; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.goal_operator_kind AS ENUM (
    'AtoGoal'
);


--
-- Name: goal_state; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.goal_state AS ENUM (
    'Proposed',
    'Active',
    'Paused',
    'Achieved',
    'Abandoned',
    'Rejected'
);


--
-- Name: memory_operator_kind; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.memory_operator_kind AS ENUM (
    'FtoA',
    'AtoP',
    'ExternalAgent',
    'Wake'
);


--
-- Name: owner_principal_kind; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.owner_principal_kind AS ENUM (
    'User',
    'Group'
);


--
-- Name: personality_status; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.personality_status AS ENUM (
    'active',
    'needs_repair',
    'tombstoned'
);


--
-- Name: relation_class; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.relation_class AS ENUM (
    'Provenance',
    'Structural',
    'Causal',
    'Interpretive',
    'Supersession'
);


--
-- Name: wake_authored_by; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.wake_authored_by AS ENUM (
    'any',
    'self',
    'other'
);


--
-- Name: wake_goal_scope; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.wake_goal_scope AS ENUM (
    'none',
    'trigger_goal_assigned'
);


--
-- Name: wake_trigger_kind; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.wake_trigger_kind AS ENUM (
    'on_memory',
    'on_edge'
);


--
-- Name: edge_layer(text); Type: FUNCTION; Schema: proxima_core; Owner: -
--

CREATE FUNCTION proxima_core.edge_layer(kind text) RETURNS integer
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT CASE kind
        WHEN 'Fact' THEN 0
        WHEN 'Abstraction' THEN 1
        WHEN 'Perspective' THEN 2
        ELSE NULL
    END;
$$;


--
-- Name: edge_layer(proxima_core.entity_kind); Type: FUNCTION; Schema: proxima_core; Owner: -
--

CREATE FUNCTION proxima_core.edge_layer(kind proxima_core.entity_kind) RETURNS integer
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT CASE kind
        WHEN 'Fact'::proxima_core.entity_kind THEN 0
        WHEN 'Abstraction'::proxima_core.entity_kind THEN 1
        WHEN 'Perspective'::proxima_core.entity_kind THEN 2
        ELSE NULL
    END;
$$;


--
-- Name: goals_pair_allowed(text, text, text); Type: FUNCTION; Schema: proxima_core; Owner: -
--

CREATE FUNCTION proxima_core.goals_pair_allowed(prior_state text, next_state text, authorship_kind text) RETURNS boolean
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT (prior_state, next_state, authorship_kind) IN (
        ('Proposed', 'Active', 'User'),
        ('Proposed', 'Rejected', 'User'),
        ('Active', 'Active', 'User'),
        ('Active', 'Paused', 'User'),
        ('Active', 'Achieved', 'User'),
        ('Active', 'Achieved', 'System'),
        ('Active', 'Abandoned', 'User'),
        ('Paused', 'Active', 'User'),
        ('Paused', 'Abandoned', 'User')
    );
$$;


--
-- Name: goals_pair_allowed(proxima_core.goal_state, proxima_core.goal_state, proxima_core.goal_authorship_kind); Type: FUNCTION; Schema: proxima_core; Owner: -
--

CREATE FUNCTION proxima_core.goals_pair_allowed(prior_state proxima_core.goal_state, next_state proxima_core.goal_state, authorship_kind proxima_core.goal_authorship_kind) RETURNS boolean
    LANGUAGE sql IMMUTABLE
    AS $$
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


--
-- Name: goals_validate_transition(); Type: FUNCTION; Schema: proxima_core; Owner: -
--

CREATE FUNCTION proxima_core.goals_validate_transition() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
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


--
-- Name: memory_entity_kind(proxima_core.entity_kind); Type: FUNCTION; Schema: proxima_core; Owner: -
--

CREATE FUNCTION proxima_core.memory_entity_kind(kind proxima_core.entity_kind) RETURNS proxima_core.entity_kind
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT COALESCE(kind, 'Fact'::proxima_core.entity_kind);
$$;


--
-- Name: notify_change_event(); Type: FUNCTION; Schema: proxima_core; Owner: -
--

CREATE FUNCTION proxima_core.notify_change_event() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    PERFORM pg_notify('proxima_change_event', NEW.seq::text);
    RETURN NEW;
END;
$$;


--
-- Name: validate_edge_invariants(); Type: FUNCTION; Schema: proxima_core; Owner: -
--

CREATE FUNCTION proxima_core.validate_edge_invariants() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
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


SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: a2p_invocations; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.a2p_invocations (
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    operator_id text NOT NULL,
    prompt_version text NOT NULL,
    model_id text NOT NULL,
    context_hash bytea NOT NULL,
    input_hash bytea NOT NULL,
    head_memory_id uuid,
    run_at timestamp with time zone DEFAULT now() NOT NULL,
    personality_instance_id uuid NOT NULL,
    wake_chain_depth smallint DEFAULT 0 NOT NULL,
    CONSTRAINT a2p_invocations_context_hash_chk CHECK ((octet_length(context_hash) = 32)),
    CONSTRAINT a2p_invocations_input_hash_chk CHECK ((octet_length(input_hash) = 32)),
    CONSTRAINT a2p_invocations_wake_chain_depth_check CHECK ((wake_chain_depth >= 0))
);


--
-- Name: change_event; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.change_event (
    seq uuid NOT NULL,
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    kind proxima_core.change_event_kind NOT NULL,
    entity_kind proxima_core.entity_kind,
    entity_memory_id uuid,
    entity_goal_id uuid,
    entity_schema_id text,
    entity_schema_version integer,
    supersedes_memory_id uuid,
    supersedes_goal_id uuid,
    edge_id uuid,
    edge_relation text,
    edge_source_kind proxima_core.entity_kind,
    edge_source_memory_id uuid,
    edge_source_goal_id uuid,
    edge_target_kind proxima_core.entity_kind,
    edge_target_memory_id uuid,
    edge_target_goal_id uuid,
    entity_personality_instance_id uuid,
    wake_chain_depth smallint DEFAULT 0 NOT NULL
);


--
-- Name: citation_mappings; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.citation_mappings (
    citation_mapping_id uuid NOT NULL,
    schema_id text NOT NULL,
    memory_id uuid NOT NULL,
    cited_object_id uuid NOT NULL,
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: cited_object_uploads; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.cited_object_uploads (
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    upload_id uuid NOT NULL,
    bucket text NOT NULL,
    object_key text NOT NULL,
    filename text NOT NULL,
    mime text NOT NULL,
    expected_byte_len bigint NOT NULL,
    status proxima_core.cited_object_upload_status DEFAULT 'pending'::proxima_core.cited_object_upload_status NOT NULL,
    cited_object_id uuid,
    prepared_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    completed_at timestamp with time zone,
    aborted_at timestamp with time zone,
    error_message text,
    CONSTRAINT cited_object_uploads_expected_len_chk CHECK ((expected_byte_len >= 0)),
    CONSTRAINT cited_object_uploads_terminal_shape_chk CHECK ((((status = 'completed'::proxima_core.cited_object_upload_status) AND (cited_object_id IS NOT NULL) AND (completed_at IS NOT NULL)) OR ((status <> 'completed'::proxima_core.cited_object_upload_status) AND (completed_at IS NULL))))
);


--
-- Name: cited_objects; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.cited_objects (
    cited_object_id uuid NOT NULL,
    schema_id text NOT NULL,
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    content_hash bytea NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: cited_uploaded_blob_v1; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.cited_uploaded_blob_v1 (
    cited_object_id uuid NOT NULL,
    bucket text NOT NULL,
    object_key text NOT NULL,
    sha256 bytea NOT NULL,
    byte_len bigint NOT NULL,
    mime text NOT NULL,
    filename text NOT NULL,
    etag text,
    uploaded_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT cited_uploaded_blob_byte_len_chk CHECK ((byte_len >= 0)),
    CONSTRAINT cited_uploaded_blob_sha256_len_chk CHECK ((octet_length(sha256) = 32))
);


--
-- Name: edges; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.edges (
    edge_id uuid NOT NULL,
    relation text NOT NULL,
    relation_class proxima_core.relation_class NOT NULL,
    source_kind proxima_core.entity_kind NOT NULL,
    source_memory_id uuid,
    source_goal_id uuid,
    target_kind proxima_core.entity_kind NOT NULL,
    target_memory_id uuid,
    target_goal_id uuid,
    authorship_kind proxima_core.edge_authorship_kind NOT NULL,
    authorship_owner_memory_id uuid,
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT edges_source_endpoint_chk CHECK (((source_memory_id IS NOT NULL) <> (source_goal_id IS NOT NULL))),
    CONSTRAINT edges_target_endpoint_chk CHECK (((target_memory_id IS NOT NULL) <> (target_goal_id IS NOT NULL)))
);


--
-- Name: embeddings; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.embeddings (
    entity_kind proxima_core.entity_kind NOT NULL,
    entity_id uuid NOT NULL,
    embedding_version integer DEFAULT 1 NOT NULL,
    model_id text NOT NULL,
    vec real[] NOT NULL,
    dim integer NOT NULL,
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: events; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.events (
    event_id bytea NOT NULL,
    source_id text NOT NULL,
    source_batch_id uuid NOT NULL,
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    schema_id text NOT NULL,
    schema_version integer NOT NULL,
    observed_at timestamp with time zone NOT NULL,
    occurred_at timestamp with time zone NOT NULL,
    payload_ref uuid
);


--
-- Name: goal_parents; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.goal_parents (
    goal_id uuid NOT NULL,
    parent_goal_id uuid NOT NULL,
    CONSTRAINT goal_parents_no_self CHECK ((goal_id <> parent_goal_id))
);


--
-- Name: goals; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.goals (
    goal_id uuid NOT NULL,
    schema_id text NOT NULL,
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    text text NOT NULL,
    state proxima_core.goal_state NOT NULL,
    supersedes uuid,
    authorship_kind proxima_core.goal_authorship_kind NOT NULL,
    authorship_origin proxima_core.goal_authorship_origin,
    authorship_operator_id uuid,
    authorship_tool_id text,
    operator_kind proxima_core.goal_operator_kind,
    model_id text,
    prompt_version text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    request_id text NOT NULL,
    schema_version integer NOT NULL,
    payload bytea DEFAULT '\x'::bytea NOT NULL,
    title text NOT NULL,
    personality_instance_id uuid,
    CONSTRAINT goals_authorship_shape_chk CHECK ((((authorship_kind = 'User'::proxima_core.goal_authorship_kind) AND (authorship_origin IS NULL) AND (authorship_operator_id IS NULL) AND (authorship_tool_id IS NULL) AND (operator_kind IS NULL) AND (model_id IS NULL) AND (prompt_version IS NULL) AND (personality_instance_id IS NULL)) OR ((authorship_kind = 'System'::proxima_core.goal_authorship_kind) AND (authorship_origin = 'Operator'::proxima_core.goal_authorship_origin) AND (authorship_operator_id IS NOT NULL) AND (operator_kind IS NOT NULL) AND (model_id IS NOT NULL) AND (prompt_version IS NOT NULL) AND (personality_instance_id IS NOT NULL) AND (authorship_tool_id IS NULL)) OR ((authorship_kind = 'System'::proxima_core.goal_authorship_kind) AND (authorship_origin = 'Tool'::proxima_core.goal_authorship_origin) AND (authorship_tool_id IS NOT NULL) AND (authorship_operator_id IS NULL) AND (operator_kind IS NULL) AND (model_id IS NULL) AND (prompt_version IS NULL) AND (personality_instance_id IS NULL)) OR ((authorship_kind = 'External'::proxima_core.goal_authorship_kind) AND (authorship_origin IS NULL) AND (authorship_operator_id IS NULL) AND (authorship_tool_id IS NULL) AND (operator_kind IS NULL) AND (model_id IS NULL) AND (prompt_version IS NULL) AND (personality_instance_id IS NULL)))),
    CONSTRAINT goals_schema_version_positive_chk CHECK ((schema_version > 0))
);


--
-- Name: master_token_personality; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.master_token_personality (
    master_token_id uuid NOT NULL,
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    personality_instance_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: memories; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.memories (
    memory_id uuid NOT NULL,
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    schema_id text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    event_id bytea,
    citation_mapping_id uuid,
    kind proxima_core.entity_kind,
    text text,
    operator_kind proxima_core.memory_operator_kind,
    model_id text,
    prompt_version text,
    supersedes uuid,
    schema_version integer NOT NULL,
    personality_instance_id uuid NOT NULL,
    wake_chain_depth smallint DEFAULT 0 NOT NULL,
    CONSTRAINT memories_kind_values_chk CHECK (((kind IS NULL) OR (kind = ANY (ARRAY['Abstraction'::proxima_core.entity_kind, 'Perspective'::proxima_core.entity_kind])))),
    CONSTRAINT memories_schema_version_positive_chk CHECK ((schema_version > 0)),
    CONSTRAINT memories_variant_chk CHECK ((((event_id IS NOT NULL) AND (citation_mapping_id IS NOT NULL) AND (kind IS NULL) AND (text IS NULL) AND (operator_kind IS NULL) AND (model_id IS NULL) AND (prompt_version IS NULL) AND (supersedes IS NULL)) OR ((kind IS NOT NULL) AND (text IS NOT NULL) AND (operator_kind IS NOT NULL) AND (model_id IS NOT NULL) AND (prompt_version IS NOT NULL) AND (event_id IS NULL) AND (citation_mapping_id IS NULL)))),
    CONSTRAINT memories_wake_chain_depth_chk CHECK ((wake_chain_depth >= 0))
);


--
-- Name: personality; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.personality (
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    personality_instance_id uuid NOT NULL,
    current_root_perspective_memory_id uuid NOT NULL,
    max_wake_chain_depth integer DEFAULT 10 NOT NULL,
    status proxima_core.personality_status DEFAULT 'active'::proxima_core.personality_status NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    tombstoned_at timestamp with time zone,
    CONSTRAINT personality_depth_chk CHECK ((max_wake_chain_depth >= 0)),
    CONSTRAINT personality_tombstoned_at_chk CHECK (((status = 'tombstoned'::proxima_core.personality_status) = (tombstoned_at IS NOT NULL)))
);


--
-- Name: personality_config_changed_v1; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.personality_config_changed_v1 (
    memory_id uuid NOT NULL,
    verb text NOT NULL,
    before jsonb,
    after jsonb,
    subject jsonb NOT NULL,
    caller jsonb NOT NULL
);


--
-- Name: personality_wake_cursor; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.personality_wake_cursor (
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    personality_instance_id uuid NOT NULL,
    last_considered_seq uuid DEFAULT '00000000-0000-0000-0000-000000000000'::uuid NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: personality_wake_entries; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.personality_wake_entries (
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    personality_instance_id uuid NOT NULL,
    wake_entry_id uuid NOT NULL,
    trigger_kind proxima_core.wake_trigger_kind NOT NULL,
    trigger_id text NOT NULL,
    label text NOT NULL,
    enabled boolean DEFAULT true NOT NULL,
    authored_by proxima_core.wake_authored_by DEFAULT 'any'::proxima_core.wake_authored_by NOT NULL,
    probability_promille integer DEFAULT 1000 NOT NULL,
    disabled_reason text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    tombstoned_at timestamp with time zone,
    goal_scope proxima_core.wake_goal_scope DEFAULT 'none'::proxima_core.wake_goal_scope NOT NULL,
    instructions text DEFAULT ''::text NOT NULL,
    CONSTRAINT personality_wake_entries_probability_chk CHECK (((probability_promille >= 0) AND (probability_promille <= 1000)))
);


--
-- Name: read_scope_matrix; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.read_scope_matrix (
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    reader_personality_instance_id uuid NOT NULL,
    readable_personality_instance_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT read_scope_matrix_no_identity_chk CHECK ((reader_personality_instance_id <> readable_personality_instance_id))
);


--
-- Name: root_personality_perspective_v1; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.root_personality_perspective_v1 (
    memory_id uuid NOT NULL,
    display_name text NOT NULL,
    purpose text NOT NULL,
    CONSTRAINT root_personality_perspective_display_name_chk CHECK ((length(TRIM(BOTH FROM display_name)) > 0)),
    CONSTRAINT root_personality_perspective_purpose_chk CHECK ((length(TRIM(BOTH FROM purpose)) > 0))
);


--
-- Name: source_batch_f2a; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.source_batch_f2a (
    batch_id uuid NOT NULL,
    operator_id text NOT NULL,
    prompt_version text NOT NULL,
    head_memory_id uuid,
    run_at timestamp with time zone DEFAULT now() NOT NULL,
    model_id text NOT NULL,
    personality_instance_id uuid NOT NULL,
    wake_chain_depth smallint DEFAULT 0 NOT NULL,
    CONSTRAINT source_batch_f2a_wake_chain_depth_check CHECK ((wake_chain_depth >= 0))
);


--
-- Name: source_batches; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.source_batches (
    id uuid NOT NULL,
    source_id text NOT NULL,
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    opened_at timestamp with time zone DEFAULT now() NOT NULL,
    closed_at timestamp with time zone
);


--
-- Name: a2p_invocations a2p_invocations_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.a2p_invocations
    ADD CONSTRAINT a2p_invocations_pkey PRIMARY KEY (owner_principal_kind, owner_principal_id, operator_id, prompt_version, model_id, personality_instance_id, context_hash, input_hash);


--
-- Name: change_event change_event_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.change_event
    ADD CONSTRAINT change_event_pkey PRIMARY KEY (seq);


--
-- Name: citation_mappings citation_mappings_one_per_fact; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.citation_mappings
    ADD CONSTRAINT citation_mappings_one_per_fact UNIQUE (memory_id);


--
-- Name: citation_mappings citation_mappings_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.citation_mappings
    ADD CONSTRAINT citation_mappings_pkey PRIMARY KEY (citation_mapping_id);


--
-- Name: cited_object_uploads cited_object_uploads_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.cited_object_uploads
    ADD CONSTRAINT cited_object_uploads_pkey PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id, upload_id);


--
-- Name: cited_objects cited_objects_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.cited_objects
    ADD CONSTRAINT cited_objects_pkey PRIMARY KEY (cited_object_id);


--
-- Name: cited_objects cited_objects_unique_per_owner; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.cited_objects
    ADD CONSTRAINT cited_objects_unique_per_owner UNIQUE (owner_principal_kind, owner_principal_id, owner_org_id, schema_id, content_hash);


--
-- Name: cited_uploaded_blob_v1 cited_uploaded_blob_object_unique; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.cited_uploaded_blob_v1
    ADD CONSTRAINT cited_uploaded_blob_object_unique UNIQUE (bucket, object_key);


--
-- Name: cited_uploaded_blob_v1 cited_uploaded_blob_v1_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.cited_uploaded_blob_v1
    ADD CONSTRAINT cited_uploaded_blob_v1_pkey PRIMARY KEY (cited_object_id);


--
-- Name: edges edges_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.edges
    ADD CONSTRAINT edges_pkey PRIMARY KEY (edge_id);


--
-- Name: embeddings embeddings_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.embeddings
    ADD CONSTRAINT embeddings_pkey PRIMARY KEY (entity_kind, entity_id, embedding_version, model_id);


--
-- Name: events events_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.events
    ADD CONSTRAINT events_pkey PRIMARY KEY (event_id);


--
-- Name: goal_parents goal_parents_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_parents
    ADD CONSTRAINT goal_parents_pkey PRIMARY KEY (goal_id, parent_goal_id);


--
-- Name: goals goals_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goals
    ADD CONSTRAINT goals_pkey PRIMARY KEY (goal_id);


--
-- Name: goals goals_request_id_idem; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goals
    ADD CONSTRAINT goals_request_id_idem UNIQUE (owner_principal_kind, owner_principal_id, owner_org_id, request_id);


--
-- Name: master_token_personality master_token_personality_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.master_token_personality
    ADD CONSTRAINT master_token_personality_pkey PRIMARY KEY (master_token_id, owner_principal_kind, owner_principal_id, owner_org_id);


--
-- Name: memories memories_one_fact_per_event; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.memories
    ADD CONSTRAINT memories_one_fact_per_event UNIQUE (event_id);


--
-- Name: memories memories_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.memories
    ADD CONSTRAINT memories_pkey PRIMARY KEY (memory_id);


--
-- Name: personality_config_changed_v1 personality_config_changed_v1_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.personality_config_changed_v1
    ADD CONSTRAINT personality_config_changed_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: personality personality_instance_id_uq; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.personality
    ADD CONSTRAINT personality_instance_id_uq UNIQUE (personality_instance_id);


--
-- Name: personality personality_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.personality
    ADD CONSTRAINT personality_pkey PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id);


--
-- Name: personality_wake_cursor personality_wake_cursor_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.personality_wake_cursor
    ADD CONSTRAINT personality_wake_cursor_pkey PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id);


--
-- Name: personality_wake_entries personality_wake_entries_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.personality_wake_entries
    ADD CONSTRAINT personality_wake_entries_pkey PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id, wake_entry_id);


--
-- Name: read_scope_matrix read_scope_matrix_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.read_scope_matrix
    ADD CONSTRAINT read_scope_matrix_pkey PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id, reader_personality_instance_id, readable_personality_instance_id);


--
-- Name: root_personality_perspective_v1 root_personality_perspective_v1_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.root_personality_perspective_v1
    ADD CONSTRAINT root_personality_perspective_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: source_batch_f2a source_batch_f2a_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.source_batch_f2a
    ADD CONSTRAINT source_batch_f2a_pkey PRIMARY KEY (batch_id, operator_id, prompt_version, model_id, personality_instance_id);


--
-- Name: source_batches source_batches_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.source_batches
    ADD CONSTRAINT source_batches_pkey PRIMARY KEY (id);


--
-- Name: source_batches source_batches_unique_per_source; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.source_batches
    ADD CONSTRAINT source_batches_unique_per_source UNIQUE (source_id, owner_principal_kind, owner_principal_id, owner_org_id, id);


--
-- Name: cited_object_uploads_pending_expiry_idx; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX cited_object_uploads_pending_expiry_idx ON proxima_core.cited_object_uploads USING btree (expires_at) WHERE (status = 'pending'::proxima_core.cited_object_upload_status);


--
-- Name: cited_object_uploads_upload_id_idx; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX cited_object_uploads_upload_id_idx ON proxima_core.cited_object_uploads USING btree (upload_id);


--
-- Name: goals_proposed_inbox_idx; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX goals_proposed_inbox_idx ON proxima_core.goals USING btree (owner_principal_kind, owner_principal_id, owner_org_id, created_at DESC) WHERE (state = 'Proposed'::proxima_core.goal_state);


--
-- Name: idx_a2p_invocations_owner_run; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_a2p_invocations_owner_run ON proxima_core.a2p_invocations USING btree (owner_principal_kind, owner_principal_id, run_at DESC);


--
-- Name: idx_change_event_owner_seq; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_change_event_owner_seq ON proxima_core.change_event USING btree (owner_principal_kind, owner_principal_id, seq);


--
-- Name: idx_citation_mappings_cited_object_id; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_citation_mappings_cited_object_id ON proxima_core.citation_mappings USING btree (cited_object_id);


--
-- Name: idx_citation_mappings_memory_id; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_citation_mappings_memory_id ON proxima_core.citation_mappings USING btree (memory_id);


--
-- Name: idx_edges_owner; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_owner ON proxima_core.edges USING btree (owner_principal_kind, owner_principal_id, owner_org_id);


--
-- Name: idx_edges_relation; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_relation ON proxima_core.edges USING btree (relation);


--
-- Name: idx_edges_source_memory; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_source_memory ON proxima_core.edges USING btree (source_memory_id) WHERE (source_memory_id IS NOT NULL);


--
-- Name: idx_edges_target_memory; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_target_memory ON proxima_core.edges USING btree (target_memory_id) WHERE (target_memory_id IS NOT NULL);


--
-- Name: idx_embeddings_owner; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_embeddings_owner ON proxima_core.embeddings USING btree (owner_principal_kind, owner_principal_id, owner_org_id);


--
-- Name: idx_events_owner_observed; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_events_owner_observed ON proxima_core.events USING btree (owner_principal_kind, owner_principal_id, owner_org_id, observed_at DESC);


--
-- Name: idx_events_source_batch; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_events_source_batch ON proxima_core.events USING btree (source_batch_id);


--
-- Name: idx_goal_parents_parent_goal_id; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_goal_parents_parent_goal_id ON proxima_core.goal_parents USING btree (parent_goal_id);


--
-- Name: idx_goals_owner_state; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_goals_owner_state ON proxima_core.goals USING btree (owner_principal_kind, owner_principal_id, owner_org_id, state);


--
-- Name: idx_goals_supersedes; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_goals_supersedes ON proxima_core.goals USING btree (supersedes) WHERE (supersedes IS NOT NULL);


--
-- Name: idx_master_token_personality_instance; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE UNIQUE INDEX idx_master_token_personality_instance ON proxima_core.master_token_personality USING btree (personality_instance_id);


--
-- Name: idx_memories_owner_kind; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_memories_owner_kind ON proxima_core.memories USING btree (owner_principal_kind, owner_principal_id, owner_org_id, kind);


--
-- Name: idx_memories_personality_instance; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_memories_personality_instance ON proxima_core.memories USING btree (personality_instance_id);


--
-- Name: idx_memories_supersedes; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_memories_supersedes ON proxima_core.memories USING btree (supersedes) WHERE (supersedes IS NOT NULL);


--
-- Name: idx_personality_config_changed_v1_subject_id; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_personality_config_changed_v1_subject_id ON proxima_core.personality_config_changed_v1 USING btree (((subject ->> 'id'::text)));


--
-- Name: idx_personality_config_changed_v1_subject_kind; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_personality_config_changed_v1_subject_kind ON proxima_core.personality_config_changed_v1 USING btree (((subject ->> 'kind'::text)));


--
-- Name: idx_personality_config_changed_v1_verb; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_personality_config_changed_v1_verb ON proxima_core.personality_config_changed_v1 USING btree (verb);


--
-- Name: idx_read_scope_matrix_readable; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_read_scope_matrix_readable ON proxima_core.read_scope_matrix USING btree (owner_principal_kind, owner_principal_id, owner_org_id, readable_personality_instance_id);


--
-- Name: idx_source_batches_owner; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_source_batches_owner ON proxima_core.source_batches USING btree (owner_principal_kind, owner_principal_id, owner_org_id);


--
-- Name: personality_wake_entries_active_trigger_uq; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE UNIQUE INDEX personality_wake_entries_active_trigger_uq ON proxima_core.personality_wake_entries USING btree (owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id, trigger_kind, trigger_id) WHERE (tombstoned_at IS NULL);


--
-- Name: personality_wake_entries_trigger_idx; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX personality_wake_entries_trigger_idx ON proxima_core.personality_wake_entries USING btree (trigger_kind, trigger_id) WHERE (enabled AND (tombstoned_at IS NULL));


--
-- Name: change_event change_event_notify_trg; Type: TRIGGER; Schema: proxima_core; Owner: -
--

CREATE TRIGGER change_event_notify_trg AFTER INSERT ON proxima_core.change_event FOR EACH ROW EXECUTE FUNCTION proxima_core.notify_change_event();


--
-- Name: edges edges_invariant_check; Type: TRIGGER; Schema: proxima_core; Owner: -
--

CREATE TRIGGER edges_invariant_check BEFORE INSERT OR UPDATE ON proxima_core.edges FOR EACH ROW EXECUTE FUNCTION proxima_core.validate_edge_invariants();


--
-- Name: goals goals_transition_check; Type: TRIGGER; Schema: proxima_core; Owner: -
--

CREATE TRIGGER goals_transition_check BEFORE INSERT ON proxima_core.goals FOR EACH ROW EXECUTE FUNCTION proxima_core.goals_validate_transition();


--
-- Name: a2p_invocations a2p_invocations_head_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.a2p_invocations
    ADD CONSTRAINT a2p_invocations_head_memory_id_fkey FOREIGN KEY (head_memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: change_event change_event_entity_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.change_event
    ADD CONSTRAINT change_event_entity_goal_id_fkey FOREIGN KEY (entity_goal_id) REFERENCES proxima_core.goals(goal_id);


--
-- Name: change_event change_event_entity_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.change_event
    ADD CONSTRAINT change_event_entity_memory_id_fkey FOREIGN KEY (entity_memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: citation_mappings citation_mappings_cited_object_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.citation_mappings
    ADD CONSTRAINT citation_mappings_cited_object_id_fkey FOREIGN KEY (cited_object_id) REFERENCES proxima_core.cited_objects(cited_object_id);


--
-- Name: citation_mappings citation_mappings_memory_fk; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.citation_mappings
    ADD CONSTRAINT citation_mappings_memory_fk FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id) DEFERRABLE INITIALLY DEFERRED;


--
-- Name: cited_object_uploads cited_object_uploads_cited_object_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.cited_object_uploads
    ADD CONSTRAINT cited_object_uploads_cited_object_id_fkey FOREIGN KEY (cited_object_id) REFERENCES proxima_core.cited_objects(cited_object_id);


--
-- Name: cited_uploaded_blob_v1 cited_uploaded_blob_v1_cited_object_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.cited_uploaded_blob_v1
    ADD CONSTRAINT cited_uploaded_blob_v1_cited_object_id_fkey FOREIGN KEY (cited_object_id) REFERENCES proxima_core.cited_objects(cited_object_id);


--
-- Name: edges edges_authorship_owner_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.edges
    ADD CONSTRAINT edges_authorship_owner_memory_id_fkey FOREIGN KEY (authorship_owner_memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: edges edges_source_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.edges
    ADD CONSTRAINT edges_source_goal_id_fkey FOREIGN KEY (source_goal_id) REFERENCES proxima_core.goals(goal_id);


--
-- Name: edges edges_source_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.edges
    ADD CONSTRAINT edges_source_memory_id_fkey FOREIGN KEY (source_memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: edges edges_target_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.edges
    ADD CONSTRAINT edges_target_goal_id_fkey FOREIGN KEY (target_goal_id) REFERENCES proxima_core.goals(goal_id);


--
-- Name: edges edges_target_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.edges
    ADD CONSTRAINT edges_target_memory_id_fkey FOREIGN KEY (target_memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: events events_source_batch_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.events
    ADD CONSTRAINT events_source_batch_id_fkey FOREIGN KEY (source_batch_id) REFERENCES proxima_core.source_batches(id);


--
-- Name: goal_parents goal_parents_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_parents
    ADD CONSTRAINT goal_parents_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id);


--
-- Name: goal_parents goal_parents_parent_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_parents
    ADD CONSTRAINT goal_parents_parent_goal_id_fkey FOREIGN KEY (parent_goal_id) REFERENCES proxima_core.goals(goal_id);


--
-- Name: goals goals_supersedes_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goals
    ADD CONSTRAINT goals_supersedes_fkey FOREIGN KEY (supersedes) REFERENCES proxima_core.goals(goal_id);


--
-- Name: master_token_personality master_token_personality_personality_instance_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.master_token_personality
    ADD CONSTRAINT master_token_personality_personality_instance_id_fkey FOREIGN KEY (personality_instance_id) REFERENCES proxima_core.personality(personality_instance_id);


--
-- Name: memories memories_citation_mapping_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.memories
    ADD CONSTRAINT memories_citation_mapping_id_fkey FOREIGN KEY (citation_mapping_id) REFERENCES proxima_core.citation_mappings(citation_mapping_id) DEFERRABLE INITIALLY DEFERRED;


--
-- Name: memories memories_event_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.memories
    ADD CONSTRAINT memories_event_id_fkey FOREIGN KEY (event_id) REFERENCES proxima_core.events(event_id);


--
-- Name: memories memories_supersedes_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.memories
    ADD CONSTRAINT memories_supersedes_fkey FOREIGN KEY (supersedes) REFERENCES proxima_core.memories(memory_id);


--
-- Name: personality_config_changed_v1 personality_config_changed_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.personality_config_changed_v1
    ADD CONSTRAINT personality_config_changed_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: personality personality_current_root_perspective_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.personality
    ADD CONSTRAINT personality_current_root_perspective_memory_id_fkey FOREIGN KEY (current_root_perspective_memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: personality_wake_cursor personality_wake_cursor_owner_principal_kind_owner_princip_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.personality_wake_cursor
    ADD CONSTRAINT personality_wake_cursor_owner_principal_kind_owner_princip_fkey FOREIGN KEY (owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id) REFERENCES proxima_core.personality(owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id);


--
-- Name: personality_wake_entries personality_wake_entries_owner_principal_kind_owner_princi_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.personality_wake_entries
    ADD CONSTRAINT personality_wake_entries_owner_principal_kind_owner_princi_fkey FOREIGN KEY (owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id) REFERENCES proxima_core.personality(owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id);


--
-- Name: read_scope_matrix read_scope_matrix_owner_principal_kind_owner_principal_id__fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.read_scope_matrix
    ADD CONSTRAINT read_scope_matrix_owner_principal_kind_owner_principal_id__fkey FOREIGN KEY (owner_principal_kind, owner_principal_id, owner_org_id, reader_personality_instance_id) REFERENCES proxima_core.personality(owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id);


--
-- Name: read_scope_matrix read_scope_matrix_owner_principal_kind_owner_principal_id_fkey1; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.read_scope_matrix
    ADD CONSTRAINT read_scope_matrix_owner_principal_kind_owner_principal_id_fkey1 FOREIGN KEY (owner_principal_kind, owner_principal_id, owner_org_id, readable_personality_instance_id) REFERENCES proxima_core.personality(owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id);


--
-- Name: root_personality_perspective_v1 root_personality_perspective_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.root_personality_perspective_v1
    ADD CONSTRAINT root_personality_perspective_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: source_batch_f2a source_batch_f2a_batch_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.source_batch_f2a
    ADD CONSTRAINT source_batch_f2a_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES proxima_core.source_batches(id);


--
-- Name: source_batch_f2a source_batch_f2a_head_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.source_batch_f2a
    ADD CONSTRAINT source_batch_f2a_head_memory_id_fkey FOREIGN KEY (head_memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- PostgreSQL database dump complete
--
