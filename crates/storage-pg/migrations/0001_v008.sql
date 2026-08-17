-- Proxima core schema — v0.0.8 timeseries (one file, fresh CREATE).
-- No ALTER of 0001–0021. Existing databases must reset.

CREATE EXTENSION IF NOT EXISTS vector;

CREATE SCHEMA proxima_core;

CREATE TYPE proxima_core.owner_kind AS ENUM (
    'world',
    'personal',
    'group'
);

CREATE TYPE proxima_core.memory_kind AS ENUM (
    'fact',
    'abstraction',
    'perspective'
);

CREATE TYPE proxima_core.announce_op AS ENUM (
    'append',
    'forget',
    'erase'
);

CREATE TYPE proxima_core.announce_entity AS ENUM (
    'memory',
    'goal'
);

CREATE TYPE proxima_core.goal_state AS ENUM (
    'Active',
    'Paused',
    'Achieved',
    'Abandoned'
);

CREATE TYPE proxima_core.wake_trigger_kind AS ENUM (
    'fact_schema',
    'fact_memory'
);

CREATE TYPE proxima_core.membership_relation AS ENUM (
    'admin',
    'editor',
    'viewer',
    'ingest'
);

CREATE TYPE proxima_core.task_priority AS ENUM (
    'Low',
    'Medium',
    'High'
);

CREATE TYPE proxima_core.interpretation_subject_kind AS ENUM (
    'Fact',
    'Abstraction',
    'Perspective'
);

CREATE TYPE proxima_core.blob_upload_status AS ENUM (
    'pending',
    'completed',
    'aborted',
    'expired'
);

CREATE TABLE proxima_core.owners (
    owner_id uuid PRIMARY KEY,
    kind proxima_core.owner_kind NOT NULL,
    CONSTRAINT owners_world_kind_chk CHECK (
        (kind = 'world') = (owner_id = '00000000-0000-0000-0000-000000000001'::uuid)
    )
);

INSERT INTO proxima_core.owners (owner_id, kind)
VALUES ('00000000-0000-0000-0000-000000000001'::uuid, 'world');

CREATE INDEX owners_kind_idx
    ON proxima_core.owners (kind, owner_id);

CREATE TABLE proxima_core.blob (
    blob_id uuid PRIMARY KEY DEFAULT uuidv7(),
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    schema_id text NOT NULL,
    content_hash bytea NOT NULL,
    UNIQUE (owner_id, schema_id, content_hash),
    CONSTRAINT blob_hash_len_chk CHECK (octet_length(content_hash) = 32)
);

