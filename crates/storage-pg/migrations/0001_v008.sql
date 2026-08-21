-- Proxima core schema — v0.0.8 timeseries (one file, fresh CREATE).
-- No ALTER of 0001–0021. Existing databases must reset.

CREATE EXTENSION IF NOT EXISTS vector;
-- The projection's index is `gin (owner_id, search_tsv)`. `btree_gin` is
-- what lets a uuid sit in a GIN index beside a tsvector: "for queries that
-- test both a GIN-indexable column and a B-tree-indexable column, it might
-- be more efficient to create a multicolumn GIN index that uses one of
-- these operator classes than to create two separate indexes that would
-- have to be combined via bitmap ANDing" (PostgreSQL F.9).
CREATE EXTENSION IF NOT EXISTS btree_gin;

CREATE SCHEMA proxima_core;

CREATE TYPE proxima_core.owner_kind AS ENUM (
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
    'erase',
    'transfer'
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
    kind proxima_core.owner_kind NOT NULL
);

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

CREATE TABLE proxima_core.content (
    content_id uuid PRIMARY KEY DEFAULT uuidv7(),
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    schema_id text NOT NULL,
    content_hash bytea NOT NULL,
    UNIQUE (owner_id, schema_id, content_hash),
    CONSTRAINT content_hash_len_chk CHECK (octet_length(content_hash) = 32)
);

CREATE INDEX content_owner_schema_idx
    ON proxima_core.content (owner_id, schema_id);

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

-- One row per sidecar surface a flavor's contract declares.
--
-- `memory.sidecar_tables` is a row-stamp: it answers "what did THIS row
-- actually write". The registry answers "what could this schema have
-- written". Those are different questions with different failure modes, and
-- until this table existed nothing related the two: a stamp naming a table
-- no flavor declares was accepted at write time and then quietly skipped by
-- erase, export, forget and hydrate, because each of those walks the
-- registry. A row nobody can reach is the one shape Art. 17 cannot honour.
--
-- Stamp ⊆ registry is therefore a database constraint (see
-- `assert_sidecar_stamp_declared` below), not a check in one of the callers.
-- Kernel relations are deliberately absent: they are not sidecars and a
-- stamp must never name one.
CREATE TABLE proxima_core.flavor_surface (
    table_name text PRIMARY KEY,
    flavor_id text NOT NULL,
    CONSTRAINT flavor_surface_qualified_chk
        CHECK (table_name LIKE '%.%' AND table_name = lower(table_name)),
    CONSTRAINT flavor_surface_flavor_id_chk
        CHECK (flavor_id <> '')
);

COMMENT ON TABLE proxima_core.flavor_surface IS
'Declared sidecar surfaces, one row per (table, flavor). Populated by each flavor migration; memory.sidecar_tables is constrained to be a subset. Flavor #0 (core) owns the proxima_core.* rows and is non-removable.';

-- Flavor #0. Kept in step with `proxima_core::FLAVOR_0` by
-- `the_migration_declares_exactly_the_contracts_sidecar_surfaces`.
INSERT INTO proxima_core.flavor_surface (table_name, flavor_id) VALUES
    ('proxima_core.agent_derivation_v1', 'core'),
    ('proxima_core.agent_note_v1', 'core'),
    ('proxima_core.interpretation_v1', 'core'),
    ('proxima_core.mcp_call_logged_v1', 'core'),
    ('proxima_core.task_goal_v1', 'core'),
    ('proxima_core.utterance_v1', 'core'),
    ('proxima_core.write_act_v1', 'core');

