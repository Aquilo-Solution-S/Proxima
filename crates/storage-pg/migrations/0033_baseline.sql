-- v0.0.36 reset baseline: the schema core migrations 1..32 built.
-- Mutable functions, triggers and policies live in ../definitions/.
-- Immutable once released.

-- Migration role and extensions

DO $owner_rls_role$
DECLARE
    role_is_superuser boolean;
    role_bypasses_rls boolean;
BEGIN
    SELECT r.rolsuper, r.rolbypassrls
      INTO role_is_superuser, role_bypasses_rls
      FROM pg_roles AS r
     WHERE r.rolname = current_user;
    IF role_is_superuser OR role_bypasses_rls THEN
        RAISE EXCEPTION 'owner RLS migration role must be NOSUPERUSER NOBYPASSRLS';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM pg_class AS c
          JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = 'proxima_core'
           AND c.relkind IN ('r', 'p')
           AND NOT pg_has_role(current_user, c.relowner, 'USAGE')
    ) THEN
        RAISE EXCEPTION 'owner RLS migration role does not own every composed table';
    END IF;
END
$owner_rls_role$;

CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS btree_gin;

-- Schema and types

CREATE SCHEMA proxima_core;

CREATE TYPE proxima_core.access_ceiling AS ENUM (
    'none',
    'fact',
    'abstraction',
    'perspective',
    'goal'
);

CREATE TYPE proxima_core.blob_upload_status AS ENUM (
    'pending',
    'completed',
    'aborted',
    'expired'
);

CREATE TYPE proxima_core.embedding_job_status AS ENUM (
    'pending',
    'processing',
    'failed',
    'failed_permanent'
);

CREATE TYPE proxima_core.goal_change_kind AS ENUM (
    'set',
    'modify',
    'decompose_child',
    'pause',
    'resume',
    'abandon',
    'achieve'
);

CREATE TYPE proxima_core.goal_pending_work AS ENUM (
    'keep',
    'discard'
);

CREATE TYPE proxima_core.goal_state AS ENUM (
    'Active',
    'Paused',
    'Achieved',
    'Abandoned'
);

CREATE TYPE proxima_core.interpretation_subject_kind AS ENUM (
    'Fact',
    'Abstraction',
    'Perspective'
);

CREATE TYPE proxima_core.membership_relation AS ENUM (
    'admin',
    'editor',
    'viewer',
    'ingest'
);

CREATE TYPE proxima_core.memory_kind AS ENUM (
    'fact',
    'abstraction',
    'perspective'
);

CREATE TYPE proxima_core.owner_kind AS ENUM (
    'personal',
    'group'
);

CREATE TYPE proxima_core.owner_rls_class AS ENUM (
    'owner_id',
    'fk_parent',
    'ownerless',
    'memory_owner'
);

CREATE TYPE proxima_core.pin_target_kind AS ENUM (
    'fact',
    'abstraction',
    'perspective',
    'goal'
);

CREATE TYPE proxima_core.publication_record_state AS ENUM (
    'pending',
    'published'
);

CREATE TYPE proxima_core.publication_state AS ENUM (
    'pending',
    'claimed',
    'published'
);

CREATE TYPE proxima_core.sketch_kind AS ENUM (
    'fact',
    'abstraction',
    'perspective',
    'goal'
);

CREATE TYPE proxima_core.task_priority AS ENUM (
    'Low',
    'Medium',
    'High'
);

COMMENT ON TYPE proxima_core.owner_rls_class IS 'The four owner-RLS policy classes; a fifth is a new migration (docs/09 §Owner RLS).';

-- Lexical metadata and column functions

CREATE TABLE proxima_core.lexical_default (
    singleton boolean DEFAULT true NOT NULL,
    config regconfig NOT NULL,
    CONSTRAINT lexical_default_singleton_check CHECK (singleton)
);

CREATE TABLE proxima_core.lexical_languages (
    config regconfig NOT NULL
);

CREATE FUNCTION proxima_core.lexical_config() RETURNS regconfig
    LANGUAGE sql STABLE PARALLEL SAFE
    AS $$ SELECT config
     FROM proxima_core.lexical_default
    WHERE singleton $$;

CREATE FUNCTION proxima_core.lexical_join(VARIADIC parts text[]) RETURNS text
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$ SELECT NULLIF(concat_ws(' ', VARIADIC parts), '') $$;

CREATE FUNCTION proxima_core.lexical_text_array(parts text[]) RETURNS text
    LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
    AS $$ SELECT NULLIF(array_to_string(parts, ' '), '') $$;

-- Identity and group authority

CREATE TABLE proxima_core.delegated_authority_grants (
    delegation_id uuid NOT NULL,
    subject_user_id uuid NOT NULL,
    owner_id uuid NOT NULL,
    tool_name text NOT NULL,
    action_name text,
    read_ceiling proxima_core.access_ceiling NOT NULL,
    write_ceiling proxima_core.access_ceiling NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    auth_epoch bigint NOT NULL,
    issued_at timestamp with time zone NOT NULL,
    revoked_at timestamp with time zone,
    revoked_by_user_id uuid
);

CREATE TABLE proxima_core.group_edges (
    child_group_id uuid NOT NULL,
    parent_group_id uuid NOT NULL,
    relation proxima_core.membership_relation DEFAULT 'viewer'::proxima_core.membership_relation NOT NULL,
    child_group_kind proxima_core.owner_kind DEFAULT 'group'::proxima_core.owner_kind NOT NULL,
    parent_group_kind proxima_core.owner_kind DEFAULT 'group'::proxima_core.owner_kind NOT NULL,
    CONSTRAINT group_edges_child_kind_chk CHECK ((child_group_kind = 'group'::proxima_core.owner_kind)),
    CONSTRAINT group_edges_parent_kind_chk CHECK ((parent_group_kind = 'group'::proxima_core.owner_kind))
);

CREATE TABLE proxima_core.group_lock_request (
    request_id uuid NOT NULL,
    left_group_id uuid NOT NULL,
    right_group_id uuid,
    member_is_group boolean DEFAULT false NOT NULL
);