CREATE TABLE proxima_core.closed_handle (
    handle uuid PRIMARY KEY,
    closed_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE proxima_core.memory_head (
    handle uuid PRIMARY KEY,
    kind proxima_core.memory_kind NOT NULL,
    schema_id text NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    t uuid NOT NULL
);

CREATE INDEX memory_head_owner_schema_idx
    ON proxima_core.memory_head (owner_id, schema_id, handle);

CREATE INDEX memory_head_owner_kind_idx
    ON proxima_core.memory_head (owner_id, kind, handle);

CREATE TABLE proxima_core.memory (
    handle uuid NOT NULL REFERENCES proxima_core.memory_head (handle),
    t uuid NOT NULL DEFAULT uuidv7(),
    kind proxima_core.memory_kind NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    schema_id text NOT NULL,
    source_id text,
    ingest_key text,
    blob_id uuid REFERENCES proxima_core.blob (blob_id),
    origins uuid[] NOT NULL DEFAULT '{}',
    refs uuid[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (handle, t),
    UNIQUE (t),
    CONSTRAINT memory_fact_source_chk CHECK (
        (kind = 'fact' AND (source_id IS NULL) = (ingest_key IS NULL))
        OR (kind <> 'fact' AND source_id IS NULL AND ingest_key IS NULL)
    ),
    CONSTRAINT memory_fact_origins_chk CHECK (
        kind <> 'fact' OR origins = '{}'
    ),
    CONSTRAINT memory_blob_fa_chk CHECK (
        blob_id IS NULL OR kind IN ('fact', 'abstraction')
    ),
    CONSTRAINT memory_origins_no_null_chk CHECK (array_position(origins, NULL) IS NULL),
    CONSTRAINT memory_refs_no_null_chk CHECK (array_position(refs, NULL) IS NULL)
);

CREATE INDEX memory_owner_handle_t_idx
    ON proxima_core.memory (owner_id, handle, t DESC);

CREATE INDEX memory_owner_t_handle_idx
    ON proxima_core.memory (owner_id, t, handle);

CREATE INDEX memory_origins_gin
    ON proxima_core.memory USING gin (origins);

CREATE INDEX memory_refs_gin
    ON proxima_core.memory USING gin (refs);

CREATE TABLE proxima_core.ingest_keys (
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    source_id text NOT NULL,
    ingest_key text NOT NULL,
    t uuid NOT NULL,
    PRIMARY KEY (owner_id, source_id, ingest_key)
);

CREATE TABLE proxima_core.announce (
    seq uuid PRIMARY KEY DEFAULT uuidv7(),
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    op proxima_core.announce_op NOT NULL,
    entity proxima_core.announce_entity NOT NULL,
    handle uuid NOT NULL,
    t uuid NOT NULL
);

CREATE INDEX announce_owner_seq_idx
    ON proxima_core.announce (owner_id, seq);

CREATE TABLE proxima_core.wake_config (
    wake_id uuid PRIMARY KEY DEFAULT uuidv7(),
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    trigger_kind proxima_core.wake_trigger_kind NOT NULL,
    trigger_schema_id text,
    trigger_t uuid,
    tool_ids text[] NOT NULL,
    prompt text NOT NULL,
    hard_memory_t uuid[] NOT NULL DEFAULT '{}',
    CONSTRAINT wake_trigger_xor_chk CHECK (
        (trigger_kind = 'fact_schema' AND trigger_schema_id IS NOT NULL AND trigger_t IS NULL)
        OR (trigger_kind = 'fact_memory' AND trigger_t IS NOT NULL AND trigger_schema_id IS NULL)
    ),
    CONSTRAINT wake_tools_chk CHECK (
        array_length(tool_ids, 1) >= 1 AND array_position(tool_ids, NULL) IS NULL
    ),
    CONSTRAINT wake_prompt_chk CHECK (length(btrim(prompt)) > 0),
    CONSTRAINT wake_hard_no_null_chk CHECK (array_position(hard_memory_t, NULL) IS NULL)
);

CREATE TABLE proxima_core.goal_head (
    handle uuid PRIMARY KEY,
    schema_id text NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    t uuid NOT NULL
);

CREATE INDEX goal_head_owner_schema_idx
    ON proxima_core.goal_head (owner_id, schema_id, handle);

CREATE TABLE proxima_core.goal (
    handle uuid NOT NULL REFERENCES proxima_core.goal_head (handle),
    t uuid NOT NULL DEFAULT uuidv7(),
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    title text NOT NULL,
    state proxima_core.goal_state NOT NULL,
    request_id text NOT NULL,
    close_fact_t uuid,
    assignment_t uuid,
    dependency_t uuid[] NOT NULL DEFAULT '{}',
    evidence_t uuid[] NOT NULL DEFAULT '{}',
    wake_id uuid REFERENCES proxima_core.wake_config (wake_id) ON DELETE RESTRICT,
    write_act_t uuid,
    PRIMARY KEY (handle, t),
    UNIQUE (t),
    UNIQUE (owner_id, request_id),
    CONSTRAINT goal_title_nonblank_chk CHECK (length(btrim(title)) > 0),
    CONSTRAINT goal_terminal_close_chk CHECK (
        (state IN ('Achieved', 'Abandoned')) = (close_fact_t IS NOT NULL)
    ),
    CONSTRAINT goal_arrays_no_null_chk CHECK (
        array_position(dependency_t, NULL) IS NULL
        AND array_position(evidence_t, NULL) IS NULL
    )
);

CREATE INDEX goal_owner_handle_t_idx
    ON proxima_core.goal (owner_id, handle, t DESC);

CREATE INDEX goal_wake_idx
    ON proxima_core.goal (wake_id) WHERE wake_id IS NOT NULL;

CREATE INDEX goal_dependency_gin
    ON proxima_core.goal USING gin (dependency_t);

CREATE INDEX goal_evidence_gin
    ON proxima_core.goal USING gin (evidence_t);

CREATE TABLE proxima_core.cooled (
    t uuid PRIMARY KEY,
    handle uuid NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    kind proxima_core.memory_kind NOT NULL,
    object_key text NOT NULL,
    cooled_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE proxima_core.group_memberships (
    group_id uuid NOT NULL,
    member_user_id uuid NOT NULL,
    relation proxima_core.membership_relation NOT NULL,
    PRIMARY KEY (group_id, member_user_id, relation)
);

CREATE TABLE proxima_core.lexical_languages (
    config regconfig PRIMARY KEY
);

INSERT INTO proxima_core.lexical_languages (config)
VALUES ('english'::regconfig)
ON CONFLICT DO NOTHING;

CREATE FUNCTION proxima_core.lexical_scrub(txt text) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT regexp_replace(
       regexp_replace(txt, '[[:punct:]]+', ' ', 'g'),
       '\m[[:alnum:]]{255}[[:alnum:]]+\M', ' ', 'g') $$;

CREATE FUNCTION proxima_core.lexical_config() RETURNS regconfig
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS
$$ SELECT 'english'::regconfig $$;

CREATE FUNCTION proxima_core.lexical_tsv(txt text) RETURNS tsvector
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT to_tsvector(proxima_core.lexical_config(), proxima_core.lexical_scrub(txt)) $$;

CREATE FUNCTION proxima_core.lexical_tsv(config regconfig, txt text) RETURNS tsvector
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT to_tsvector(config, proxima_core.lexical_scrub(txt)) $$;

CREATE FUNCTION proxima_core.lexical_text_array(parts text[]) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT NULLIF(array_to_string(parts, ' '), '') $$;

CREATE FUNCTION proxima_core.lexical_join(VARIADIC parts text[]) RETURNS text
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS
$$ SELECT NULLIF(concat_ws(' ', VARIADIC parts), '') $$;

CREATE FUNCTION proxima_core.lexical_query_text(config regconfig, query_text text)
RETURNS text
LANGUAGE sql STABLE PARALLEL SAFE AS
$$ SELECT CASE
       WHEN config = 'simple'::regconfig THEN
           (SELECT COALESCE(string_agg(tok, ' '), '')
              FROM regexp_split_to_table(query_text, '\s+') AS tok
             WHERE tok <> ''
               AND to_tsvector(proxima_core.lexical_config(), tok) <> '')
       ELSE query_text
   END $$;

CREATE AGGREGATE proxima_core.tsquery_or_agg(tsquery) (
    SFUNC = pg_catalog.tsquery_or,
    STYPE = tsquery,
    COMBINEFUNC = pg_catalog.tsquery_or,
    PARALLEL = SAFE
);

CREATE FUNCTION proxima_core.set_lexical_config(cfg text) RETURNS void
LANGUAGE sql VOLATILE AS
$$ INSERT INTO proxima_core.lexical_languages (config)
   VALUES (cfg::regconfig)
   ON CONFLICT DO NOTHING $$;

CREATE FUNCTION proxima_core.remember_lexical_language() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO proxima_core.lexical_languages (config)
    VALUES (NEW.lexical_language)
    ON CONFLICT DO NOTHING;
    RETURN NEW;
END;
$$;

CREATE TABLE proxima_core.mcp_call_logged_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    tool_name text NOT NULL,
    actor_oid text NOT NULL,
    actor_upn text NOT NULL,
    ok boolean NOT NULL,
    error text,
    latency_ms bigint NOT NULL,
    io_byte_len bigint NOT NULL,
    io_truncated boolean NOT NULL,
    io_content_hash bytea NOT NULL
);

CREATE TABLE proxima_core.embeddings (
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    embedding_version int NOT NULL DEFAULT 1,
    vec vector(1024) NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    PRIMARY KEY (entity_id, model_id, embedding_version)
);

CREATE INDEX idx_embeddings_vec_hnsw
    ON proxima_core.embeddings USING hnsw (vec vector_cosine_ops);

CREATE TABLE proxima_core.embedding_heads (
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    embedding_version int NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    PRIMARY KEY (entity_id, model_id)
);

CREATE TABLE proxima_core.embedding_jobs (
    job_id uuid PRIMARY KEY DEFAULT uuidv7(),
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    status text NOT NULL DEFAULT 'pending',
    UNIQUE (owner_id, entity_id, model_id)
);

CREATE TABLE proxima_core.agent_note_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    note_id uuid NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    tags text[] NOT NULL DEFAULT '{}',
    idempotency_key text,
    lexical_language regconfig NOT NULL DEFAULT proxima_core.lexical_config(),
    search_tsv tsvector GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(
            lexical_language,
            proxima_core.lexical_join(
                VARIADIC ARRAY[
                    NULLIF(title, ''),
                    NULLIF(body, ''),
                    proxima_core.lexical_text_array(tags)
                ]
            )
        )
    ) STORED,
    embed_text text GENERATED ALWAYS AS (
        proxima_core.lexical_join(
            VARIADIC ARRAY[
                NULLIF(title, ''),
                NULLIF(body, ''),
                proxima_core.lexical_text_array(tags)
            ]
        )
    ) STORED
);