CREATE TABLE proxima_core.memory (
    handle uuid NOT NULL REFERENCES proxima_core.memory_head (handle),
    t uuid NOT NULL DEFAULT uuidv7(),
    kind proxima_core.memory_kind NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    schema_id text NOT NULL,
    source_id text,
    ingest_key text,
    blob_id uuid REFERENCES proxima_core.blob (blob_id),
    content_id uuid REFERENCES proxima_core.content (content_id),
    origins uuid[] NOT NULL DEFAULT '{}',
    refs uuid[] NOT NULL DEFAULT '{}',
    sidecar_tables text[] NOT NULL DEFAULT '{}',
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
    CONSTRAINT memory_ap_content_chk CHECK (
        kind = 'fact' OR content_id IS NOT NULL
    ),
    CONSTRAINT memory_origins_no_null_chk CHECK (array_position(origins, NULL) IS NULL),
    CONSTRAINT memory_refs_no_null_chk CHECK (array_position(refs, NULL) IS NULL),
    CONSTRAINT memory_sidecar_tables_no_null_chk CHECK (array_position(sidecar_tables, NULL) IS NULL)
);

CREATE INDEX memory_owner_handle_t_idx
    ON proxima_core.memory (owner_id, handle, t DESC);

CREATE INDEX memory_owner_t_handle_idx
    ON proxima_core.memory (owner_id, t, handle);

CREATE INDEX memory_owner_schema_t_idx
    ON proxima_core.memory (owner_id, schema_id, t DESC);

CREATE INDEX memory_blob_id_idx
    ON proxima_core.memory (blob_id)
    WHERE blob_id IS NOT NULL;

CREATE INDEX memory_content_id_idx
    ON proxima_core.memory (content_id)
    WHERE content_id IS NOT NULL;

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

-- Goals do not transfer: the owner transfer verb is memory-only, and
-- `goal_head_t_only` freezes `goal_head.owner_id` as the DDL backstop.
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

CREATE INDEX goal_owner_state_t_idx
    ON proxima_core.goal (owner_id, state, t DESC);

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
    blob_id uuid REFERENCES proxima_core.blob (blob_id),
    content_id uuid REFERENCES proxima_core.content (content_id),
    source_id text,
    ingest_key text,
    cooled_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX cooled_content_id_idx
    ON proxima_core.cooled (content_id)
    WHERE content_id IS NOT NULL;

CREATE INDEX cooled_owner_id_idx
    ON proxima_core.cooled (owner_id);

CREATE INDEX cooled_owner_source_idx
    ON proxima_core.cooled (owner_id, source_id)
    WHERE source_id IS NOT NULL;