CREATE TABLE proxima_core.group_memberships (
    group_id uuid NOT NULL,
    member_user_id uuid NOT NULL,
    relation proxima_core.membership_relation NOT NULL,
    group_kind proxima_core.owner_kind DEFAULT 'group'::proxima_core.owner_kind NOT NULL,
    CONSTRAINT group_memberships_group_kind_chk CHECK ((group_kind = 'group'::proxima_core.owner_kind))
);

CREATE TABLE proxima_core.group_role_projection (
    member_user_id uuid NOT NULL,
    group_id uuid NOT NULL,
    read_ceiling proxima_core.access_ceiling NOT NULL,
    write_ceiling proxima_core.access_ceiling NOT NULL,
    manage boolean NOT NULL,
    CONSTRAINT group_role_projection_role_chk CHECK (((write_ceiling = 'none'::proxima_core.access_ceiling) OR ((write_ceiling = 'fact'::proxima_core.access_ceiling) AND (read_ceiling = ANY (ARRAY['fact'::proxima_core.access_ceiling, 'abstraction'::proxima_core.access_ceiling, 'perspective'::proxima_core.access_ceiling, 'goal'::proxima_core.access_ceiling]))) OR ((write_ceiling = 'abstraction'::proxima_core.access_ceiling) AND (read_ceiling = ANY (ARRAY['abstraction'::proxima_core.access_ceiling, 'perspective'::proxima_core.access_ceiling, 'goal'::proxima_core.access_ceiling]))) OR ((write_ceiling = 'perspective'::proxima_core.access_ceiling) AND (read_ceiling = ANY (ARRAY['perspective'::proxima_core.access_ceiling, 'goal'::proxima_core.access_ceiling]))) OR ((write_ceiling = 'goal'::proxima_core.access_ceiling) AND (read_ceiling = 'goal'::proxima_core.access_ceiling))))
);

CREATE TABLE proxima_core.owner_rls_epoch (
    singleton boolean DEFAULT true NOT NULL,
    epoch integer NOT NULL,
    CONSTRAINT owner_rls_epoch_singleton_check CHECK (singleton)
);

CREATE TABLE proxima_core.owners (
    owner_id uuid NOT NULL,
    kind proxima_core.owner_kind NOT NULL
);

COMMENT ON TABLE proxima_core.owner_rls_epoch IS 'Non-optional marker proving that the owner-RLS migration and complete policy census ran.';

-- Memory, content and provenance

CREATE TABLE proxima_core.blob (
    blob_id uuid DEFAULT uuidv7() NOT NULL,
    owner_id uuid NOT NULL,
    schema_id text NOT NULL,
    content_hash bytea NOT NULL,
    CONSTRAINT blob_hash_len_chk CHECK ((octet_length(content_hash) = 32))
);

CREATE TABLE proxima_core.blob_uploads (
    upload_id uuid DEFAULT uuidv7() NOT NULL,
    owner_id uuid NOT NULL,
    bucket text NOT NULL,
    object_key text NOT NULL,
    filename text NOT NULL,
    mime text NOT NULL,
    expected_byte_len bigint NOT NULL,
    status proxima_core.blob_upload_status DEFAULT 'pending'::proxima_core.blob_upload_status NOT NULL,
    blob_id uuid,
    sha256 bytea,
    etag text,
    error_message text,
    expires_at timestamp with time zone NOT NULL,
    completed_at timestamp with time zone,
    aborted_at timestamp with time zone,
    mounted_from_upload_id uuid,
    content_hash bytea,
    CONSTRAINT blob_uploads_content_hash_chk CHECK (((content_hash IS NULL) OR (octet_length(content_hash) = 32))),
    CONSTRAINT blob_uploads_mount_not_self_chk CHECK ((mounted_from_upload_id IS DISTINCT FROM upload_id))
);