CREATE INDEX agent_note_v1_search_tsv_gin
    ON proxima_core.agent_note_v1 USING gin (search_tsv);

CREATE TABLE proxima_core.utterance_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    speaker text NOT NULL,
    conversation_id text NOT NULL,
    text text NOT NULL,
    lexical_language regconfig NOT NULL DEFAULT proxima_core.lexical_config(),
    search_tsv tsvector GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(lexical_language, NULLIF(text, ''))
    ) STORED,
    embed_text text GENERATED ALWAYS AS (NULLIF(text, '')) STORED
);

CREATE INDEX utterance_v1_search_tsv_gin
    ON proxima_core.utterance_v1 USING gin (search_tsv);

CREATE TABLE proxima_core.agent_derivation_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    title text NOT NULL,
    body text NOT NULL,
    tags text[] NOT NULL DEFAULT '{}',
    idempotency_key text,
    source_memory_ids uuid[] NOT NULL DEFAULT '{}',
    model_id text NOT NULL,
    client_name text NOT NULL,
    client_version text NOT NULL,
    lexical_language regconfig NOT NULL DEFAULT proxima_core.lexical_config(),
    search_tsv tsvector GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(
            lexical_language,
            proxima_core.lexical_join(
                VARIADIC ARRAY[
                    NULLIF(title, ''),
                    NULLIF(body, ''),
                    proxima_core.lexical_text_array(tags)
                ]
            )
        )
    ) STORED,
    embed_text text GENERATED ALWAYS AS (
        proxima_core.lexical_join(
            VARIADIC ARRAY[
                NULLIF(title, ''),
                NULLIF(body, ''),
                proxima_core.lexical_text_array(tags)
            ]
        )
    ) STORED
);