-- Cold objects a committed erase still owes the object store. An erase marks
-- the locator here in the same transaction that deletes the `cooled` row and
-- destroys the object only after that transaction commits: destroying it
-- in-transaction loses the object outright on rollback (the locator returns,
-- the bytes do not), and a crash between commit and destruction leaves a
-- reclaimable mark instead of a `cooled` row naming nothing.
CREATE TABLE proxima_core.cold_purge_pending (
    object_key text PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    compliance_operation_id uuid,
    enqueued_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX cold_purge_pending_owner_idx
    ON proxima_core.cold_purge_pending (owner_id, enqueued_at);

CREATE TABLE proxima_core.group_memberships (
    group_id uuid NOT NULL,
    member_user_id uuid NOT NULL,
    relation proxima_core.membership_relation NOT NULL,
    PRIMARY KEY (group_id, member_user_id, relation)
);

-- Personal authorize_read is WHERE member_user_id = $1 ORDER BY group_id, relation.
-- The PK leads with group_id and cannot serve that lookup.
CREATE INDEX group_memberships_member_user_id_idx
    ON proxima_core.group_memberships (member_user_id, group_id, relation);

CREATE TABLE proxima_core.lexical_languages (
    config regconfig PRIMARY KEY
);

INSERT INTO proxima_core.lexical_languages (config)
VALUES ('english'::regconfig)
ON CONFLICT DO NOTHING;

-- The mutable deployment default is data, not an IMMUTABLE function body.
-- The boolean key admits at most one row and serializes concurrent switches.
CREATE TABLE proxima_core.lexical_default (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    config regconfig NOT NULL REFERENCES proxima_core.lexical_languages (config)
);

INSERT INTO proxima_core.lexical_default (singleton, config)
VALUES (true, 'english'::regconfig);

CREATE FUNCTION proxima_core.lexical_scrub(txt text) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT regexp_replace(
       regexp_replace(txt, '[[:punct:]]+', ' ', 'g'),
       '\m[[:alnum:]]{255}[[:alnum:]]+\M', ' ', 'g') $$;

CREATE FUNCTION proxima_core.lexical_config() RETURNS regconfig
LANGUAGE sql STABLE PARALLEL SAFE AS
$$ SELECT config
     FROM proxima_core.lexical_default
    WHERE singleton $$;

CREATE FUNCTION proxima_core.lexical_tsv(txt text) RETURNS tsvector
LANGUAGE sql STABLE STRICT PARALLEL SAFE AS
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
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS
$$ SELECT query_text $$;

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
   ON CONFLICT DO NOTHING;
   INSERT INTO proxima_core.lexical_default (singleton, config)
   VALUES (true, cfg::regconfig)
   ON CONFLICT (singleton) DO UPDATE
   SET config = EXCLUDED.config $$;

-- Fired BEFORE INSERT, not AFTER: the stamped columns carry a foreign key
-- into lexical_languages, and the RI check queues with the other AFTER-row
-- triggers — self-registration must already be visible when it runs.
CREATE FUNCTION proxima_core.remember_lexical_language() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO proxima_core.lexical_languages (config)
    VALUES (NEW.lexical_language)
    ON CONFLICT DO NOTHING;
    RETURN NEW;
END;
$$;

-- ---------------------------------------------------------------------------
-- Guarded removal from the active-language set. PostgreSQL does not block
-- DROP TEXT SEARCH CONFIGURATION while table rows hold its regconfig value
-- (no pg_depend entry is recorded for stored values — verified on PG 18.4):
-- the rows are left with a dangling OID that renders as a number and makes
-- any later UPDATE of the row fail with `cache lookup failed`. So the rule
-- is: forget a language here FIRST — this refuses while any row still
-- references it — and only then, if it was a custom configuration, drop it.
--
-- The still-referenced guarantee is the FK machinery, not a scan: every
-- stamped `lexical_language` column REFERENCES lexical_languages (config),
-- so a concurrent writer's RI check holds KEY SHARE on the registration row
-- for its transaction and this DELETE blocks or refuses — there is no
-- check-then-delete window. Regconfig columns outside those FKs (operator
-- DDL, foreign tables) are the operator's own responsibility.
-- ---------------------------------------------------------------------------
CREATE FUNCTION proxima_core.lexical_language_forget(config_to_forget regconfig)
RETURNS void
LANGUAGE plpgsql AS
$$
DECLARE
    holder_table text;
    fk_detail text;
BEGIN
    IF config_to_forget IS NULL THEN
        RAISE EXCEPTION 'lexical configuration must not be null';
    END IF;
    IF config_to_forget = proxima_core.lexical_config() THEN
        RAISE EXCEPTION 'cannot forget %: it is the default lexical configuration',
            config_to_forget;
    END IF;

    BEGIN
        DELETE FROM proxima_core.lexical_languages
         WHERE config = config_to_forget;
        -- Zero rows deleted = not registered: nothing to forget.
    EXCEPTION
        WHEN foreign_key_violation THEN
            GET STACKED DIAGNOSTICS
                holder_table = TABLE_NAME,
                fk_detail = PG_EXCEPTION_DETAIL;
            RAISE EXCEPTION 'cannot forget %: rows in % still reference it (%)',
                config_to_forget, holder_table, fk_detail
                USING ERRCODE = '23503';
    END;
END
$$;

COMMENT ON FUNCTION proxima_core.lexical_language_forget(regconfig) IS
'Remove a configuration from the active-language set, refusing while any row still holds it in an FK-stamped lexical_language column. The FK checks serialize this against in-flight writes. Run this BEFORE dropping a custom text search configuration: PostgreSQL allows the drop with rows still referencing it, and those rows are then un-updatable (cache lookup failed on the dangling OID).';

-- The one owner-pinned Memory sidecar. `owner_id` is the owner that MADE
-- the call, stamped at write time from the Memory's owner and never
-- rewritten: this table answers "what did my agents do", which stays true
-- of the acting owner after the Memory it describes is transferred away.
-- Every other sidecar reaches its owner through the Memory and follows it.
-- `t` names the Memory the call was recorded as, and carries NO foreign key
-- to it — the same FK-free `t` `sketch` below has, for a different reason.
-- The row outlives that Memory on purpose: once the Memory is transferred
-- away, its new owner may forget or erase it, and neither of those is
-- allowed to destroy the acting owner's audit trail — nor to fail on a
-- child row the erasing owner cannot see. The row dies with its own owner.
CREATE TABLE proxima_core.mcp_call_logged_v1 (
    t uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
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

COMMENT ON COLUMN proxima_core.mcp_call_logged_v1.owner_id IS
'The owner that made the call, pinned at write time. Deliberately NOT derived from proxima_core.memory.owner_id on read: an owner transfer moves the Memory and leaves this row behind, so history, export, and Art. 17 erase all stay with the acting owner and the destination never sees the prior owner''s actor identities.';

-- read_mcp_call_history pages by (time, t) for one owner, optionally
-- filtered by actor. The Memory-side index cannot serve it any more: the
-- scope is this table's own owner_id.
CREATE INDEX mcp_call_logged_v1_owner_t_idx
    ON proxima_core.mcp_call_logged_v1 (owner_id, t DESC);

-- Hot one-liners for recall/think. Plumbing, not a kernel sort.
-- `t` is Memory.t or Goal.t; no FK (two home tables). Forget deletes the row.
CREATE TYPE proxima_core.sketch_kind AS ENUM (
    'fact',
    'abstraction',
    'perspective',
    'goal'
);

-- The sketch carries no vector. It never had a reader: `core_search_memories`
-- scans the four declared sidecars and nothing scanned `sketch`, so its
-- `search_tsv`, its GIN and the `lexical_language` that fed them were index
-- maintenance on every recall write for no query. Lexical search is the
-- projection's job now.
CREATE TABLE proxima_core.sketch (
    t uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    kind proxima_core.sketch_kind NOT NULL,
    text text NOT NULL,
    CONSTRAINT sketch_text_nonblank_chk CHECK (length(btrim(text)) > 0)
);

CREATE INDEX sketch_owner_t_idx
    ON proxima_core.sketch (owner_id, t DESC);

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

CREATE INDEX embeddings_owner_model_idx
    ON proxima_core.embeddings (owner_id, model_id);

CREATE TABLE proxima_core.embedding_heads (
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    embedding_version int NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    PRIMARY KEY (entity_id, model_id)
);

-- `failed` is retryable-terminal (reconcile requeues it); `failed_permanent`
-- is an input the provider will always reject, so nothing requeues it.
CREATE TYPE proxima_core.embedding_job_status AS ENUM (
    'pending',
    'processing',
    'failed',
    'failed_permanent'
);

CREATE TABLE proxima_core.embedding_jobs (
    job_id uuid PRIMARY KEY DEFAULT uuidv7(),
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    status proxima_core.embedding_job_status NOT NULL DEFAULT 'pending',
    claimed_at timestamptz,
    claim_token uuid,
    last_error text,
    UNIQUE (owner_id, entity_id, model_id),
    CONSTRAINT embedding_job_processing_claim_chk CHECK (
        (status = 'processing') = (claimed_at IS NOT NULL AND claim_token IS NOT NULL)
    )
);

CREATE INDEX embedding_jobs_pending_claim_idx
    ON proxima_core.embedding_jobs (model_id, job_id)
    WHERE status = 'pending';

CREATE TABLE proxima_core.write_act_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    episode_id uuid NOT NULL
);

CREATE TABLE proxima_core.agent_note_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    note_id uuid NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    tags text[] NOT NULL DEFAULT '{}',
    idempotency_key text,
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

CREATE TABLE proxima_core.utterance_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    speaker text NOT NULL,
    conversation_id text NOT NULL,
    text text NOT NULL,
    embed_text text GENERATED ALWAYS AS (NULLIF(text, '')) STORED
);

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