CREATE TABLE proxima_core.closed_handle (
    handle uuid NOT NULL,
    closed_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE proxima_core.cold_purge_pending (
    object_key text NOT NULL,
    owner_id uuid,
    enqueued_at timestamp with time zone DEFAULT now() NOT NULL,
    backend text DEFAULT ''::text NOT NULL
);

CREATE TABLE proxima_core.content (
    content_id uuid DEFAULT uuidv7() NOT NULL,
    owner_id uuid NOT NULL,
    schema_id text NOT NULL,
    content_hash bytea NOT NULL,
    CONSTRAINT content_hash_len_chk CHECK ((octet_length(content_hash) = 32))
);

CREATE TABLE proxima_core.cooled (
    t uuid NOT NULL,
    handle uuid NOT NULL,
    owner_id uuid NOT NULL,
    kind proxima_core.memory_kind NOT NULL,
    object_key text NOT NULL,
    blob_id uuid,
    content_id uuid,
    source_id text,
    ingest_key text,
    cooled_at timestamp with time zone DEFAULT now() NOT NULL,
    origins uuid[],
    refs uuid[],
    goal_refs uuid[],
    cold_digest bytea,
    CONSTRAINT cooled_cold_digest_len_chk CHECK (((cold_digest IS NULL) OR (octet_length(cold_digest) = 32))),
    CONSTRAINT cooled_goal_refs_no_null_chk CHECK (((goal_refs IS NULL) OR (array_position(goal_refs, NULL::uuid) IS NULL))),
    CONSTRAINT cooled_origins_no_null_chk CHECK (((origins IS NULL) OR (array_position(origins, NULL::uuid) IS NULL))),
    CONSTRAINT cooled_refs_no_null_chk CHECK (((refs IS NULL) OR (array_position(refs, NULL::uuid) IS NULL)))
);

CREATE TABLE proxima_core.erased_pin_target (
    t uuid NOT NULL,
    kind proxima_core.pin_target_kind NOT NULL
);

CREATE TABLE proxima_core.ingest_keys (
    owner_id uuid NOT NULL,
    source_id text NOT NULL,
    ingest_key text NOT NULL,
    t uuid NOT NULL
);

CREATE TABLE proxima_core.memory (
    handle uuid NOT NULL,
    t uuid DEFAULT uuidv7() NOT NULL,
    kind proxima_core.memory_kind NOT NULL,
    owner_id uuid NOT NULL,
    schema_id text NOT NULL,
    source_id text,
    ingest_key text,
    blob_id uuid,
    content_id uuid,
    origins uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    refs uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    sidecar_tables text[] DEFAULT '{}'::text[] NOT NULL,
    goal_refs uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    occurred_at timestamp with time zone,
    CONSTRAINT memory_ap_content_chk CHECK (((kind = 'fact'::proxima_core.memory_kind) OR (content_id IS NOT NULL))),
    CONSTRAINT memory_blob_fa_chk CHECK (((blob_id IS NULL) OR (kind = ANY (ARRAY['fact'::proxima_core.memory_kind, 'abstraction'::proxima_core.memory_kind])))),
    CONSTRAINT memory_fact_occurred_at_chk CHECK (((kind = 'fact'::proxima_core.memory_kind) = (occurred_at IS NOT NULL))),
    CONSTRAINT memory_fact_origins_chk CHECK (((kind <> 'fact'::proxima_core.memory_kind) OR (origins = '{}'::uuid[]))),
    CONSTRAINT memory_fact_source_chk CHECK ((((kind = 'fact'::proxima_core.memory_kind) AND ((source_id IS NULL) = (ingest_key IS NULL))) OR ((kind <> 'fact'::proxima_core.memory_kind) AND (source_id IS NULL) AND (ingest_key IS NULL)))),
    CONSTRAINT memory_goal_refs_no_null_chk CHECK ((array_position(goal_refs, NULL::uuid) IS NULL)),
    CONSTRAINT memory_origins_no_null_chk CHECK ((array_position(origins, NULL::uuid) IS NULL)),
    CONSTRAINT memory_refs_no_null_chk CHECK ((array_position(refs, NULL::uuid) IS NULL)),
    CONSTRAINT memory_sidecar_tables_no_null_chk CHECK ((array_position(sidecar_tables, NULL::text) IS NULL))
);

CREATE TABLE proxima_core.memory_external_origin (
    t uuid NOT NULL,
    store text NOT NULL,
    handle text NOT NULL,
    CONSTRAINT memory_external_origin_handle_chk CHECK (((char_length(handle) >= 1) AND (char_length(handle) <= 512))),
    CONSTRAINT memory_external_origin_store_chk CHECK (((store ~ '^[a-z][a-z0-9_-]{1,63}$'::text) AND (store <> 'local'::text)))
);

CREATE TABLE proxima_core.memory_head (
    handle uuid NOT NULL,
    kind proxima_core.memory_kind NOT NULL,
    schema_id text NOT NULL,
    owner_id uuid NOT NULL,
    t uuid NOT NULL,
    publication_sequence bigint DEFAULT 0 NOT NULL,
    CONSTRAINT memory_head_publication_sequence_chk CHECK ((publication_sequence >= 0))
);

CREATE TABLE proxima_core.source_cursors (
    owner_id uuid NOT NULL,
    source text NOT NULL,
    cursor bytea NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);

COMMENT ON TABLE proxima_core.erased_pin_target IS 'Internal historical identity witness for a hard-erased pin target. It carries no owner or readable payload and is excluded from export.';

-- Core sidecars and search

CREATE TABLE proxima_core.agent_derivation_v1 (
    t uuid NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    tags text[] DEFAULT '{}'::text[] NOT NULL,
    idempotency_key text,
    source_memory_ids uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    model_id text NOT NULL,
    client_name text NOT NULL,
    client_version text NOT NULL,
    embed_text text GENERATED ALWAYS AS (proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(title, ''::text), NULLIF(body, ''::text), proxima_core.lexical_text_array(tags)])) STORED
);

CREATE TABLE proxima_core.agent_note_v1 (
    t uuid NOT NULL,
    note_id uuid NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    tags text[] DEFAULT '{}'::text[] NOT NULL,
    idempotency_key text,
    embed_text text GENERATED ALWAYS AS (proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(title, ''::text), NULLIF(body, ''::text), proxima_core.lexical_text_array(tags)])) STORED
);

CREATE TABLE proxima_core.flavor_surface (
    table_name text NOT NULL,
    flavor_id text NOT NULL,
    CONSTRAINT flavor_surface_flavor_id_chk CHECK ((flavor_id <> ''::text)),
    CONSTRAINT flavor_surface_qualified_chk CHECK (((table_name ~~ '%.%'::text) AND (table_name = lower(table_name))))
);

CREATE TABLE proxima_core.interpretation_v1 (
    t uuid NOT NULL,
    claim text NOT NULL,
    confidence smallint NOT NULL,
    subject_memory_ids uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    subject_kinds proxima_core.interpretation_subject_kind[] DEFAULT '{}'::proxima_core.interpretation_subject_kind[] NOT NULL,
    model_id text NOT NULL,
    client_name text NOT NULL,
    client_version text NOT NULL,
    embed_text text GENERATED ALWAYS AS (NULLIF(claim, ''::text)) STORED
);

CREATE TABLE proxima_core.sketch (
    t uuid NOT NULL,
    owner_id uuid NOT NULL,
    kind proxima_core.sketch_kind NOT NULL,
    text text NOT NULL,
    CONSTRAINT sketch_text_nonblank_chk CHECK ((length(btrim(text)) > 0))
);

CREATE TABLE proxima_core.task_goal_v1 (
    t uuid NOT NULL,
    due_at timestamp with time zone,
    priority proxima_core.task_priority
);

CREATE TABLE proxima_core.utterance_v1 (
    t uuid NOT NULL,
    speaker text NOT NULL,
    conversation_id text NOT NULL,
    text text NOT NULL,
    embed_text text GENERATED ALWAYS AS (NULLIF(text, ''::text)) STORED
);

CREATE TABLE proxima_core.write_act_v1 (
    t uuid NOT NULL,
    episode_id uuid NOT NULL
);

COMMENT ON TABLE proxima_core.flavor_surface IS 'Declared sidecar surfaces, one row per (table, flavor). Populated by each flavor migration; memory.sidecar_tables is constrained to be a subset. Flavor #0 (core) owns the proxima_core.* rows and is non-removable.';

-- Embedding jobs and vectors