CREATE INDEX agent_derivation_v1_search_tsv_gin
    ON proxima_core.agent_derivation_v1 USING gin (search_tsv);

CREATE TABLE proxima_core.interpretation_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    claim text NOT NULL,
    confidence smallint NOT NULL,
    subject_memory_ids uuid[] NOT NULL DEFAULT '{}',
    subject_kinds proxima_core.interpretation_subject_kind[] NOT NULL DEFAULT '{}',
    model_id text NOT NULL,
    client_name text NOT NULL,
    client_version text NOT NULL,
    lexical_language regconfig NOT NULL DEFAULT proxima_core.lexical_config(),
    search_tsv tsvector GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(lexical_language, NULLIF(claim, ''))
    ) STORED,
    embed_text text GENERATED ALWAYS AS (NULLIF(claim, '')) STORED
);

CREATE INDEX interpretation_v1_search_tsv_gin
    ON proxima_core.interpretation_v1 USING gin (search_tsv);

CREATE TRIGGER agent_note_v1_remember_lang
    AFTER INSERT ON proxima_core.agent_note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.remember_lexical_language();

CREATE TRIGGER utterance_v1_remember_lang
    AFTER INSERT ON proxima_core.utterance_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.remember_lexical_language();

CREATE TRIGGER agent_derivation_v1_remember_lang
    AFTER INSERT ON proxima_core.agent_derivation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.remember_lexical_language();

CREATE TRIGGER interpretation_v1_remember_lang
    AFTER INSERT ON proxima_core.interpretation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.remember_lexical_language();

CREATE TABLE proxima_core.task_goal_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.goal (t),
    due_at timestamptz,
    priority proxima_core.task_priority
);

CREATE TABLE proxima_core.blob_uploads (
    upload_id uuid PRIMARY KEY DEFAULT uuidv7(),
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    bucket text NOT NULL,
    object_key text NOT NULL,
    filename text NOT NULL,
    mime text NOT NULL,
    expected_byte_len bigint NOT NULL,
    status proxima_core.blob_upload_status NOT NULL DEFAULT 'pending',
    blob_id uuid REFERENCES proxima_core.blob (blob_id),
    sha256 bytea,
    etag text,
    error_message text,
    expires_at timestamptz NOT NULL,
    completed_at timestamptz,
    aborted_at timestamptz
);

CREATE INDEX blob_uploads_owner_status_idx
    ON proxima_core.blob_uploads (owner_id, status);

CREATE TYPE proxima_core.access_ceiling AS ENUM (
    'none',
    'fact',
    'abstraction',
    'perspective',
    'goal'
);

