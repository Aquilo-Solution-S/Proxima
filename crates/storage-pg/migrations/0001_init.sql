-- Proxima core schema — destructive v0.0.4 baseline.
-- PR2 folds the pre-v0.0.4 migration stack into one corrected schema truth:
-- direct OwnerRef columns, no legacy owner/share/read-scope compatibility
-- tables, Goal topology through ordinary edges, Fact receipt metadata in
-- fact_receipts, and append-heavy timestamp columns/indexes. Existing
-- pre-v0.0.4 databases must export/reset before this baseline is applied.
-- Prefer regenerating from a migrated DB (pg_dump --schema-only) for broad
-- schema changes; targeted PR2 corrections may be hand-applied here.

CREATE EXTENSION IF NOT EXISTS vector;

CREATE SCHEMA proxima_core;


--
-- Name: change_event_kind; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.change_event_kind AS ENUM (
    'EntityAppend',
    'EdgeAppend',
    'EdgeDelete',
    'EntityDelete'
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
    'OperatorAtoA',
    'OperatorAtoGoal',
    'PerspectiveLink',
    'PerspectiveGoalLink',
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
-- Name: embedding_job_status; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.embedding_job_status AS ENUM (
    'pending',
    'processing',
    'done',
    'failed'
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
    'Active',
    'Paused',
    'Achieved',
    'Abandoned'
);


--
-- Name: goal_wake_trigger_kind; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.goal_wake_trigger_kind AS ENUM (
    'fact_schema',
    'fact_memory'
);

--
-- Name: task_priority; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.task_priority AS ENUM (
    'Low',
    'Medium',
    'High'
);


--
-- Name: memory_operator_kind; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.memory_operator_kind AS ENUM (
    'FtoA',
    'AtoA',
    'AtoP'
);


--
-- Name: owner_ref_kind; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.owner_ref_kind AS ENUM (
    'world',
    'personal',
    'group'
);


--
-- Name: membership_relation; Type: TYPE; Schema: proxima_core; Owner: -
--

CREATE TYPE proxima_core.membership_relation AS ENUM (
    'admin',
    'editor',
    'viewer',
    'ingest'
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
-- Name: goals_pair_allowed(proxima_core.goal_state, proxima_core.goal_state, proxima_core.goal_authorship_kind); Type: FUNCTION; Schema: proxima_core; Owner: -
--

CREATE FUNCTION proxima_core.goals_pair_allowed(prior_state proxima_core.goal_state, next_state proxima_core.goal_state, authorship_kind proxima_core.goal_authorship_kind) RETURNS boolean
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT (prior_state, next_state, authorship_kind) IN (
        ('Active'::proxima_core.goal_state, 'Active'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind),
        ('Active'::proxima_core.goal_state, 'Active'::proxima_core.goal_state, 'System'::proxima_core.goal_authorship_kind),
        ('Active'::proxima_core.goal_state, 'Paused'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind),
        ('Active'::proxima_core.goal_state, 'Paused'::proxima_core.goal_state, 'System'::proxima_core.goal_authorship_kind),
        ('Active'::proxima_core.goal_state, 'Achieved'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind),
        ('Active'::proxima_core.goal_state, 'Achieved'::proxima_core.goal_state, 'System'::proxima_core.goal_authorship_kind),
        ('Active'::proxima_core.goal_state, 'Abandoned'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind),
        ('Active'::proxima_core.goal_state, 'Abandoned'::proxima_core.goal_state, 'System'::proxima_core.goal_authorship_kind),
        ('Paused'::proxima_core.goal_state, 'Active'::proxima_core.goal_state, 'User'::proxima_core.goal_authorship_kind),
        ('Paused'::proxima_core.goal_state, 'Active'::proxima_core.goal_state, 'System'::proxima_core.goal_authorship_kind)
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
        IF NEW.authorship_kind NOT IN ('User', 'System') THEN
            RAISE EXCEPTION 'goal: only User/System may seed state=%', NEW.state;
        END IF;
        IF NEW.state <> 'Active' THEN
            RAISE EXCEPTION 'goal: root rows must be Active';
        END IF;
        RETURN NEW;
    END IF;

    SELECT state INTO prior_state
      FROM proxima_core.goals
     WHERE goal_id = NEW.supersedes;

    IF prior_state IS NULL THEN
        RAISE EXCEPTION 'goal: supersedes references unknown id';
    END IF;
    IF prior_state IN ('Achieved', 'Abandoned') THEN
        RAISE EXCEPTION 'goal: state=% is terminal', prior_state;
    END IF;
    IF prior_state = 'Active'
       AND NEW.state = 'Active'
       AND NEW.authorship_kind IN ('User', 'System') THEN
        RETURN NEW;
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
-- Name: validate_edge_invariants(); Type: FUNCTION; Schema: proxima_core; Owner: -
--

CREATE FUNCTION proxima_core.validate_edge_invariants() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    source_actual_kind proxima_core.entity_kind;
    source_owner_kind proxima_core.owner_ref_kind;
    source_owner_id uuid;
    target_actual_kind proxima_core.entity_kind;
    target_owner_kind proxima_core.owner_ref_kind;
    target_owner_id uuid;
    source_layer int;
    target_layer int;
BEGIN
    IF NEW.source_memory_id IS NOT NULL THEN
        SELECT proxima_core.memory_entity_kind(kind),
               owner_kind,
               owner_id
          INTO source_actual_kind,
               source_owner_kind,
               source_owner_id
         FROM proxima_core.memories
         WHERE memory_id = NEW.source_memory_id;
    ELSIF NEW.source_goal_id IS NOT NULL THEN
        SELECT 'Goal'::proxima_core.entity_kind,
               owner_kind,
               owner_id
          INTO source_actual_kind,
               source_owner_kind,
               source_owner_id
          FROM proxima_core.goals
         WHERE goal_id = NEW.source_goal_id;
    ELSE
        SELECT 'Fact'::proxima_core.entity_kind,
               owner_kind,
               owner_id
          INTO source_actual_kind,
               source_owner_kind,
               source_owner_id
          FROM proxima_core.fact_entities
         WHERE fact_entity_id = NEW.source_fact_entity_id;
    END IF;

    IF NEW.target_memory_id IS NOT NULL THEN
        SELECT proxima_core.memory_entity_kind(kind),
               owner_kind,
               owner_id
          INTO target_actual_kind,
               target_owner_kind,
               target_owner_id
         FROM proxima_core.memories
         WHERE memory_id = NEW.target_memory_id;
    ELSIF NEW.target_goal_id IS NOT NULL THEN
        SELECT 'Goal'::proxima_core.entity_kind,
               owner_kind,
               owner_id
          INTO target_actual_kind,
               target_owner_kind,
               target_owner_id
          FROM proxima_core.goals
         WHERE goal_id = NEW.target_goal_id;
    ELSE
        SELECT 'Fact'::proxima_core.entity_kind,
               owner_kind,
               owner_id
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

    IF source_owner_kind <> NEW.owner_kind
       OR source_owner_id IS DISTINCT FROM NEW.owner_id THEN
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
        IF source_owner_kind <> target_owner_kind
           OR source_owner_id IS DISTINCT FROM target_owner_id THEN
            RAISE EXCEPTION 'edge: supersession requires source and target to share Owner';
        END IF;
    END IF;

    RETURN NEW;
END;
$$;


SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: group_memberships; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.group_memberships (
    group_id uuid NOT NULL,
    member_user_id uuid NOT NULL,
    relation proxima_core.membership_relation NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT group_memberships_group_not_nil_chk CHECK ((group_id <> '00000000-0000-0000-0000-000000000000'::uuid)),
    CONSTRAINT group_memberships_member_not_nil_chk CHECK ((member_user_id <> '00000000-0000-0000-0000-000000000000'::uuid))
);


--
-- Name: change_event; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.change_event (
    seq uuid NOT NULL,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    kind proxima_core.change_event_kind NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    entity_kind proxima_core.entity_kind,
    entity_memory_id uuid,
    entity_goal_id uuid,
    entity_schema_id text,
    entity_schema_version integer,
    supersedes_memory_id uuid,
    supersedes_goal_id uuid,
    edge_id uuid,
    edge_relation text,
    edge_source_memory_id uuid,
    edge_source_goal_id uuid,
    edge_source_fact_entity_id uuid,
    edge_target_memory_id uuid,
    edge_target_goal_id uuid,
    edge_target_fact_entity_id uuid,
    CONSTRAINT change_event_endpoint_chk CHECK (
        CASE
            WHEN kind IN ('EdgeAppend', 'EdgeDelete') THEN
                entity_kind IS NULL
                AND entity_memory_id IS NULL AND entity_goal_id IS NULL
                AND entity_schema_id IS NULL AND entity_schema_version IS NULL
                AND supersedes_memory_id IS NULL AND supersedes_goal_id IS NULL
                AND edge_id IS NOT NULL AND edge_relation IS NOT NULL
                AND num_nonnulls(edge_source_memory_id, edge_source_goal_id, edge_source_fact_entity_id) = 1
                AND num_nonnulls(edge_target_memory_id, edge_target_goal_id, edge_target_fact_entity_id) = 1
            ELSE
                num_nonnulls(entity_memory_id, entity_goal_id) = 1
                AND entity_kind IS NOT NULL
                AND entity_schema_id IS NOT NULL
                AND entity_schema_version IS NOT NULL
                AND edge_id IS NULL AND edge_relation IS NULL
                AND edge_source_memory_id IS NULL AND edge_source_goal_id IS NULL AND edge_source_fact_entity_id IS NULL
                AND edge_target_memory_id IS NULL AND edge_target_goal_id IS NULL AND edge_target_fact_entity_id IS NULL
                AND NOT (supersedes_memory_id IS NOT NULL AND supersedes_goal_id IS NOT NULL)
        END
    ),
    CONSTRAINT change_event_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT change_event_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


COMMENT ON CONSTRAINT change_event_endpoint_chk ON proxima_core.change_event IS
  'Endpoint XOR + not-null companions guarding the pull-read decode (change_event.rs). EdgeAppend/EdgeDelete rows carry edge_id/edge_relation and exactly one of *_memory_id/*_goal_id/*_fact_entity_id per edge endpoint, with all entity/supersedes columns NULL. EntityAppend/EntityDelete rows carry exactly one of entity_memory_id/entity_goal_id plus entity_kind/schema, at most one supersedes endpoint, and all edge columns NULL. Mirrors edges_source/target_endpoint_chk; keeps a raw INSERT from persisting an undecodable row.';


--
-- Name: citation_mappings; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.citation_mappings (
    citation_mapping_id uuid NOT NULL,
    schema_id text NOT NULL,
    memory_id uuid NOT NULL,
    cited_object_id uuid NOT NULL,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT citation_mappings_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT citation_mappings_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


COMMENT ON TABLE proxima_core.citation_mappings IS
  'Links a Fact (memory_id) to its cited_object (the outside-proof). This row is the whole mapping for a pure-link citation; schema-specific mapping metadata, when any exists, lives in an optional citation_<schema> sidecar. See docs/11-citations.md.';


--
-- Name: cited_mcp_call_io_v1; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.cited_mcp_call_io_v1 (
    cited_object_id uuid NOT NULL,
    byte_len bigint NOT NULL,
    truncated boolean NOT NULL,
    body bytea NOT NULL,
    CONSTRAINT cited_mcp_call_io_v1_byte_len_chk CHECK ((byte_len >= 0))
);


--
-- Name: cited_object_uploads; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.cited_object_uploads (
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
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
    CONSTRAINT cited_object_uploads_terminal_shape_chk CHECK ((((status = 'completed'::proxima_core.cited_object_upload_status) AND (cited_object_id IS NOT NULL) AND (completed_at IS NOT NULL)) OR ((status <> 'completed'::proxima_core.cited_object_upload_status) AND (completed_at IS NULL)))),
    CONSTRAINT cited_object_uploads_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT cited_object_uploads_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


--
-- Name: cited_objects; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.cited_objects (
    cited_object_id uuid NOT NULL,
    schema_id text NOT NULL,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    content_hash bytea NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT cited_objects_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT cited_objects_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


COMMENT ON TABLE proxima_core.cited_objects IS
  'Parent of a Citation''s evidence: an immutable, content-addressed outside-proof (content_hash), deduplicated across the Facts that cite it. The actual bytes live in the per-schema cited_<schema> sidecar (e.g. cited_mcp_call_io_v1, cited_uploaded_blob_v1). A Citation is NOT a node kind. See docs/11-citations.md.';


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
    source_fact_entity_id uuid,
    target_kind proxima_core.entity_kind NOT NULL,
    target_memory_id uuid,
    target_goal_id uuid,
    target_fact_entity_id uuid,
    authorship_kind proxima_core.edge_authorship_kind NOT NULL,
    authorship_owner_memory_id uuid,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT edges_source_endpoint_chk CHECK ((num_nonnulls(source_memory_id, source_goal_id, source_fact_entity_id) = 1 AND (source_fact_entity_id IS NULL OR source_kind = 'Fact'::proxima_core.entity_kind))),
    CONSTRAINT edges_target_endpoint_chk CHECK ((num_nonnulls(target_memory_id, target_goal_id, target_fact_entity_id) = 1 AND (target_fact_entity_id IS NULL OR target_kind = 'Fact'::proxima_core.entity_kind))),
    CONSTRAINT edges_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT edges_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


COMMENT ON TABLE proxima_core.edges IS
  'Typed directed relations between any two nodes (memory, goal, or fact-entity endpoints; exactly one endpoint column per side, likewise on target). relation_class groups them: Provenance, Structural, Causal, Interpretive, Supersession. See docs/02-memory.md.';


--
-- Name: embeddings; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.embeddings (
    entity_kind proxima_core.entity_kind NOT NULL,
    entity_id uuid NOT NULL,
    embedding_version integer DEFAULT 1 NOT NULL,
    model_id text NOT NULL,
    vec vector(1024) NOT NULL,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT embeddings_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT embeddings_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


--
-- Name: embedding_jobs; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.embedding_jobs (
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    entity_kind proxima_core.entity_kind NOT NULL,
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    embedding_version integer DEFAULT 1 NOT NULL,
    status proxima_core.embedding_job_status DEFAULT 'pending' NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    last_error text,
    enqueued_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT embedding_jobs_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT embedding_jobs_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


--
-- Name: fact_receipts; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.fact_receipts (
    receipt_id bytea NOT NULL,
    source text NOT NULL,
    source_batch_id uuid NOT NULL,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    schema_id text NOT NULL,
    schema_version integer NOT NULL,
    payload_hash bytea NOT NULL DEFAULT '\x'::bytea,
    occurred_at timestamp with time zone NOT NULL,
    observed_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT fact_receipts_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind) AND (owner_id IS NULL)) OR ((owner_kind = ANY (ARRAY['personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind])) AND (owner_id IS NOT NULL))),
    CONSTRAINT fact_receipts_schema_version_positive_chk CHECK ((schema_version > 0)),
    CONSTRAINT fact_receipts_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


--
-- Name: fact_entities; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.fact_entities (
    fact_entity_id uuid NOT NULL,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    schema_id text NOT NULL,
    schema_version integer NOT NULL,
    natural_key text[] NOT NULL,
    current_memory_id uuid NOT NULL,
    current_created_at timestamp with time zone NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT fact_entities_schema_version_positive_chk CHECK ((schema_version > 0)),
    CONSTRAINT fact_entities_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT fact_entities_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


--
-- Name: goals; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.goals (
    goal_id uuid NOT NULL,
    schema_id text NOT NULL,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    text text NOT NULL,
    state proxima_core.goal_state NOT NULL,
    supersedes uuid,
    authorship_kind proxima_core.goal_authorship_kind NOT NULL,
    authorship_origin proxima_core.goal_authorship_origin,
    authorship_operator_id uuid,
    authorship_tool_id text,
    operator_kind proxima_core.goal_operator_kind,
    input_contract_id uuid,
    model_id text,
    prompt_version text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    request_id text NOT NULL,
    idempotency_key text NOT NULL,
    schema_version integer NOT NULL,
    payload bytea NOT NULL,
    title text NOT NULL,
    CONSTRAINT goals_authorship_shape_chk CHECK ((((authorship_kind = 'User'::proxima_core.goal_authorship_kind) AND (authorship_origin IS NULL) AND (authorship_operator_id IS NULL) AND (authorship_tool_id IS NULL) AND (operator_kind IS NULL) AND (input_contract_id IS NULL) AND (model_id IS NULL) AND (prompt_version IS NULL)) OR ((authorship_kind = 'System'::proxima_core.goal_authorship_kind) AND (authorship_origin = 'Operator'::proxima_core.goal_authorship_origin) AND (authorship_operator_id IS NOT NULL) AND (operator_kind IS NOT NULL) AND (input_contract_id IS NOT NULL) AND (model_id IS NOT NULL) AND (prompt_version IS NOT NULL) AND (authorship_tool_id IS NULL)) OR ((authorship_kind = 'System'::proxima_core.goal_authorship_kind) AND (authorship_origin = 'Tool'::proxima_core.goal_authorship_origin) AND (authorship_tool_id IS NOT NULL) AND (authorship_operator_id IS NULL) AND (operator_kind IS NULL) AND (input_contract_id IS NULL) AND (model_id IS NULL) AND (prompt_version IS NULL)) OR ((authorship_kind = 'External'::proxima_core.goal_authorship_kind) AND (authorship_origin IS NULL) AND (authorship_operator_id IS NULL) AND (authorship_tool_id IS NULL) AND (operator_kind IS NULL) AND (input_contract_id IS NULL) AND (model_id IS NULL) AND (prompt_version IS NULL)))),
    CONSTRAINT goals_schema_version_positive_chk CHECK ((schema_version > 0)),
    CONSTRAINT goals_payload_nonempty_chk CHECK ((octet_length(payload) > 0)),
    CONSTRAINT goals_request_id_nonempty CHECK ((length(btrim(request_id)) > 0)),
    CONSTRAINT goals_text_nonempty CHECK ((length(btrim(text)) > 0)),
    CONSTRAINT goals_title_nonempty CHECK ((length(btrim(title)) > 0)),
    CONSTRAINT goals_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT goals_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


COMMENT ON TABLE proxima_core.goals IS
  'The Goal node kind (desired end-states), kept out of memories because it carries a lifecycle and authorship model. Goal topology is ordinary proxima_core.edges. See docs/06-goals-and-self.md.';

COMMENT ON COLUMN proxima_core.goals.input_contract_id IS
  'Opaque input contract id for A→Goal OperatorInvocation proof carriers; NULL for non-operator authorship.';


COMMENT ON CONSTRAINT goals_payload_nonempty_chk ON proxima_core.goals IS
  'Defense-in-depth against zero-byte goal-key payloads. Every registered Goal schema produces non-empty schema-owned key material. Engine GoalWrite validates the payload against its registered schema before storage; this CHECK is the last line of defense if a zero-byte key reaches storage by another path. Replaces the former DEFAULT ''\x'' which silently admitted empty payloads.';

COMMENT ON CONSTRAINT goals_request_id_nonempty ON proxima_core.goals IS
  'Goal writes are idempotent per Owner/request_id; empty request ids are never valid.';

COMMENT ON CONSTRAINT goals_text_nonempty ON proxima_core.goals IS
  'Goal text is the retrieval body for the Goal node and must be nonblank.';

COMMENT ON CONSTRAINT goals_title_nonempty ON proxima_core.goals IS
  'Goal title is the compact display label for the Goal node and must be nonblank.';


--
-- Name: goal_wake_config; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.goal_wake_config (
    goal_id uuid NOT NULL,
    trigger_kind proxima_core.goal_wake_trigger_kind NOT NULL,
    trigger_schema_id text,
    trigger_schema_version integer,
    trigger_memory_id uuid,
    tool_ids text[] NOT NULL,
    prompt text NOT NULL,
    hard_memory_ids uuid[] NOT NULL,
    CONSTRAINT goal_wake_config_trigger_shape_chk CHECK ((((trigger_kind = 'fact_schema'::proxima_core.goal_wake_trigger_kind) AND (trigger_schema_id IS NOT NULL) AND (trigger_schema_version IS NOT NULL) AND (trigger_memory_id IS NULL)) OR ((trigger_kind = 'fact_memory'::proxima_core.goal_wake_trigger_kind) AND (trigger_schema_id IS NULL) AND (trigger_schema_version IS NULL) AND (trigger_memory_id IS NOT NULL)))),
    CONSTRAINT goal_wake_config_trigger_schema_version_positive_chk CHECK (((trigger_schema_version IS NULL) OR (trigger_schema_version > 0))),
    CONSTRAINT goal_wake_config_prompt_nonempty_chk CHECK ((length(btrim(prompt)) > 0)),
    CONSTRAINT goal_wake_config_tool_ids_nonempty_chk CHECK ((cardinality(tool_ids) > 0)),
    CONSTRAINT goal_wake_config_tool_ids_no_null_chk CHECK ((array_position(tool_ids, NULL::text) IS NULL)),
    CONSTRAINT goal_wake_config_hard_memory_ids_no_null_chk CHECK ((array_position(hard_memory_ids, NULL::uuid) IS NULL))
);


COMMENT ON TABLE proxima_core.goal_wake_config IS
  'Goal-owned optional WakeConfig. One row per armed Goal; no independent wake identity, handle, owner, executor, or plugin table.';


--
-- Name: goal_activated_v1; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.goal_activated_v1 (
    memory_id uuid NOT NULL,
    goal_id uuid NOT NULL,
    transitioned_at timestamp with time zone NOT NULL
);

COMMENT ON TABLE proxima_core.goal_activated_v1 IS
  'Lifecycle Fact sidecar for core/goal-activated-v1. Skinny by design: readers join proxima_core.goals for title/schema/body.';


--
-- Name: goal_paused_v1; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.goal_paused_v1 (
    memory_id uuid NOT NULL,
    goal_id uuid NOT NULL,
    transitioned_at timestamp with time zone NOT NULL
);

COMMENT ON TABLE proxima_core.goal_paused_v1 IS
  'Lifecycle Fact sidecar for core/goal-paused-v1. Skinny by design: readers join proxima_core.goals for title/schema/body.';


--
-- Name: goal_achieved_v1; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.goal_achieved_v1 (
    memory_id uuid NOT NULL,
    goal_id uuid NOT NULL,
    transitioned_at timestamp with time zone NOT NULL
);

COMMENT ON TABLE proxima_core.goal_achieved_v1 IS
  'Lifecycle Fact sidecar for core/goal-achieved-v1. Skinny by design: readers join proxima_core.goals for title/schema/body.';


--
-- Name: goal_abandoned_v1; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.goal_abandoned_v1 (
    memory_id uuid NOT NULL,
    goal_id uuid NOT NULL,
    transitioned_at timestamp with time zone NOT NULL
);

COMMENT ON TABLE proxima_core.goal_abandoned_v1 IS
  'Lifecycle Fact sidecar for core/goal-abandoned-v1. Skinny by design: readers join proxima_core.goals for title/schema/body.';


--
-- Name: task_goal_v1; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.task_goal_v1 (
    goal_id uuid NOT NULL,
    due_at timestamp with time zone,
    priority proxima_core.task_priority
);

COMMENT ON TABLE proxima_core.task_goal_v1 IS
  'Typed sidecar for core/task-v1 Goal payloads. core/simple-text-v1 has no sidecar table.';



--
-- Name: mcp_call_logged_v1; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.mcp_call_logged_v1 (
    memory_id uuid NOT NULL,
    tool_name text NOT NULL,
    actor_oid text NOT NULL,
    actor_upn text NOT NULL,
    ok boolean NOT NULL,
    error text,
    latency_ms bigint NOT NULL,
    io_byte_len bigint NOT NULL,
    io_truncated boolean NOT NULL,
    io_content_hash bytea NOT NULL,
    CONSTRAINT mcp_call_logged_v1_io_content_hash_len_chk CHECK ((octet_length(io_content_hash) = 32)),
    CONSTRAINT mcp_call_logged_v1_io_byte_len_chk CHECK ((io_byte_len >= 0)),
    CONSTRAINT mcp_call_logged_v1_latency_ms_chk CHECK ((latency_ms >= 0))
);


--
-- Name: memories; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.memories (
    memory_id uuid NOT NULL,
    fact_entity_id uuid,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    schema_id text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    receipt_id bytea,
    citation_mapping_id uuid,
    kind proxima_core.entity_kind,
    text text,
    operator_kind proxima_core.memory_operator_kind,
    operator_id uuid,
    input_contract_id uuid,
    source_batch_id uuid,
    model_id text,
    prompt_version text,
    supersedes uuid,
    schema_version integer NOT NULL,
    tombstoned_at timestamp with time zone,
    CONSTRAINT memories_fact_entity_chk CHECK ((fact_entity_id IS NULL OR kind IS NULL)),
    CONSTRAINT memories_kind_values_chk CHECK (((kind IS NULL) OR (kind = ANY (ARRAY['Abstraction'::proxima_core.entity_kind, 'Perspective'::proxima_core.entity_kind])))),
    CONSTRAINT memories_schema_version_positive_chk CHECK ((schema_version > 0)),
    CONSTRAINT memories_variant_chk CHECK (((kind IS NULL AND operator_kind IS NULL AND operator_id IS NULL AND input_contract_id IS NULL AND source_batch_id IS NULL AND model_id IS NULL AND prompt_version IS NULL AND supersedes IS NULL) OR ((kind IS NOT NULL) AND (text IS NOT NULL) AND (operator_kind IS NOT NULL) AND (operator_id IS NOT NULL) AND (input_contract_id IS NOT NULL) AND (((operator_kind = 'FtoA'::proxima_core.memory_operator_kind) AND (kind = 'Abstraction'::proxima_core.entity_kind) AND (source_batch_id IS NOT NULL)) OR ((operator_kind = 'AtoA'::proxima_core.memory_operator_kind) AND (kind = 'Abstraction'::proxima_core.entity_kind) AND (source_batch_id IS NULL)) OR ((operator_kind = 'AtoP'::proxima_core.memory_operator_kind) AND (kind = 'Perspective'::proxima_core.entity_kind) AND (source_batch_id IS NULL))) AND (model_id IS NOT NULL) AND (prompt_version IS NOT NULL) AND (receipt_id IS NULL) AND (citation_mapping_id IS NULL)))),
    CONSTRAINT memories_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT memories_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


COMMENT ON TABLE proxima_core.memories IS
  'Graph nodes of kind Fact | Abstraction | Perspective (the fourth node kind, Goal, lives in goals). Discriminated by the kind column via memories_variant_chk: Fact rows have kind NULL, optional receipt_id, optional citation_mapping_id, and no operator fields; Abstraction (FtoA operator) and Perspective (AtoP operator) = kind set, operator-derived (operator_kind/model_id/prompt_version), with no receipt_id or citation. See docs/02-memory.md for the Fact -> Abstraction -> Perspective -> Goal derivation pipeline.';

COMMENT ON COLUMN proxima_core.memories.kind IS
  'NULL => Fact; otherwise Abstraction or Perspective (constrained by memories_kind_values_chk + memories_variant_chk).';

COMMENT ON COLUMN proxima_core.memories.receipt_id IS
  'Optional on Facts: the source fact receipt (proxima_core.fact_receipts) this Fact was ingested from when receipt metadata exists. NULL on receiptless Facts and on Abstractions/Perspectives.';

COMMENT ON COLUMN proxima_core.memories.citation_mapping_id IS
  'Optional outside-proof for a Fact (-> citation_mappings). Forbidden on Abstractions/Perspectives.';

COMMENT ON COLUMN proxima_core.memories.operator_id IS
  'Opaque operator id for PR7 OperatorInvocation proof carriers on derived memories.';

COMMENT ON COLUMN proxima_core.memories.input_contract_id IS
  'Opaque input contract id for PR7 OperatorInvocation proof carriers on derived memories.';

COMMENT ON COLUMN proxima_core.memories.source_batch_id IS
  'Closed source batch id for F→A derived Abstractions; NULL for A→A/A→P.';


--
-- Name: owner_fact_retention; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.owner_fact_retention (
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    retention_seconds bigint NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT owner_fact_retention_retention_seconds_check CHECK ((retention_seconds > 0)),
    CONSTRAINT owner_fact_retention_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT owner_fact_retention_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);




--
-- Name: source_batches; Type: TABLE; Schema: proxima_core; Owner: -
--

CREATE TABLE proxima_core.source_batches (
    id uuid NOT NULL,
    source_id text NOT NULL,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    opened_at timestamp with time zone DEFAULT now() NOT NULL,
    closed_at timestamp with time zone,
    CONSTRAINT source_batches_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT source_batches_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);



--
-- Name: change_event change_event_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.group_memberships
    ADD CONSTRAINT group_memberships_pkey PRIMARY KEY (group_id, member_user_id, relation);


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
-- Name: cited_mcp_call_io_v1 cited_mcp_call_io_v1_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.cited_mcp_call_io_v1
    ADD CONSTRAINT cited_mcp_call_io_v1_pkey PRIMARY KEY (cited_object_id);


--
-- Name: cited_object_uploads cited_object_uploads_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.cited_object_uploads
    ADD CONSTRAINT cited_object_uploads_pkey PRIMARY KEY (owner_kind, owner_id, upload_id);


--
-- Name: cited_objects cited_objects_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.cited_objects
    ADD CONSTRAINT cited_objects_pkey PRIMARY KEY (cited_object_id);


--
-- Name: cited_objects cited_objects_unique_per_owner; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.cited_objects
    ADD CONSTRAINT cited_objects_unique_per_owner UNIQUE (owner_kind, owner_id, schema_id, content_hash);


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
-- Name: embedding_jobs embedding_jobs_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.embedding_jobs
    ADD CONSTRAINT embedding_jobs_pkey PRIMARY KEY (owner_kind, owner_id, entity_kind, entity_id, model_id, embedding_version);


--
-- Name: fact_receipts fact_receipts_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.fact_receipts
    ADD CONSTRAINT fact_receipts_pkey PRIMARY KEY (receipt_id);


--
-- Name: fact_entities fact_entities_identity_uq; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.fact_entities
    ADD CONSTRAINT fact_entities_identity_uq UNIQUE (owner_kind, owner_id, schema_id, schema_version, natural_key);


--
-- Name: fact_entities fact_entities_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.fact_entities
    ADD CONSTRAINT fact_entities_pkey PRIMARY KEY (fact_entity_id);



--
-- Name: goal_abandoned_v1 goal_abandoned_v1_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_abandoned_v1
    ADD CONSTRAINT goal_abandoned_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: goal_achieved_v1 goal_achieved_v1_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_achieved_v1
    ADD CONSTRAINT goal_achieved_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: goal_activated_v1 goal_activated_v1_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_activated_v1
    ADD CONSTRAINT goal_activated_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: goal_paused_v1 goal_paused_v1_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_paused_v1
    ADD CONSTRAINT goal_paused_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: goals goals_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goals
    ADD CONSTRAINT goals_pkey PRIMARY KEY (goal_id);


--
-- Name: goals goals_idempotency_key; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goals
    ADD CONSTRAINT goals_idempotency_key UNIQUE (idempotency_key);


--
-- Name: goal_wake_config goal_wake_config_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_wake_config
    ADD CONSTRAINT goal_wake_config_pkey PRIMARY KEY (goal_id);


--
-- Name: task_goal_v1 task_goal_v1_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.task_goal_v1
    ADD CONSTRAINT task_goal_v1_pkey PRIMARY KEY (goal_id);



--
-- Name: mcp_call_logged_v1 mcp_call_logged_v1_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.mcp_call_logged_v1
    ADD CONSTRAINT mcp_call_logged_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: memories memories_one_fact_per_receipt; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.memories
    ADD CONSTRAINT memories_one_fact_per_receipt UNIQUE (receipt_id);


--
-- Name: memories memories_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.memories
    ADD CONSTRAINT memories_pkey PRIMARY KEY (memory_id);


--
-- Name: memories_ftoa_batch_exclusive_uidx; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE UNIQUE INDEX memories_ftoa_batch_exclusive_uidx ON proxima_core.memories USING btree (owner_kind, owner_id, source_batch_id, input_contract_id, operator_id, schema_id, schema_version) WHERE ((kind = 'Abstraction'::proxima_core.entity_kind) AND (operator_kind = 'FtoA'::proxima_core.memory_operator_kind) AND (source_batch_id IS NOT NULL));


--
-- Name: owner_fact_retention owner_fact_retention_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.owner_fact_retention
    ADD CONSTRAINT owner_fact_retention_pkey PRIMARY KEY (owner_kind, owner_id);


--
-- Name: source_batches source_batches_pkey; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.source_batches
    ADD CONSTRAINT source_batches_pkey PRIMARY KEY (id);


--
-- Name: source_batches source_batches_unique_per_source; Type: CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.source_batches
    ADD CONSTRAINT source_batches_unique_per_source UNIQUE (source_id, owner_kind, owner_id, id);



--
-- Name: cited_object_uploads_cited_object_id_idx; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX cited_object_uploads_cited_object_id_idx ON proxima_core.cited_object_uploads USING btree (cited_object_id);


--
-- Name: cited_object_uploads_pending_expiry_idx; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX cited_object_uploads_pending_expiry_idx ON proxima_core.cited_object_uploads USING btree (expires_at) WHERE (status = 'pending'::proxima_core.cited_object_upload_status);


--
-- Name: cited_object_uploads_upload_id_idx; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX cited_object_uploads_upload_id_idx ON proxima_core.cited_object_uploads USING btree (upload_id);


--
-- Name: idx_change_event_owner_seq; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_change_event_owner_seq ON proxima_core.change_event USING btree (owner_kind, owner_id, seq);


--
-- Name: idx_change_event_owner_created; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_change_event_owner_created ON proxima_core.change_event USING btree (owner_kind, owner_id, created_at);


--
-- Name: idx_group_memberships_member; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_group_memberships_member ON proxima_core.group_memberships USING btree (member_user_id, group_id, relation);


--
-- Name: idx_citation_mappings_cited_object_id; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_citation_mappings_cited_object_id ON proxima_core.citation_mappings USING btree (cited_object_id);


--
-- Name: idx_citation_mappings_memory_id; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_citation_mappings_memory_id ON proxima_core.citation_mappings USING btree (memory_id);


--
-- Name: idx_edges_authorship_owner; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_authorship_owner ON proxima_core.edges USING btree (authorship_owner_memory_id) WHERE (authorship_owner_memory_id IS NOT NULL);


--
-- Name: idx_edges_owner; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_owner ON proxima_core.edges USING btree (owner_kind, owner_id);

--
-- Name: idx_edges_owner_created; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_owner_created ON proxima_core.edges USING btree (owner_kind, owner_id, created_at);



--
-- Name: idx_edges_provenance_target; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_provenance_target ON proxima_core.edges USING btree (target_memory_id) WHERE (relation_class = 'Provenance'::proxima_core.relation_class);


--
-- Name: idx_edges_relation; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_relation ON proxima_core.edges USING btree (relation);


--
-- Name: idx_edges_source_memory; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_source_memory ON proxima_core.edges USING btree (source_memory_id) WHERE (source_memory_id IS NOT NULL);

--
-- Name: idx_edges_source_memory_created; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_source_memory_created ON proxima_core.edges USING btree (source_memory_id, created_at) WHERE (source_memory_id IS NOT NULL);



--
-- Name: idx_edges_source_goal; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_source_goal ON proxima_core.edges USING btree (source_goal_id) WHERE (source_goal_id IS NOT NULL);

--
-- Name: idx_edges_source_goal_created; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_source_goal_created ON proxima_core.edges USING btree (source_goal_id, created_at) WHERE (source_goal_id IS NOT NULL);



--
-- Name: idx_edges_source_fact_entity; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_source_fact_entity ON proxima_core.edges USING btree (source_fact_entity_id) WHERE (source_fact_entity_id IS NOT NULL);

--
-- Name: idx_edges_source_fact_entity_created; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_source_fact_entity_created ON proxima_core.edges USING btree (source_fact_entity_id, created_at) WHERE (source_fact_entity_id IS NOT NULL);



--
-- Name: idx_edges_target_memory; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_target_memory ON proxima_core.edges USING btree (target_memory_id) WHERE (target_memory_id IS NOT NULL);

--
-- Name: idx_edges_target_memory_created; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_target_memory_created ON proxima_core.edges USING btree (target_memory_id, created_at) WHERE (target_memory_id IS NOT NULL);



--
-- Name: idx_edges_target_goal; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_target_goal ON proxima_core.edges USING btree (target_goal_id) WHERE (target_goal_id IS NOT NULL);

--
-- Name: idx_edges_target_goal_created; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_target_goal_created ON proxima_core.edges USING btree (target_goal_id, created_at) WHERE (target_goal_id IS NOT NULL);



--
-- Name: idx_edges_target_fact_entity; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_target_fact_entity ON proxima_core.edges USING btree (target_fact_entity_id) WHERE (target_fact_entity_id IS NOT NULL);

--
-- Name: idx_edges_target_fact_entity_created; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_edges_target_fact_entity_created ON proxima_core.edges USING btree (target_fact_entity_id, created_at) WHERE (target_fact_entity_id IS NOT NULL);



--
-- Name: idx_embeddings_owner; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_embeddings_owner ON proxima_core.embeddings USING btree (owner_kind, owner_id);


--
-- Name: idx_embedding_jobs_status_enqueued; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_embedding_jobs_status_enqueued ON proxima_core.embedding_jobs USING btree (status, enqueued_at);


--
-- Name: idx_embeddings_vec_hnsw; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_embeddings_vec_hnsw ON proxima_core.embeddings USING hnsw (vec vector_cosine_ops);


--
-- Name: idx_fact_receipts_owner_observed; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_fact_receipts_owner_observed ON proxima_core.fact_receipts USING btree (owner_kind, owner_id, observed_at DESC);


--
-- Name: idx_fact_receipts_source_batch; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_fact_receipts_source_batch ON proxima_core.fact_receipts USING btree (source_batch_id);



--
-- Name: idx_goal_abandoned_v1_goal; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_goal_abandoned_v1_goal ON proxima_core.goal_abandoned_v1 USING btree (goal_id);


--
-- Name: idx_goal_achieved_v1_goal; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_goal_achieved_v1_goal ON proxima_core.goal_achieved_v1 USING btree (goal_id);


--
-- Name: idx_goal_activated_v1_goal; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_goal_activated_v1_goal ON proxima_core.goal_activated_v1 USING btree (goal_id);


--
-- Name: idx_goal_paused_v1_goal; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_goal_paused_v1_goal ON proxima_core.goal_paused_v1 USING btree (goal_id);


--
-- Name: idx_goals_owner_state; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_goals_owner_state ON proxima_core.goals USING btree (owner_kind, owner_id, state);

--
-- Name: idx_goals_owner_state_created; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_goals_owner_state_created ON proxima_core.goals USING btree (owner_kind, owner_id, state, created_at);



--
-- Name: goals_supersedes_unique; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE UNIQUE INDEX goals_supersedes_unique ON proxima_core.goals USING btree (supersedes) WHERE (supersedes IS NOT NULL);


--
-- Name: idx_goal_wake_config_fact_schema; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_goal_wake_config_fact_schema ON proxima_core.goal_wake_config USING btree (trigger_schema_id, trigger_schema_version) WHERE (trigger_kind = 'fact_schema'::proxima_core.goal_wake_trigger_kind);


--
-- Name: idx_goal_wake_config_fact_memory; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_goal_wake_config_fact_memory ON proxima_core.goal_wake_config USING btree (trigger_memory_id) WHERE (trigger_kind = 'fact_memory'::proxima_core.goal_wake_trigger_kind);



--
-- Name: idx_memories_owner_kind; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_memories_owner_kind ON proxima_core.memories USING btree (owner_kind, owner_id, kind);


--
-- Name: idx_memories_fact_entity; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_memories_fact_entity ON proxima_core.memories USING btree (fact_entity_id) WHERE (fact_entity_id IS NOT NULL);


--
-- Name: memories owner-created lookup; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_memories_owner_created ON proxima_core.memories USING btree (owner_kind, owner_id, created_at);



--
-- Name: idx_memories_retention_due; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_memories_retention_due ON proxima_core.memories USING btree (owner_kind, owner_id, created_at) WHERE ((kind IS NULL) AND (citation_mapping_id IS NOT NULL) AND (tombstoned_at IS NULL));


--
-- Name: idx_memories_supersedes_uq; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE UNIQUE INDEX idx_memories_supersedes_uq ON proxima_core.memories USING btree (supersedes) WHERE (supersedes IS NOT NULL);



--
-- Name: idx_source_batches_owner; Type: INDEX; Schema: proxima_core; Owner: -
--

CREATE INDEX idx_source_batches_owner ON proxima_core.source_batches USING btree (owner_kind, owner_id);


--
-- Name: edges edges_invariant_check; Type: TRIGGER; Schema: proxima_core; Owner: -
--

CREATE TRIGGER edges_invariant_check BEFORE INSERT OR UPDATE ON proxima_core.edges FOR EACH ROW EXECUTE FUNCTION proxima_core.validate_edge_invariants();


--
-- Name: goals goals_transition_check; Type: TRIGGER; Schema: proxima_core; Owner: -
--

CREATE TRIGGER goals_transition_check BEFORE INSERT ON proxima_core.goals FOR EACH ROW EXECUTE FUNCTION proxima_core.goals_validate_transition();


--
-- Name: change_event change_event_entity_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.change_event
    ADD CONSTRAINT change_event_entity_goal_id_fkey FOREIGN KEY (entity_goal_id) REFERENCES proxima_core.goals(goal_id);


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
-- Name: cited_mcp_call_io_v1 cited_mcp_call_io_v1_cited_object_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.cited_mcp_call_io_v1
    ADD CONSTRAINT cited_mcp_call_io_v1_cited_object_id_fkey FOREIGN KEY (cited_object_id) REFERENCES proxima_core.cited_objects(cited_object_id);


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
-- Name: edges edges_source_fact_entity_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.edges
    ADD CONSTRAINT edges_source_fact_entity_id_fkey FOREIGN KEY (source_fact_entity_id) REFERENCES proxima_core.fact_entities(fact_entity_id) ON DELETE RESTRICT;


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
-- Name: edges edges_target_fact_entity_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.edges
    ADD CONSTRAINT edges_target_fact_entity_id_fkey FOREIGN KEY (target_fact_entity_id) REFERENCES proxima_core.fact_entities(fact_entity_id) ON DELETE RESTRICT;


--
-- Name: edges edges_target_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.edges
    ADD CONSTRAINT edges_target_memory_id_fkey FOREIGN KEY (target_memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: fact_receipts fact_receipts_source_batch_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.fact_receipts
    ADD CONSTRAINT fact_receipts_source_batch_id_fkey FOREIGN KEY (source_batch_id) REFERENCES proxima_core.source_batches(id);


--
-- Name: fact_entities fact_entities_current_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.fact_entities
    ADD CONSTRAINT fact_entities_current_memory_id_fkey FOREIGN KEY (current_memory_id) REFERENCES proxima_core.memories(memory_id) ON DELETE RESTRICT;


--
-- Name: goal_abandoned_v1 goal_abandoned_v1_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_abandoned_v1
    ADD CONSTRAINT goal_abandoned_v1_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id);


--
-- Name: goal_abandoned_v1 goal_abandoned_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_abandoned_v1
    ADD CONSTRAINT goal_abandoned_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: goal_achieved_v1 goal_achieved_v1_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_achieved_v1
    ADD CONSTRAINT goal_achieved_v1_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id);


--
-- Name: goal_achieved_v1 goal_achieved_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_achieved_v1
    ADD CONSTRAINT goal_achieved_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: goal_activated_v1 goal_activated_v1_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_activated_v1
    ADD CONSTRAINT goal_activated_v1_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id);


--
-- Name: goal_activated_v1 goal_activated_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_activated_v1
    ADD CONSTRAINT goal_activated_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: goal_paused_v1 goal_paused_v1_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_paused_v1
    ADD CONSTRAINT goal_paused_v1_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id);


--
-- Name: goal_paused_v1 goal_paused_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_paused_v1
    ADD CONSTRAINT goal_paused_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);




--
-- Name: goals goals_supersedes_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goals
    ADD CONSTRAINT goals_supersedes_fkey FOREIGN KEY (supersedes) REFERENCES proxima_core.goals(goal_id);


--
-- Name: goal_wake_config goal_wake_config_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_wake_config
    ADD CONSTRAINT goal_wake_config_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id) ON DELETE CASCADE;


--
-- Name: goal_wake_config goal_wake_config_trigger_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.goal_wake_config
    ADD CONSTRAINT goal_wake_config_trigger_memory_id_fkey FOREIGN KEY (trigger_memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: task_goal_v1 task_goal_v1_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.task_goal_v1
    ADD CONSTRAINT task_goal_v1_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id) ON DELETE CASCADE;



--
-- Name: mcp_call_logged_v1 mcp_call_logged_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.mcp_call_logged_v1
    ADD CONSTRAINT mcp_call_logged_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: memories memories_citation_mapping_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.memories
    ADD CONSTRAINT memories_citation_mapping_id_fkey FOREIGN KEY (citation_mapping_id) REFERENCES proxima_core.citation_mappings(citation_mapping_id) DEFERRABLE INITIALLY DEFERRED;


--
-- Name: memories memories_receipt_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.memories
    ADD CONSTRAINT memories_receipt_id_fkey FOREIGN KEY (receipt_id) REFERENCES proxima_core.fact_receipts(receipt_id);


--
-- Name: memories memories_fact_entity_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.memories
    ADD CONSTRAINT memories_fact_entity_id_fkey FOREIGN KEY (fact_entity_id) REFERENCES proxima_core.fact_entities(fact_entity_id) ON DELETE SET NULL;


--
-- Name: memories memories_supersedes_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.memories
    ADD CONSTRAINT memories_supersedes_fkey FOREIGN KEY (supersedes) REFERENCES proxima_core.memories(memory_id);


--
-- Name: memories memories_source_batch_id_fkey; Type: FK CONSTRAINT; Schema: proxima_core; Owner: -
--

ALTER TABLE ONLY proxima_core.memories
    ADD CONSTRAINT memories_source_batch_id_fkey FOREIGN KEY (source_batch_id) REFERENCES proxima_core.source_batches(id);


CREATE TABLE proxima_core.agent_derivation_v1 (
    memory_id uuid NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    tags text[] NOT NULL,
    idempotency_key text,
    source_memory_ids uuid[] NOT NULL,
    model_id text NOT NULL,
    client_name text NOT NULL,
    client_version text NOT NULL,
    CONSTRAINT agent_derivation_v1_body_nonempty CHECK ((length(btrim(body)) > 0)),
    CONSTRAINT agent_derivation_v1_title_nonempty CHECK ((length(btrim(title)) > 0))
);

ALTER TABLE ONLY proxima_core.agent_derivation_v1
    ADD CONSTRAINT agent_derivation_v1_pkey PRIMARY KEY (memory_id);

CREATE INDEX idx_agent_derivation_v1_search ON proxima_core.agent_derivation_v1 USING gin (to_tsvector('simple'::regconfig, ((title || ' '::text) || body)));

ALTER TABLE ONLY proxima_core.agent_derivation_v1
    ADD CONSTRAINT agent_derivation_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


CREATE TABLE proxima_core.agent_link_v1 (
    edge_id uuid NOT NULL,
    reason text NOT NULL,
    confidence smallint NOT NULL,
    CONSTRAINT agent_link_v1_confidence_chk CHECK (((confidence >= 0) AND (confidence <= 100))),
    CONSTRAINT agent_link_v1_reason_nonempty CHECK ((length(btrim(reason)) > 0))
);

ALTER TABLE ONLY proxima_core.agent_link_v1
    ADD CONSTRAINT agent_link_v1_pkey PRIMARY KEY (edge_id);

ALTER TABLE ONLY proxima_core.agent_link_v1
    ADD CONSTRAINT agent_link_v1_edge_id_fkey FOREIGN KEY (edge_id) REFERENCES proxima_core.edges(edge_id);


CREATE TABLE proxima_core.agent_note_v1 (
    memory_id uuid NOT NULL,
    note_id uuid NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    tags text[] NOT NULL,
    idempotency_key text,
    CONSTRAINT agent_note_v1_body_nonempty CHECK ((length(btrim(body)) > 0)),
    CONSTRAINT agent_note_v1_title_nonempty CHECK ((length(btrim(title)) > 0))
);

ALTER TABLE ONLY proxima_core.agent_note_v1
    ADD CONSTRAINT agent_note_v1_pkey PRIMARY KEY (memory_id);

CREATE INDEX idx_agent_note_v1_note_id ON proxima_core.agent_note_v1 USING btree (note_id);

CREATE INDEX idx_agent_note_v1_search ON proxima_core.agent_note_v1 USING gin (to_tsvector('simple'::regconfig, ((title || ' '::text) || body)));

ALTER TABLE ONLY proxima_core.agent_note_v1
    ADD CONSTRAINT agent_note_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


CREATE TABLE proxima_core.utterance_v1 (
    memory_id uuid NOT NULL,
    speaker text NOT NULL,
    conversation_id text NOT NULL,
    text text NOT NULL,
    CONSTRAINT utterance_v1_conversation_id_nonempty CHECK ((length(btrim(conversation_id)) > 0)),
    CONSTRAINT utterance_v1_text_nonempty CHECK ((length(btrim(text)) > 0))
);

ALTER TABLE ONLY proxima_core.utterance_v1
    ADD CONSTRAINT utterance_v1_pkey PRIMARY KEY (memory_id);

ALTER TABLE ONLY proxima_core.utterance_v1
    ADD CONSTRAINT utterance_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- PostgreSQL database dump complete
--