CREATE TABLE proxima_core.interpretation_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    claim text NOT NULL,
    confidence smallint NOT NULL,
    subject_memory_ids uuid[] NOT NULL DEFAULT '{}',
    subject_kinds proxima_core.interpretation_subject_kind[] NOT NULL DEFAULT '{}',
    model_id text NOT NULL,
    client_name text NOT NULL,
    client_version text NOT NULL,
    embed_text text GENERATED ALWAYS AS (NULLIF(claim, '')) STORED
);

-- ---------------------------------------------------------------------------
-- The lexical projection. GENERATED — see `crates/storage-pg/src/projection.rs`
-- and the `generator_output_is_the_migration_text` pin. Edit the generator,
-- not this block.
--
-- One table per flavor, in the flavor's own schema, holding one row per
-- (memory, projected schema). It replaces five per-sidecar `search_tsv`
-- generated columns and their five GIN indexes with one index the whole
-- flavor shares, and it is where a memory's `lexical_language` is stamped
-- now that the sidecars no longer carry one.
--
-- Deliberately NOT registered in `proxima_core.flavor_surface`: a
-- projection row is derived from a sidecar row, never stamped by a memory,
-- so `memory.sidecar_tables` must never name it.
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

CREATE INDEX core_projection_owner_tsv_gin ON proxima_core.projection USING gin (owner_id, search_tsv);