CREATE TABLE proxima_core.embedding_heads (
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    embedding_version integer NOT NULL,
    owner_id uuid NOT NULL,
    dim smallint NOT NULL,
    CONSTRAINT embedding_heads_dim_lane_chk CHECK ((dim = ANY (ARRAY[384, 768, 1024, 1536, 2048, 3072])))
);

CREATE TABLE proxima_core.embedding_jobs (
    job_id uuid DEFAULT uuidv7() NOT NULL,
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    owner_id uuid NOT NULL,
    status proxima_core.embedding_job_status DEFAULT 'pending'::proxima_core.embedding_job_status NOT NULL,
    claimed_at timestamp with time zone,
    claim_token uuid,
    last_error text,
    dim smallint NOT NULL,
    CONSTRAINT embedding_job_processing_claim_chk CHECK (((status = 'processing'::proxima_core.embedding_job_status) = ((claimed_at IS NOT NULL) AND (claim_token IS NOT NULL)))),
    CONSTRAINT embedding_jobs_dim_lane_chk CHECK ((dim = ANY (ARRAY[384, 768, 1024, 1536, 2048, 3072])))
);

CREATE TABLE proxima_core.embeddings (
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    embedding_version integer DEFAULT 1 NOT NULL,
    vec public.vector NOT NULL,
    owner_id uuid NOT NULL,
    dim smallint NOT NULL,
    chunk_ordinal integer DEFAULT 0 NOT NULL,
    CONSTRAINT embeddings_chunk_ordinal_chk CHECK ((chunk_ordinal >= 0)),
    CONSTRAINT embeddings_dim_lane_chk CHECK ((dim = ANY (ARRAY[384, 768, 1024, 1536, 2048, 3072]))),
    CONSTRAINT embeddings_vec_width_chk CHECK ((public.vector_dims(vec) = dim))
);

-- Goals and replay declarations

CREATE TABLE proxima_core.goal (
    handle uuid NOT NULL,
    t uuid DEFAULT uuidv7() NOT NULL,
    owner_id uuid NOT NULL,
    title text NOT NULL,
    state proxima_core.goal_state NOT NULL,
    request_id text NOT NULL,
    close_fact_t uuid,
    assignment_t uuid,
    dependency_t uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    evidence_t uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    write_act_t uuid,
    text text,
    CONSTRAINT goal_arrays_no_null_chk CHECK (((array_position(dependency_t, NULL::uuid) IS NULL) AND (array_position(evidence_t, NULL::uuid) IS NULL))),
    CONSTRAINT goal_terminal_close_chk CHECK (((state = ANY (ARRAY['Achieved'::proxima_core.goal_state, 'Abandoned'::proxima_core.goal_state])) = (close_fact_t IS NOT NULL))),
    CONSTRAINT goal_text_nonblank_chk CHECK (((text IS NULL) OR (length(btrim(text, E' \t\n\r\x0b\f'::text)) > 0))),
    CONSTRAINT goal_title_nonblank_chk CHECK ((length(btrim(title)) > 0))
);

CREATE TABLE proxima_core.goal_change_v1 (
    t uuid NOT NULL,
    series uuid NOT NULL,
    goal_t uuid NOT NULL,
    prior_t uuid,
    revision integer NOT NULL,
    state proxima_core.goal_state NOT NULL,
    change_kind proxima_core.goal_change_kind NOT NULL,
    parent_t uuid,
    pending proxima_core.goal_pending_work,
    CONSTRAINT goal_change_v1_parent_chk CHECK (((change_kind = 'decompose_child'::proxima_core.goal_change_kind) = (parent_t IS NOT NULL))),
    CONSTRAINT goal_change_v1_pending_chk CHECK (((change_kind = 'pause'::proxima_core.goal_change_kind) = (pending IS NOT NULL))),
    CONSTRAINT goal_change_v1_revision_chk CHECK ((revision >= 1))
);

CREATE TABLE proxima_core.goal_head (
    handle uuid NOT NULL,
    schema_id text NOT NULL,
    owner_id uuid NOT NULL,
    t uuid NOT NULL
);

CREATE TABLE proxima_core.goal_replay_declaration (
    goal_t uuid NOT NULL,
    declaration jsonb NOT NULL,
    edge_count integer NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT goal_replay_declaration_object_chk CHECK ((jsonb_typeof(declaration) = 'object'::text)),
    CONSTRAINT goal_replay_edge_count_chk CHECK ((edge_count >= 0))
);

-- Publication and installation

CREATE TABLE proxima_core.installation (
    singleton boolean DEFAULT true NOT NULL,
    installation_id uuid DEFAULT gen_random_uuid() NOT NULL,
    CONSTRAINT installation_singleton_check CHECK (singleton)
);

CREATE TABLE proxima_core.publication_consumer (
    name text NOT NULL,
    registered_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT publication_consumer_name_chk CHECK ((name ~ '^[a-z0-9][a-z0-9_-]{0,62}$'::text))
);

CREATE TABLE proxima_core.publication_delivery (
    t uuid NOT NULL,
    consumer text NOT NULL,
    state proxima_core.publication_state DEFAULT 'pending'::proxima_core.publication_state NOT NULL,
    claim_token uuid,
    claimed_by text,
    lease_expires_at timestamp with time zone,
    attempts integer DEFAULT 0 NOT NULL,
    published_at timestamp with time zone,
    CONSTRAINT publication_delivery_attempts_chk CHECK ((attempts >= 0)),
    CONSTRAINT publication_delivery_claim_chk CHECK (((state = 'claimed'::proxima_core.publication_state) = ((claim_token IS NOT NULL) AND (lease_expires_at IS NOT NULL) AND (claimed_by IS NOT NULL)))),
    CONSTRAINT publication_delivery_published_chk CHECK (((state = 'published'::proxima_core.publication_state) = (published_at IS NOT NULL)))
);

CREATE TABLE proxima_core.publication_origin (
    t uuid NOT NULL,
    original_owner_id uuid NOT NULL,
    source_id text
);