CREATE TYPE proxima_core.compliance_erase_outcome AS ENUM (
    'Completed',
    'Refused',
    'NotFound',
    'Unauthorized'
);

CREATE TYPE proxima_core.compliance_erase_refusal AS ENUM (
    'OwnerNotAbandoned',
    'WorldOwner',
    'SourceScopeOwnerStillLive',
    'PersonalDropNotVerified',
    'DropProofPortUnavailable',
    'LegalHoldActive'
);

CREATE TABLE proxima_core.owner_legal_holds (
    owner_kind proxima_core.owner_kind NOT NULL,
    owner_id uuid,
    hold_active boolean NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE NULLS NOT DISTINCT (owner_kind, owner_id)
);

CREATE TABLE proxima_core.compliance_audit_log (
    operation_id uuid PRIMARY KEY,
    target_kind text NOT NULL,
    outcome proxima_core.compliance_erase_outcome NOT NULL,
    refusal proxima_core.compliance_erase_refusal,
    owner_ref_digest bytea NOT NULL,
    requester_digest bytea,
    source_scope_digest bytea,
    derived_auth_path text NOT NULL,
    requested_at timestamptz NOT NULL,
    completed_at timestamptz,
    memories_count bigint NOT NULL DEFAULT 0,
    goals_count bigint NOT NULL DEFAULT 0,
    edges_count bigint NOT NULL DEFAULT 0,
    receipts_count bigint NOT NULL DEFAULT 0,
    source_batches_count bigint NOT NULL DEFAULT 0,
    source_cursors_count bigint NOT NULL DEFAULT 0,
    embeddings_count bigint NOT NULL DEFAULT 0,
    embedding_jobs_count bigint NOT NULL DEFAULT 0,
    mcp_call_rows_count bigint NOT NULL DEFAULT 0,
    change_events_count bigint NOT NULL DEFAULT 0,
    redacted_edge_targets_count bigint NOT NULL DEFAULT 0,
    suppressed_keys_count bigint NOT NULL DEFAULT 0,
    delegated_authority_grants_count bigint NOT NULL DEFAULT 0,
    cited_object_purge_pending boolean NOT NULL DEFAULT false
);

CREATE TABLE proxima_core.delegated_authority_grants (
    delegation_id uuid PRIMARY KEY,
    subject_user_id uuid NOT NULL,
    owner_kind proxima_core.owner_kind NOT NULL,
    owner_id uuid,
    tool_name text NOT NULL,
    action_name text,
    read_ceiling proxima_core.access_ceiling NOT NULL,
    write_ceiling proxima_core.access_ceiling NOT NULL,
    expires_at timestamptz NOT NULL,
    auth_epoch bigint NOT NULL,
    issued_at timestamptz NOT NULL,
    revoked_at timestamptz,
    revoked_by_user_id uuid
);

CREATE TABLE proxima_core.source_cursors (
    owner_kind proxima_core.owner_kind NOT NULL,
    owner_id uuid NOT NULL,
    source text NOT NULL,
    cursor bytea NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_kind, owner_id, source)
);

CREATE FUNCTION proxima_core.enforce_row_append_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    RAISE EXCEPTION 'append-only: % does not accept UPDATE', TG_TABLE_NAME
        USING ERRCODE = '25006';
END;
$$;

CREATE FUNCTION proxima_core.memory_align_head() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    head_kind proxima_core.memory_kind;
    head_owner uuid;
    head_schema text;
BEGIN
    SELECT kind, owner_id, schema_id INTO head_kind, head_owner, head_schema
      FROM proxima_core.memory_head
     WHERE handle = NEW.handle;
    IF head_kind IS DISTINCT FROM NEW.kind
       OR head_owner IS DISTINCT FROM NEW.owner_id
       OR head_schema IS DISTINCT FROM NEW.schema_id THEN
        RAISE EXCEPTION 'memory kind/owner/schema must equal memory_head'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION proxima_core.memory_head_t_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id THEN
        RAISE EXCEPTION 'memory_head is frozen except t and owner_id'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;

-- Content is append-only. `owner_id` may move: publish-to-World is a
-- series transfer (MemoryHeadAligned), not a new (handle, t).
CREATE FUNCTION proxima_core.memory_owner_or_append_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.t IS DISTINCT FROM OLD.t
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
       OR NEW.source_id IS DISTINCT FROM OLD.source_id
       OR NEW.ingest_key IS DISTINCT FROM OLD.ingest_key
       OR NEW.blob_id IS DISTINCT FROM OLD.blob_id
       OR NEW.origins IS DISTINCT FROM OLD.origins
       OR NEW.refs IS DISTINCT FROM OLD.refs THEN
        RAISE EXCEPTION 'append-only: % does not accept UPDATE', TG_TABLE_NAME
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION proxima_core.memory_pin_checks() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    pin uuid;
    pin_kind proxima_core.memory_kind;
    pin_handle uuid;