CREATE TRIGGER projection_remember_lang
    BEFORE INSERT ON proxima_core.projection
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
    aborted_at timestamptz,
    -- The upload row whose id minted the object this row names, when that
    -- is not this row itself. NULL means "I minted my own object", which
    -- is every row an upload creates; a cross-owner transfer sets it, so
    -- the destination gets a row of its own over the same bytes instead of
    -- a copy of the bytes (OCI's cross-repo blob mount, same shape).
    --
    -- Always the MINTING id, never the immediate source: mounting B from
    -- A and then C from B must leave C naming A's object, because B never
    -- had one of its own. The transfer writes
    -- COALESCE(source.mounted_from_upload_id, source.upload_id).
    --
    -- DELIBERATELY NOT A FOREIGN KEY, against the projection map's §3.5
    -- prescription. A reference to blob_uploads (upload_id) makes one
    -- owner's mount a veto over another owner's erase: NO ACTION aborts
    -- the source's Art. 17 deletion, SET NULL silently breaks the
    -- destination's read (the row would then claim a key it did not mint
    -- and the gate would reject it), and CASCADE deletes the
    -- destination's row outright. Erase must stay owner-scoped, so the
    -- column is a derivation input rather than a relationship. It cannot
    -- dangle onto someone else's object either: upload_id is uuidv7 and
    -- is never reused, so a pointer to a deleted row resolves to a key
    -- nothing else will ever mint.
    mounted_from_upload_id uuid,
    CONSTRAINT blob_uploads_mount_not_self_chk
        CHECK (mounted_from_upload_id IS DISTINCT FROM upload_id)
);

CREATE INDEX blob_uploads_owner_status_idx
    ON proxima_core.blob_uploads (owner_id, status);

-- Refcount-by-query for the object, the way gc_unreferenced_content is
-- refcount-by-query for the row. Two owners may now name one object, so
-- "delete the object when its row goes" became "delete the object when no
-- surviving row names it" -- an anti-join that wants this index and no
-- stored counter.
CREATE INDEX blob_uploads_object_key_idx
    ON proxima_core.blob_uploads (object_key);

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
    'SourceScopeOwnerStillLive',
    'PersonalDropNotVerified',
    'DropProofPortUnavailable',
    'LegalHoldActive'
);

CREATE TABLE proxima_core.owner_legal_holds (
    owner_kind proxima_core.owner_kind NOT NULL,
    owner_id uuid NOT NULL,
    hold_active boolean NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner_kind, owner_id)
);