CREATE TABLE proxima_core.publication_outbox (
    t uuid NOT NULL,
    owner_id uuid NOT NULL,
    schema_id text NOT NULL,
    schema_version integer NOT NULL,
    event_type text NOT NULL,
    event_id text NOT NULL,
    envelope bytea NOT NULL,
    envelope_digest bytea NOT NULL,
    state proxima_core.publication_record_state DEFAULT 'pending'::proxima_core.publication_record_state NOT NULL,
    published_at timestamp with time zone,
    published_stream text,
    published_seq bigint,
    captured_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT publication_outbox_digest_len_chk CHECK ((octet_length(envelope_digest) = 32)),
    CONSTRAINT publication_outbox_published_chk CHECK (((state = 'published'::proxima_core.publication_record_state) = (published_at IS NOT NULL)))
);

COMMENT ON TABLE proxima_core.installation IS 'Payload-free deployment identity, minted at install and never updated. Stamped on published broker messages so retained-copy cleanup can prove a message is this installation''s before reading an absent origin row as a revocation.';

COMMENT ON TABLE proxima_core.publication_origin IS 'Payload-free immutable original publication identity. Lifecycle erasure may delete the row; payload and broker delivery state remain exclusively in publication_outbox.';

-- Primary, unique and check constraints

ALTER TABLE ONLY proxima_core.agent_derivation_v1
    ADD CONSTRAINT agent_derivation_v1_pkey PRIMARY KEY (t);

ALTER TABLE ONLY proxima_core.agent_note_v1
    ADD CONSTRAINT agent_note_v1_pkey PRIMARY KEY (t);

ALTER TABLE ONLY proxima_core.blob
    ADD CONSTRAINT blob_owner_id_schema_id_content_hash_key UNIQUE (owner_id, schema_id, content_hash);

ALTER TABLE ONLY proxima_core.blob
    ADD CONSTRAINT blob_pkey PRIMARY KEY (blob_id);

ALTER TABLE ONLY proxima_core.blob_uploads
    ADD CONSTRAINT blob_uploads_pkey PRIMARY KEY (upload_id);

ALTER TABLE ONLY proxima_core.closed_handle
    ADD CONSTRAINT closed_handle_pkey PRIMARY KEY (handle);

ALTER TABLE ONLY proxima_core.cold_purge_pending
    ADD CONSTRAINT cold_purge_pending_pkey PRIMARY KEY (object_key);

ALTER TABLE ONLY proxima_core.content
    ADD CONSTRAINT content_owner_id_schema_id_content_hash_key UNIQUE (owner_id, schema_id, content_hash);

ALTER TABLE ONLY proxima_core.content
    ADD CONSTRAINT content_pkey PRIMARY KEY (content_id);

ALTER TABLE ONLY proxima_core.cooled
    ADD CONSTRAINT cooled_pkey PRIMARY KEY (t);

ALTER TABLE ONLY proxima_core.delegated_authority_grants
    ADD CONSTRAINT delegated_authority_grants_pkey PRIMARY KEY (delegation_id);

ALTER TABLE ONLY proxima_core.embedding_heads
    ADD CONSTRAINT embedding_heads_pkey PRIMARY KEY (entity_id, model_id, dim);

ALTER TABLE ONLY proxima_core.embedding_jobs
    ADD CONSTRAINT embedding_jobs_owner_id_entity_id_model_id_dim_key UNIQUE (owner_id, entity_id, model_id, dim);

ALTER TABLE ONLY proxima_core.embedding_jobs
    ADD CONSTRAINT embedding_jobs_pkey PRIMARY KEY (job_id);

ALTER TABLE ONLY proxima_core.embeddings
    ADD CONSTRAINT embeddings_pkey PRIMARY KEY (entity_id, model_id, dim, embedding_version, chunk_ordinal);

ALTER TABLE ONLY proxima_core.erased_pin_target
    ADD CONSTRAINT erased_pin_target_pkey PRIMARY KEY (t);

ALTER TABLE ONLY proxima_core.flavor_surface
    ADD CONSTRAINT flavor_surface_pkey PRIMARY KEY (table_name);

ALTER TABLE ONLY proxima_core.goal_change_v1
    ADD CONSTRAINT goal_change_v1_pkey PRIMARY KEY (t);

ALTER TABLE ONLY proxima_core.goal_head
    ADD CONSTRAINT goal_head_handle_owner_key UNIQUE (handle, owner_id);

ALTER TABLE ONLY proxima_core.goal_head
    ADD CONSTRAINT goal_head_pkey PRIMARY KEY (handle);

ALTER TABLE ONLY proxima_core.goal
    ADD CONSTRAINT goal_owner_id_request_id_key UNIQUE (owner_id, request_id);

ALTER TABLE ONLY proxima_core.goal
    ADD CONSTRAINT goal_pkey PRIMARY KEY (handle, t);

ALTER TABLE ONLY proxima_core.goal_replay_declaration
    ADD CONSTRAINT goal_replay_declaration_pkey PRIMARY KEY (goal_t);

ALTER TABLE ONLY proxima_core.goal
    ADD CONSTRAINT goal_t_key UNIQUE (t);

ALTER TABLE ONLY proxima_core.group_edges
    ADD CONSTRAINT group_edges_pkey PRIMARY KEY (child_group_id);

ALTER TABLE ONLY proxima_core.group_lock_request
    ADD CONSTRAINT group_lock_request_pkey PRIMARY KEY (request_id);

ALTER TABLE ONLY proxima_core.group_memberships
    ADD CONSTRAINT group_memberships_pkey PRIMARY KEY (group_id, member_user_id, relation);

ALTER TABLE ONLY proxima_core.group_role_projection
    ADD CONSTRAINT group_role_projection_pkey PRIMARY KEY (member_user_id, group_id);

ALTER TABLE ONLY proxima_core.ingest_keys
    ADD CONSTRAINT ingest_keys_pkey PRIMARY KEY (owner_id, source_id, ingest_key);

ALTER TABLE ONLY proxima_core.installation
    ADD CONSTRAINT installation_pkey PRIMARY KEY (singleton);

ALTER TABLE ONLY proxima_core.interpretation_v1
    ADD CONSTRAINT interpretation_v1_pkey PRIMARY KEY (t);

ALTER TABLE ONLY proxima_core.lexical_default
    ADD CONSTRAINT lexical_default_pkey PRIMARY KEY (singleton);