BEGIN
    FOREACH pin IN ARRAY NEW.origins || NEW.refs LOOP
        SELECT kind, handle INTO pin_kind, pin_handle
          FROM proxima_core.memory
         WHERE t = pin;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'pin % does not exist', pin USING ERRCODE = '23503';
        END IF;
        IF EXISTS (SELECT 1 FROM proxima_core.closed_handle WHERE handle = pin_handle) THEN
            RAISE EXCEPTION 'closed_handle: no new pin to %', pin_handle USING ERRCODE = '23514';
        END IF;
    END LOOP;

    IF NEW.kind = 'abstraction' THEN
        FOREACH pin IN ARRAY NEW.origins LOOP
            SELECT kind INTO pin_kind FROM proxima_core.memory WHERE t = pin;
            IF pin_kind NOT IN ('fact', 'abstraction') THEN
                RAISE EXCEPTION 'abstraction origins must be fact or abstraction t' USING ERRCODE = '23514';
            END IF;
        END LOOP;
    ELSIF NEW.kind = 'perspective' AND NEW.origins <> '{}' THEN
        FOREACH pin IN ARRAY NEW.origins LOOP
            SELECT kind INTO pin_kind FROM proxima_core.memory WHERE t = pin;
            IF pin_kind IS DISTINCT FROM 'abstraction' THEN
                RAISE EXCEPTION 'perspective origins must be abstraction t' USING ERRCODE = '23514';
            END IF;
        END LOOP;
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION proxima_core.goal_head_t_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id THEN
        RAISE EXCEPTION 'goal_head is frozen except t and owner_id'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION proxima_core.goal_owner_or_append_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.t IS DISTINCT FROM OLD.t
       OR NEW.title IS DISTINCT FROM OLD.title
       OR NEW.state IS DISTINCT FROM OLD.state
       OR NEW.request_id IS DISTINCT FROM OLD.request_id
       OR NEW.close_fact_t IS DISTINCT FROM OLD.close_fact_t
       OR NEW.assignment_t IS DISTINCT FROM OLD.assignment_t
       OR NEW.dependency_t IS DISTINCT FROM OLD.dependency_t
       OR NEW.evidence_t IS DISTINCT FROM OLD.evidence_t
       OR NEW.wake_id IS DISTINCT FROM OLD.wake_id
       OR NEW.write_act_t IS DISTINCT FROM OLD.write_act_t THEN
        RAISE EXCEPTION 'append-only: % does not accept UPDATE', TG_TABLE_NAME
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION proxima_core.goal_no_later_after_terminal() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM proxima_core.goal g
         WHERE g.handle = NEW.handle
           AND g.state IN ('Achieved', 'Abandoned')
           AND g.t <> NEW.t
    ) THEN
        RAISE EXCEPTION 'terminal goal admits no later t' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER memory_append_only
    BEFORE UPDATE ON proxima_core.memory
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.memory_owner_or_append_only();

CREATE TRIGGER ingest_keys_append_only
    BEFORE UPDATE ON proxima_core.ingest_keys
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

CREATE TRIGGER announce_append_only
    BEFORE UPDATE ON proxima_core.announce
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

CREATE TRIGGER owners_append_only
    BEFORE UPDATE ON proxima_core.owners
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

CREATE TRIGGER memory_align_head
    BEFORE INSERT ON proxima_core.memory
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.memory_align_head();

CREATE TRIGGER memory_head_t_only
    BEFORE UPDATE ON proxima_core.memory_head
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.memory_head_t_only();

CREATE TRIGGER memory_pin_checks
    BEFORE INSERT ON proxima_core.memory
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.memory_pin_checks();

CREATE TRIGGER goal_append_only
    BEFORE UPDATE ON proxima_core.goal
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.goal_owner_or_append_only();

CREATE TRIGGER goal_head_t_only
    BEFORE UPDATE ON proxima_core.goal_head
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.goal_head_t_only();

CREATE TRIGGER goal_no_later_after_terminal
    BEFORE INSERT ON proxima_core.goal
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.goal_no_later_after_terminal();