-- Per-owner Fact-retention window, read by `proxima://graph` and enforced by
-- the `maintain-retention` sweep. `(owner_kind, owner_id)` is the ON CONFLICT
-- arbiter `upsert_fact_retention` names; every owner kind carries an id, so
-- no NULL arbiter arm is needed.
CREATE TABLE proxima_core.owner_fact_retention (
    owner_kind proxima_core.owner_kind NOT NULL,
    owner_id uuid NOT NULL,
    retention_seconds bigint NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT owner_fact_retention_retention_seconds_check
        CHECK (retention_seconds > 0),
    UNIQUE (owner_kind, owner_id)
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
    wake_configs_count bigint NOT NULL DEFAULT 0,
    blobs_count bigint NOT NULL DEFAULT 0,
    blob_uploads_count bigint NOT NULL DEFAULT 0,
    sidecar_rows_count bigint NOT NULL DEFAULT 0,
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
    cold_object_purge_pending boolean NOT NULL DEFAULT false,
    cited_object_purge_pending boolean NOT NULL DEFAULT false
);

ALTER TABLE proxima_core.cold_purge_pending
    ADD CONSTRAINT cold_purge_pending_compliance_operation_fk
    FOREIGN KEY (compliance_operation_id)
    REFERENCES proxima_core.compliance_audit_log (operation_id);