ALTER TABLE ONLY proxima_core.lexical_languages
    ADD CONSTRAINT lexical_languages_pkey PRIMARY KEY (config);

ALTER TABLE ONLY proxima_core.memory_external_origin
    ADD CONSTRAINT memory_external_origin_pkey PRIMARY KEY (t, store, handle);

ALTER TABLE ONLY proxima_core.memory_head
    ADD CONSTRAINT memory_head_pkey PRIMARY KEY (handle);

ALTER TABLE ONLY proxima_core.memory
    ADD CONSTRAINT memory_pkey PRIMARY KEY (handle, t);

ALTER TABLE ONLY proxima_core.memory
    ADD CONSTRAINT memory_t_key UNIQUE (t);

ALTER TABLE ONLY proxima_core.owner_rls_epoch
    ADD CONSTRAINT owner_rls_epoch_pkey PRIMARY KEY (singleton);

ALTER TABLE ONLY proxima_core.owners
    ADD CONSTRAINT owners_owner_id_kind_key UNIQUE (owner_id, kind);

ALTER TABLE ONLY proxima_core.owners
    ADD CONSTRAINT owners_pkey PRIMARY KEY (owner_id);

ALTER TABLE ONLY proxima_core.publication_consumer
    ADD CONSTRAINT publication_consumer_pkey PRIMARY KEY (name);

ALTER TABLE ONLY proxima_core.publication_delivery
    ADD CONSTRAINT publication_delivery_pkey PRIMARY KEY (t, consumer);

ALTER TABLE ONLY proxima_core.publication_origin
    ADD CONSTRAINT publication_origin_pkey PRIMARY KEY (t);

ALTER TABLE ONLY proxima_core.publication_outbox
    ADD CONSTRAINT publication_outbox_event_id_key UNIQUE (event_id);

ALTER TABLE ONLY proxima_core.publication_outbox
    ADD CONSTRAINT publication_outbox_pkey PRIMARY KEY (t);

ALTER TABLE ONLY proxima_core.sketch
    ADD CONSTRAINT sketch_pkey PRIMARY KEY (t);

ALTER TABLE ONLY proxima_core.source_cursors
    ADD CONSTRAINT source_cursors_pkey PRIMARY KEY (owner_id, source);

ALTER TABLE ONLY proxima_core.task_goal_v1
    ADD CONSTRAINT task_goal_v1_pkey PRIMARY KEY (t);

ALTER TABLE ONLY proxima_core.utterance_v1
    ADD CONSTRAINT utterance_v1_pkey PRIMARY KEY (t);

ALTER TABLE ONLY proxima_core.write_act_v1
    ADD CONSTRAINT write_act_v1_pkey PRIMARY KEY (t);

-- Generated search projection (projection.rs)

CREATE TABLE proxima_core.projection (
    memory_id        uuid      NOT NULL
                     REFERENCES proxima_core.memory (t) ON DELETE CASCADE,
    schema_id        text      NOT NULL,
    owner_id         uuid      NOT NULL
                     REFERENCES proxima_core.owners (owner_id),
    search_tsv       tsvector  NOT NULL,
    tag              text[]    NOT NULL DEFAULT '{}',
    lexical_language regconfig NOT NULL DEFAULT proxima_core.lexical_config()
                     REFERENCES proxima_core.lexical_languages (config),
    PRIMARY KEY (memory_id, schema_id)
);

-- Indexes

CREATE INDEX blob_uploads_object_key_idx ON proxima_core.blob_uploads USING btree (object_key);

CREATE INDEX blob_uploads_owner_status_idx ON proxima_core.blob_uploads USING btree (owner_id, status);

CREATE INDEX blob_uploads_terminal_content_idx ON proxima_core.blob_uploads USING btree (owner_id, content_hash) WHERE (status = ANY (ARRAY['aborted'::proxima_core.blob_upload_status, 'expired'::proxima_core.blob_upload_status]));

CREATE INDEX cold_purge_pending_backend_idx ON proxima_core.cold_purge_pending USING btree (backend, enqueued_at, object_key);

CREATE INDEX cold_purge_pending_owner_idx ON proxima_core.cold_purge_pending USING btree (owner_id, enqueued_at);

CREATE INDEX content_owner_schema_idx ON proxima_core.content USING btree (owner_id, schema_id);

CREATE INDEX cooled_content_id_idx ON proxima_core.cooled USING btree (content_id) WHERE (content_id IS NOT NULL);

CREATE INDEX cooled_owner_id_idx ON proxima_core.cooled USING btree (owner_id);

CREATE INDEX cooled_owner_source_idx ON proxima_core.cooled USING btree (owner_id, source_id) WHERE (source_id IS NOT NULL);

CREATE INDEX core_projection_owner_tsv_gin ON proxima_core.projection USING gin (owner_id, search_tsv);

CREATE INDEX embedding_jobs_pending_claim_idx ON proxima_core.embedding_jobs USING btree (job_id) WHERE (status = 'pending'::proxima_core.embedding_job_status);

CREATE INDEX embeddings_hnsw_d1024 ON proxima_core.embeddings USING hnsw (((vec)::public.vector(1024)) public.vector_cosine_ops) WHERE (dim = 1024);

CREATE INDEX embeddings_hnsw_d1536 ON proxima_core.embeddings USING hnsw (((vec)::public.vector(1536)) public.vector_cosine_ops) WHERE (dim = 1536);

CREATE INDEX embeddings_hnsw_d2048 ON proxima_core.embeddings USING hnsw (((vec)::public.halfvec(2048)) public.halfvec_cosine_ops) WHERE (dim = 2048);

CREATE INDEX embeddings_hnsw_d3072 ON proxima_core.embeddings USING hnsw (((vec)::public.halfvec(3072)) public.halfvec_cosine_ops) WHERE (dim = 3072);

CREATE INDEX embeddings_hnsw_d384 ON proxima_core.embeddings USING hnsw (((vec)::public.vector(384)) public.vector_cosine_ops) WHERE (dim = 384);

CREATE INDEX embeddings_hnsw_d768 ON proxima_core.embeddings USING hnsw (((vec)::public.vector(768)) public.vector_cosine_ops) WHERE (dim = 768);