CREATE TABLE proxima_core.delegated_authority_grants (
    delegation_id uuid PRIMARY KEY,
    subject_user_id uuid NOT NULL,
    owner_kind proxima_core.owner_kind NOT NULL,
    owner_id uuid NOT NULL,
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

-- Stamp ⊆ registry, enforced at write time.
--
-- An array cannot carry a foreign key, so this is the array-FK shim the
-- constraint would be if PostgreSQL had element references. It is a trigger
-- rather than a CHECK because the predicate reads another table. Same
-- discipline as `lexical_language_forget`: no list of tables lives here —
-- the anti-join asks `flavor_surface` and reports whichever element failed.
CREATE FUNCTION proxima_core.assert_sidecar_stamp_declared() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    undeclared text;
BEGIN
    SELECT stamped INTO undeclared
      FROM unnest(NEW.sidecar_tables) AS stamped
     WHERE NOT EXISTS (
               SELECT 1
                 FROM proxima_core.flavor_surface fs
                WHERE fs.table_name = stamped
           )
     LIMIT 1;
    IF undeclared IS NOT NULL THEN
        RAISE EXCEPTION
            'memory.sidecar_tables names %, which no flavor declares in proxima_core.flavor_surface',
            undeclared
            USING ERRCODE = '23503';
    END IF;
    RETURN NEW;
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

-- Content is append-only. `owner_id` may move: an owner-to-owner transfer
-- is a series transfer (MemoryHeadAligned), not a new (handle, t).
--
-- `blob_id` may move too, and ONLY onto identical bytes. The Lean model
-- (Causa/Citations.lean) requires `memory_cites m b -> memory_owner m =
-- blob_owner b`: a memory and the blob row it cites name the same owner.
-- A transfer moves `owner_id`, so something has to give -- the pre-dedupe
-- code kept the invariant by moving the blob row along, or by refusing
-- when it could not, and the dedupe arm keeps it by repointing the
-- citation at the destination's own row over the same object.
--
-- The `content_hash` and `schema_id` equality check is what stops that
-- from being a hole. `content_id` has been freely mutable here for the
-- same remap reason with no such check, which means nothing but the
-- calling code stops it repointing at unrelated bytes. This is that same
-- move, done properly: the database, not the caller's memory, is what
-- guarantees a repointed citation still cites what it cited.
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
       OR NEW.origins IS DISTINCT FROM OLD.origins
       OR NEW.refs IS DISTINCT FROM OLD.refs
       OR NEW.sidecar_tables IS DISTINCT FROM OLD.sidecar_tables THEN
        RAISE EXCEPTION 'append-only: % does not accept UPDATE', TG_TABLE_NAME
            USING ERRCODE = '25006';
    END IF;
    IF NEW.blob_id IS DISTINCT FROM OLD.blob_id
       AND NOT EXISTS (
               SELECT 1
                 FROM proxima_core.blob old_blob
                 JOIN proxima_core.blob new_blob
                   ON new_blob.schema_id = old_blob.schema_id
                  AND new_blob.content_hash = old_blob.content_hash
                WHERE old_blob.blob_id = OLD.blob_id
                  AND new_blob.blob_id = NEW.blob_id
           ) THEN
        RAISE EXCEPTION
            'append-only: %.blob_id may only be repointed at a blob row naming the same '
            'schema_id and content_hash', TG_TABLE_NAME
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;

-- B2 — a pin is grounding support iff it is a hot row, or a cooled Fact.
-- `cooling` is the t about to leave the hot set (forget); NULL at admit.
CREATE FUNCTION proxima_core.pins_have_grounding_support(
    pins uuid[],
    cooling uuid,
    cooling_kind proxima_core.memory_kind
) RETURNS boolean
    LANGUAGE sql
    VOLATILE
    AS $$
    SELECT EXISTS (
        SELECT 1
          FROM unnest(pins) AS p(id)
         WHERE CASE
                 WHEN cooling IS NOT NULL AND p.id = cooling THEN
                   cooling_kind = 'fact'
                 ELSE
                   EXISTS (SELECT 1 FROM proxima_core.memory h WHERE h.t = p.id)
                   OR EXISTS (
                        SELECT 1 FROM proxima_core.cooled c
                         WHERE c.t = p.id AND c.kind = 'fact'
                   )
               END
    );
$$;

CREATE FUNCTION proxima_core.memory_pin_checks() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    pin uuid;
    pin_handle uuid;
BEGIN
    IF NEW.kind = 'fact' AND NEW.origins = '{}' AND NEW.refs = '{}' THEN
        RETURN NEW;
    END IF;

    IF NEW.origins <> '{}' OR NEW.refs <> '{}' THEN
        -- Wait out an in-flight forget *before* B2 (FOR UPDATE in commit_forget).
        PERFORM 1
          FROM proxima_core.memory
         WHERE t = ANY (NEW.origins || NEW.refs)
         FOR SHARE;
    END IF;

    IF NEW.kind <> 'fact'
       AND NOT proxima_core.pins_have_grounding_support(
             NEW.origins || NEW.refs, NULL, NULL
           )
    THEN
        RAISE EXCEPTION 'non-fact must pin a hot memory or a cooled fact'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.origins = '{}' AND NEW.refs = '{}' THEN
        RETURN NEW;
    END IF;

    SELECT p.pin INTO pin
      FROM unnest(NEW.origins || NEW.refs) AS p(pin)
      LEFT JOIN proxima_core.memory m ON m.t = p.pin
      LEFT JOIN proxima_core.cooled c ON c.t = p.pin
     WHERE m.t IS NULL AND c.t IS NULL
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'pin % does not exist', pin USING ERRCODE = '23503';
    END IF;

    SELECT m.handle INTO pin_handle
      FROM proxima_core.memory m
      JOIN proxima_core.closed_handle c ON c.handle = m.handle
     WHERE m.t = ANY (NEW.origins || NEW.refs)
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'closed_handle: no new pin to %', pin_handle USING ERRCODE = '23514';
    END IF;

    IF NEW.kind = 'abstraction' AND NEW.origins <> '{}' THEN
        IF EXISTS (
            SELECT 1
              FROM unnest(NEW.origins) AS o(id)
             WHERE NOT EXISTS (
                       SELECT 1 FROM proxima_core.memory m
                        WHERE m.t = o.id AND m.kind IN ('fact', 'abstraction')
                   )
               AND NOT EXISTS (
                       SELECT 1 FROM proxima_core.cooled c
                        WHERE c.t = o.id AND c.kind IN ('fact', 'abstraction')
                   )
        ) THEN
            RAISE EXCEPTION 'abstraction origins must be fact or abstraction t'
                USING ERRCODE = '23514';
        END IF;
    ELSIF NEW.kind = 'perspective' AND NEW.origins <> '{}' THEN
        IF EXISTS (
            SELECT 1
              FROM unnest(NEW.origins) AS o(id)
             WHERE NOT EXISTS (
                       SELECT 1 FROM proxima_core.memory m
                        WHERE m.t = o.id AND m.kind = 'abstraction'
                   )
               AND NOT EXISTS (
                       SELECT 1 FROM proxima_core.cooled c
                        WHERE c.t = o.id AND c.kind = 'abstraction'
                   )
        ) THEN
            RAISE EXCEPTION 'perspective origins must be abstraction t'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION proxima_core.cooled_forget_grounding() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.kind = 'fact' THEN
        RETURN NEW;
    END IF;
    -- Lock dependers (any owner) so two forgets cannot each treat the
    -- other target as still-hot support. ORDER BY t for a stable wait graph.
    PERFORM 1
      FROM proxima_core.memory m
     WHERE m.kind <> 'fact'
       AND m.t <> NEW.t
       AND (m.origins @> ARRAY[NEW.t] OR m.refs @> ARRAY[NEW.t])
     ORDER BY m.t
     FOR UPDATE;
    IF EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.kind <> 'fact'
           AND m.t <> NEW.t
           AND (m.origins @> ARRAY[NEW.t] OR m.refs @> ARRAY[NEW.t])
           AND NOT proxima_core.pins_have_grounding_support(
                 m.origins || m.refs, NEW.t, NEW.kind
               )
    ) THEN
        RAISE EXCEPTION 'forget would leave an ungrounded memory'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