CREATE INDEX embeddings_owner_model_idx ON proxima_core.embeddings USING btree (owner_id, model_id, dim);

CREATE INDEX goal_dependency_gin ON proxima_core.goal USING gin (dependency_t);

CREATE INDEX goal_evidence_gin ON proxima_core.goal USING gin (evidence_t);

CREATE INDEX goal_head_owner_schema_idx ON proxima_core.goal_head USING btree (owner_id, schema_id, handle);

CREATE INDEX goal_owner_handle_t_idx ON proxima_core.goal USING btree (owner_id, handle, t DESC);

CREATE INDEX goal_owner_state_t_idx ON proxima_core.goal USING btree (owner_id, state, t DESC);

CREATE INDEX group_edges_parent_idx ON proxima_core.group_edges USING btree (parent_group_id);

CREATE INDEX group_memberships_member_user_id_idx ON proxima_core.group_memberships USING btree (member_user_id, group_id, relation);

CREATE INDEX group_role_projection_group_idx ON proxima_core.group_role_projection USING btree (group_id, member_user_id);

CREATE INDEX idx_agent_note_v1_nk ON proxima_core.agent_note_v1 USING btree (note_id);

CREATE INDEX memory_blob_id_idx ON proxima_core.memory USING btree (blob_id) WHERE (blob_id IS NOT NULL);

CREATE INDEX memory_content_id_idx ON proxima_core.memory USING btree (content_id) WHERE (content_id IS NOT NULL);

CREATE INDEX memory_goal_refs_gin ON proxima_core.memory USING gin (goal_refs);

CREATE INDEX memory_head_owner_kind_idx ON proxima_core.memory_head USING btree (owner_id, kind, handle);

CREATE INDEX memory_head_owner_schema_idx ON proxima_core.memory_head USING btree (owner_id, schema_id, handle);

CREATE INDEX memory_head_owner_schema_t_idx ON proxima_core.memory_head USING btree (owner_id, schema_id, t DESC) INCLUDE (handle, kind);

CREATE INDEX memory_head_owner_t_idx ON proxima_core.memory_head USING btree (owner_id, t DESC) INCLUDE (handle, kind);

CREATE INDEX memory_origins_gin ON proxima_core.memory USING gin (origins);

CREATE INDEX memory_owner_handle_t_idx ON proxima_core.memory USING btree (owner_id, handle, t DESC);

CREATE INDEX memory_owner_schema_t_idx ON proxima_core.memory USING btree (owner_id, schema_id, t DESC);

CREATE INDEX memory_owner_t_handle_idx ON proxima_core.memory USING btree (owner_id, t, handle);

CREATE INDEX memory_refs_gin ON proxima_core.memory USING gin (refs);

CREATE INDEX owners_kind_idx ON proxima_core.owners USING btree (kind, owner_id);

CREATE INDEX publication_delivery_consumer_idx ON proxima_core.publication_delivery USING btree (consumer);

CREATE INDEX publication_delivery_pending_idx ON proxima_core.publication_delivery USING btree (consumer, attempts, t) WHERE (state <> 'published'::proxima_core.publication_state);

CREATE INDEX publication_origin_owner_source_idx ON proxima_core.publication_origin USING btree (original_owner_id, source_id, t);

CREATE INDEX publication_outbox_owner_idx ON proxima_core.publication_outbox USING btree (owner_id);

CREATE INDEX publication_outbox_pending_idx ON proxima_core.publication_outbox USING btree (t) WHERE (state <> 'published'::proxima_core.publication_record_state);

CREATE INDEX sketch_owner_t_idx ON proxima_core.sketch USING btree (owner_id, t DESC);

-- Foreign keys

ALTER TABLE ONLY proxima_core.agent_derivation_v1
    ADD CONSTRAINT agent_derivation_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);

ALTER TABLE ONLY proxima_core.agent_note_v1
    ADD CONSTRAINT agent_note_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);

ALTER TABLE ONLY proxima_core.blob
    ADD CONSTRAINT blob_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.blob_uploads
    ADD CONSTRAINT blob_uploads_blob_id_fkey FOREIGN KEY (blob_id) REFERENCES proxima_core.blob(blob_id);

ALTER TABLE ONLY proxima_core.blob_uploads
    ADD CONSTRAINT blob_uploads_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.cold_purge_pending
    ADD CONSTRAINT cold_purge_pending_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.content
    ADD CONSTRAINT content_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.cooled
    ADD CONSTRAINT cooled_blob_id_fkey FOREIGN KEY (blob_id) REFERENCES proxima_core.blob(blob_id);

ALTER TABLE ONLY proxima_core.cooled
    ADD CONSTRAINT cooled_content_id_fkey FOREIGN KEY (content_id) REFERENCES proxima_core.content(content_id);

ALTER TABLE ONLY proxima_core.cooled
    ADD CONSTRAINT cooled_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.embedding_heads
    ADD CONSTRAINT embedding_heads_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.embedding_jobs
    ADD CONSTRAINT embedding_jobs_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.embeddings
    ADD CONSTRAINT embeddings_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.goal_change_v1
    ADD CONSTRAINT goal_change_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);

ALTER TABLE ONLY proxima_core.goal
    ADD CONSTRAINT goal_handle_fkey FOREIGN KEY (handle) REFERENCES proxima_core.goal_head(handle);

ALTER TABLE ONLY proxima_core.goal_head
    ADD CONSTRAINT goal_head_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.goal
    ADD CONSTRAINT goal_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.goal
    ADD CONSTRAINT goal_owner_matches_head_fk FOREIGN KEY (handle, owner_id) REFERENCES proxima_core.goal_head(handle, owner_id) NOT VALID;

ALTER TABLE ONLY proxima_core.goal_replay_declaration
    ADD CONSTRAINT goal_replay_declaration_goal_t_fkey FOREIGN KEY (goal_t) REFERENCES proxima_core.goal(t) ON DELETE CASCADE;

ALTER TABLE ONLY proxima_core.group_edges
    ADD CONSTRAINT group_edges_child_group_fk FOREIGN KEY (child_group_id, child_group_kind) REFERENCES proxima_core.owners(owner_id, kind) DEFERRABLE;