-- Goals never change owner (the transfer verb is memory-only), so only the
-- head pointer `t` may move.
CREATE FUNCTION proxima_core.goal_head_t_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
       OR NEW.owner_id IS DISTINCT FROM OLD.owner_id THEN
        RAISE EXCEPTION 'goal_head is frozen except t'
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

-- The four searchable core sidecars became append-only WITH the projection.
-- Their text used to be indexed in place by a GENERATED column, so an
-- UPDATE re-derived the vector for free. The projection row is written
-- once, by the same transaction as the sidecar row; an UPDATE of the text
-- would leave the vector describing the old text with nothing to notice.
-- Supersession is a later `t` on the same handle, never an UPDATE, so this
-- forbids nothing the write path does — it forbids the drift.
CREATE TRIGGER agent_note_v1_append_only
    BEFORE UPDATE ON proxima_core.agent_note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

CREATE TRIGGER utterance_v1_append_only
    BEFORE UPDATE ON proxima_core.utterance_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

CREATE TRIGGER agent_derivation_v1_append_only
    BEFORE UPDATE ON proxima_core.agent_derivation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

CREATE TRIGGER interpretation_v1_append_only
    BEFORE UPDATE ON proxima_core.interpretation_v1
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

CREATE TRIGGER memory_sidecar_stamp_declared
    BEFORE INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    FOR EACH ROW
    WHEN (NEW.sidecar_tables <> '{}')
    EXECUTE FUNCTION proxima_core.assert_sidecar_stamp_declared();

CREATE TRIGGER memory_pin_checks
    BEFORE INSERT ON proxima_core.memory
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.memory_pin_checks();

CREATE TRIGGER cooled_forget_grounding
    BEFORE INSERT ON proxima_core.cooled
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.cooled_forget_grounding();

CREATE TRIGGER goal_append_only
    BEFORE UPDATE ON proxima_core.goal
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

CREATE TRIGGER goal_head_t_only
    BEFORE UPDATE ON proxima_core.goal_head
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.goal_head_t_only();

CREATE TRIGGER goal_no_later_after_terminal
    BEFORE INSERT ON proxima_core.goal
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.goal_no_later_after_terminal();