ALTER TABLE ONLY proxima_core.group_edges
    ADD CONSTRAINT group_edges_parent_group_fk FOREIGN KEY (parent_group_id, parent_group_kind) REFERENCES proxima_core.owners(owner_id, kind) DEFERRABLE;

ALTER TABLE ONLY proxima_core.group_memberships
    ADD CONSTRAINT group_memberships_group_registered_fk FOREIGN KEY (group_id, group_kind) REFERENCES proxima_core.owners(owner_id, kind);

ALTER TABLE ONLY proxima_core.group_role_projection
    ADD CONSTRAINT group_role_projection_group_id_fkey FOREIGN KEY (group_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.ingest_keys
    ADD CONSTRAINT ingest_keys_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.interpretation_v1
    ADD CONSTRAINT interpretation_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);

ALTER TABLE ONLY proxima_core.lexical_default
    ADD CONSTRAINT lexical_default_config_fkey FOREIGN KEY (config) REFERENCES proxima_core.lexical_languages(config);

ALTER TABLE ONLY proxima_core.memory
    ADD CONSTRAINT memory_blob_id_fkey FOREIGN KEY (blob_id) REFERENCES proxima_core.blob(blob_id);

ALTER TABLE ONLY proxima_core.memory
    ADD CONSTRAINT memory_content_id_fkey FOREIGN KEY (content_id) REFERENCES proxima_core.content(content_id);

ALTER TABLE ONLY proxima_core.memory
    ADD CONSTRAINT memory_handle_fkey FOREIGN KEY (handle) REFERENCES proxima_core.memory_head(handle);

ALTER TABLE ONLY proxima_core.memory_head
    ADD CONSTRAINT memory_head_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.memory
    ADD CONSTRAINT memory_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.publication_delivery
    ADD CONSTRAINT publication_delivery_consumer_fkey FOREIGN KEY (consumer) REFERENCES proxima_core.publication_consumer(name) ON DELETE CASCADE;

ALTER TABLE ONLY proxima_core.publication_delivery
    ADD CONSTRAINT publication_delivery_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.publication_outbox(t) ON DELETE CASCADE;

ALTER TABLE ONLY proxima_core.publication_origin
    ADD CONSTRAINT publication_origin_original_owner_id_fkey FOREIGN KEY (original_owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.publication_outbox
    ADD CONSTRAINT publication_outbox_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.sketch
    ADD CONSTRAINT sketch_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id);

ALTER TABLE ONLY proxima_core.task_goal_v1
    ADD CONSTRAINT task_goal_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.goal(t);

ALTER TABLE ONLY proxima_core.utterance_v1
    ADD CONSTRAINT utterance_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);

ALTER TABLE ONLY proxima_core.write_act_v1
    ADD CONSTRAINT write_act_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);

-- Core seed rows

-- Insertion order is the old chain's; owner-erase goldens compare ctid order.
INSERT INTO proxima_core.flavor_surface (table_name, flavor_id) VALUES
    ('proxima_core.agent_derivation_v1', 'core'),
    ('proxima_core.agent_note_v1', 'core'),
    ('proxima_core.interpretation_v1', 'core'),
    ('proxima_core.task_goal_v1', 'core'),
    ('proxima_core.utterance_v1', 'core'),
    ('proxima_core.write_act_v1', 'core'),
    ('proxima_core.goal_change_v1', 'core');

INSERT INTO proxima_core.lexical_languages (config)
VALUES ('english'::regconfig);

INSERT INTO proxima_core.lexical_default (singleton, config)
VALUES (true, 'english'::regconfig);

INSERT INTO proxima_core.installation (singleton) VALUES (true);

INSERT INTO proxima_core.owner_rls_epoch (singleton, epoch)
VALUES (true, 1);

INSERT INTO proxima_core.publication_consumer (name) VALUES ('default');

-- Row-level security enablement

ALTER TABLE proxima_core.agent_derivation_v1 ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.agent_note_v1 ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.blob ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.blob_uploads ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.closed_handle ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.cold_purge_pending ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.content ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.cooled ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.delegated_authority_grants ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.embedding_heads ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.embedding_jobs ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.embeddings ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.erased_pin_target ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.flavor_surface ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.goal ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.goal_change_v1 ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.goal_head ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.goal_replay_declaration ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.group_edges ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.group_lock_request ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.group_memberships ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.group_role_projection ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.ingest_keys ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.installation ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.interpretation_v1 ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.lexical_default ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.lexical_languages ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.memory ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.memory_external_origin ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.memory_head ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.owner_rls_epoch ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.owners ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.projection ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.publication_consumer ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.publication_delivery ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.publication_origin ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.publication_outbox ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.sketch ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.source_cursors ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.task_goal_v1 ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.utterance_v1 ENABLE ROW LEVEL SECURITY;

ALTER TABLE proxima_core.write_act_v1 ENABLE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.agent_derivation_v1 FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.agent_note_v1 FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.blob FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.blob_uploads FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.closed_handle FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.cold_purge_pending FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.content FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.cooled FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.delegated_authority_grants FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.embedding_heads FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.embedding_jobs FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.embeddings FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.erased_pin_target FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.flavor_surface FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.goal FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.goal_change_v1 FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.goal_head FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.goal_replay_declaration FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.group_edges FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.group_lock_request FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.group_memberships FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.group_role_projection FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.ingest_keys FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.installation FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.interpretation_v1 FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.lexical_default FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.lexical_languages FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.memory FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.memory_external_origin FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.memory_head FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.owner_rls_epoch FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.owners FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.projection FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.publication_consumer FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.publication_delivery FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.publication_origin FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.publication_outbox FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.sketch FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.source_cursors FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.task_goal_v1 FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.utterance_v1 FORCE ROW LEVEL SECURITY;

ALTER TABLE ONLY proxima_core.write_act_v1 FORCE ROW LEVEL SECURITY;

-- Public privileges

GRANT SELECT ON TABLE proxima_core.group_edges TO PUBLIC;

GRANT SELECT,INSERT ON TABLE proxima_core.group_lock_request TO PUBLIC;

GRANT SELECT ON TABLE proxima_core.group_role_projection TO PUBLIC;
