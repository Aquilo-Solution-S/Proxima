-- Proxima code-flavor schema — destructive v0.0.8 baseline (the edge lane).
--
-- THIS MIGRATION DROPS proxima_code AND REBUILDS IT. Re-register the
-- repositories and re-index; that is the supported path back, and the flavor
-- already ships the runbook for it (`proxima-code_erase_repo`, then register
-- and ingest). Nothing is carried over, deliberately.
--
-- The reason is structural rather than cosmetic. The pre-v0.0.8 baseline
-- (20260516000020) created `proxima_code.code_calls_v1` with a foreign key to
-- a now-deleted edge id. Pins live on `memory.origins` / `memory.refs`;
-- there is no edge table. The old baseline can therefore no longer run at
-- all — not on a fresh database and not on an old one — so the flavor lane is
-- replaced rather than extended.
--
-- The four superseded lanes (20260516000020, 20260709000020, 20260726000020,
-- 20260728000020, 20260729000020) are folded in here and deleted from the
-- tree. `migrator()` sets `ignore_missing`, so a database that already ran
-- them tolerates their absence and applies this one.
--
-- What changed with the edge model (docs/16-edges.md §Flavor Migration):
--
--   * `code_calls_v1` is GONE. It was an edge sidecar holding one call site
--     per edge, which is why a second call to the same callee needed a second
--     edge and a synthetic id to keep them apart. Call sites now live in the
--     caller chunk's own payload (`code_chunk_call_v1`), and the connection is
--     one `memory.refs` pin derived from it. Ten call
--     sites, one index row.
--   * `work_assignment_v1` is NEW: the Perspective that replaced the
--     `proxima-code/targets-execution-request` relation. Neither endpoint
--     could own the claim, so the model was missing a node.
--   * `work_requested_v1` and `test_requested_v1` gain
--     `depends_on_memory_ids` — `core/depends-on` moved onto the depending
--     row, which is the node that owns the statement.
--   * `execution_plan_item_v1` gains `request_memory_id`: the plan is written
--     after its items and names each one, instead of pointing at them with
--     edges appended afterwards.

DROP SCHEMA IF EXISTS proxima_code CASCADE;

CREATE EXTENSION IF NOT EXISTS pg_trgm;

--
-- Name: proxima_code; Type: SCHEMA; Schema: -; Owner: -
--

CREATE SCHEMA proxima_code;


--
-- Name: acceptance_verification_status; Type: TYPE; Schema: proxima_code; Owner: -
--

CREATE TYPE proxima_code.acceptance_verification_status AS ENUM (
    'passed',
    'failed',
    'skipped',
    'blocked'
);


--
-- Name: acceptance_verifier_kind; Type: TYPE; Schema: proxima_code; Owner: -
--

CREATE TYPE proxima_code.acceptance_verifier_kind AS ENUM (
    'file_exists',
    'command',
    'browser_smoke',
    'diff_scope',
    'reviewer_only'
);


--
-- Name: execution_plan_item_kind; Type: TYPE; Schema: proxima_code; Owner: -
--

CREATE TYPE proxima_code.execution_plan_item_kind AS ENUM (
    'work',
    'test'
);


--
-- Name: file_state; Type: TYPE; Schema: proxima_code; Owner: -
--

CREATE TYPE proxima_code.file_state AS ENUM (
    'Present',
    'Tombstone'
);


--
-- Name: repo_ingestion_run_stage; Type: TYPE; Schema: proxima_code; Owner: -
--

CREATE TYPE proxima_code.repo_ingestion_run_stage AS ENUM (
    'starting',
    'facts',
    'ast_edges',
    'f2a',
    'embeddings',
    'done'
);


--
-- Name: repo_ingestion_run_status; Type: TYPE; Schema: proxima_code; Owner: -
--

CREATE TYPE proxima_code.repo_ingestion_run_status AS ENUM (
    'queued',
    'running',
    'succeeded',
    'failed'
);


--
-- Name: work_result_status; Type: TYPE; Schema: proxima_code; Owner: -
--

CREATE TYPE proxima_code.work_result_status AS ENUM (
    'succeeded',
    'failed',
    'blocked',
    'cancelled'
);


--
-- Name: text_array_search(text[]); Type: FUNCTION; Schema: proxima_code; Owner: -
--

CREATE FUNCTION proxima_code.text_array_search(items text[]) RETURNS text
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$
    SELECT array_to_string(items, ' ')
$$;


SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: acceptance_criteria_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.acceptance_criteria_v1 (
    t uuid NOT NULL,
    work_item_memory_id uuid NOT NULL,
    criteria_count integer NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT acceptance_criteria_v1_count_chk CHECK ((criteria_count > 0))
);


--
-- Name: acceptance_criterion_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.acceptance_criterion_v1 (
    criteria_memory_id uuid NOT NULL,
    criterion_index integer NOT NULL,
    criterion_key text NOT NULL,
    description text NOT NULL,
    required boolean NOT NULL,
    verifier_kind proxima_code.acceptance_verifier_kind NOT NULL,
    verifier_path text,
    verifier_command text[],
    verifier_pattern text,
    verifier_note text,
    CONSTRAINT acceptance_criterion_v1_command_chk CHECK (((verifier_command IS NULL) OR (array_position(verifier_command, NULL::text) IS NULL))),
    CONSTRAINT acceptance_criterion_v1_description_chk CHECK (((char_length(description) >= 1) AND (char_length(description) <= 4000))),
    CONSTRAINT acceptance_criterion_v1_index_chk CHECK ((criterion_index >= 0)),
    CONSTRAINT acceptance_criterion_v1_key_chk CHECK (((char_length(criterion_key) >= 1) AND (char_length(criterion_key) <= 80)))
);


--
-- Name: acceptance_summary_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.acceptance_summary_v1 (
    t uuid NOT NULL,
    work_item_memory_id uuid NOT NULL,
    repo_id uuid NOT NULL,
    passed_required boolean NOT NULL,
    summary text NOT NULL,
    verification_memory_ids uuid[] NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT acceptance_summary_v1_summary_chk CHECK (((char_length(summary) >= 1) AND (char_length(summary) <= 4000)))
);


--
-- Name: acceptance_verification_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.acceptance_verification_v1 (
    t uuid NOT NULL,
    work_item_memory_id uuid NOT NULL,
    criterion_key text NOT NULL,
    status proxima_code.acceptance_verification_status NOT NULL,
    summary text NOT NULL,
    artifact_refs text[] NOT NULL,
    verifier_memory_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT acceptance_verification_v1_artifacts_chk CHECK ((array_position(artifact_refs, NULL::text) IS NULL)),
    CONSTRAINT acceptance_verification_v1_criterion_key_chk CHECK (((char_length(criterion_key) >= 1) AND (char_length(criterion_key) <= 80))),
    CONSTRAINT acceptance_verification_v1_summary_chk CHECK (((char_length(summary) >= 1) AND (char_length(summary) <= 4000)))
);


--
-- Name: code_chunk_call_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.code_chunk_call_v1 (
    caller_memory_id uuid NOT NULL,
    callee_memory_id uuid NOT NULL,
    site_index integer NOT NULL,
    byte_start bigint NOT NULL,
    byte_end bigint NOT NULL,
    callee_name text NOT NULL,
    is_dynamic boolean NOT NULL,
    CONSTRAINT code_chunk_call_v1_byte_range_chk CHECK ((byte_end >= byte_start)),
    CONSTRAINT code_chunk_call_v1_site_index_chk CHECK ((site_index >= 0)),
    CONSTRAINT code_chunk_call_v1_no_self_call_chk CHECK ((caller_memory_id <> callee_memory_id))
);


--
-- Name: TABLE code_chunk_call_v1; Type: COMMENT; Schema: proxima_code; Owner: -
--

COMMENT ON TABLE proxima_code.code_chunk_call_v1 IS 'Call sites of proxima_code.code_chunk_v1, one row per site. The caller chunk''s payload owns them: ten calls into the same callee are ten rows here and exactly one pin in memory.refs. Successor to the code_calls_v1 edge sidecar, which stored one site per edge and could therefore never hold the second one. There is deliberately no foreign key on callee_memory_id: chunks of one file call each other in both directions, so the payload rows are written before any of the group''s index rows, and the index is what enforces that the callee exists.';


--
-- Name: work_assignment_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.work_assignment_v1 (
    t uuid NOT NULL,
    repo_id uuid NOT NULL,
    target_perspective_memory_id uuid NOT NULL,
    work_item_memory_id uuid NOT NULL,
    reason text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT work_assignment_v1_reason_chk
        CHECK (((char_length(reason) >= 1) AND (char_length(reason) <= 4000)))
);


--
-- Name: TABLE work_assignment_v1; Type: COMMENT; Schema: proxima_code; Owner: -
--

COMMENT ON TABLE proxima_code.work_assignment_v1 IS 'Perspective payload for proxima-code/work-assignment-v1: "this worker should pick up that request". Successor to the targets-execution-request relation — neither endpoint could own the claim, so it became a node whose two reference fields are the connections.';


--
-- Name: code_chunk_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.code_chunk_v1 (
    t uuid NOT NULL,
    repo_id uuid NOT NULL,
    file_path text NOT NULL,
    chunk_index integer NOT NULL,
    text text NOT NULL,
    language text,
    chunk_type text NOT NULL,
    byte_range_start bigint NOT NULL,
    byte_range_end bigint NOT NULL,
    line_range_start bigint NOT NULL,
    line_range_end bigint NOT NULL,
    state proxima_code.file_state NOT NULL,
    search_tsv tsvector GENERATED ALWAYS AS (proxima_core.lexical_tsv(lexical_language, proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(file_path, ''::text), NULLIF(text, ''::text)]))) STORED,
    lexical_language regconfig DEFAULT 'english'::regconfig NOT NULL,
    CONSTRAINT code_chunk_v1_chunk_index_chk CHECK ((chunk_index >= 0))
);


--
-- Name: COLUMN code_chunk_v1.search_tsv; Type: COMMENT; Schema: proxima_code; Owner: -
--

COMMENT ON COLUMN proxima_code.code_chunk_v1.search_tsv IS 'Lexical vector over file_path + text via the two-argument proxima_core.lexical_tsv under the row''s lexical_language (pinned english), so CodeChunkV1::search_projection() can name this column as its tsv_column. Must stay identical to lexical_tsv(lexical_language, lexical_join(<projected fields>)).';


--
-- Name: COLUMN code_chunk_v1.lexical_language; Type: COMMENT; Schema: proxima_code; Owner: -
--

COMMENT ON COLUMN proxima_code.code_chunk_v1.lexical_language IS 'Text-search configuration for this chunk''s stored vector. Pinned english per row: code search must not follow proxima_core.set_lexical_config, which serves the deployment''s prose.';


--
-- Name: commit_summarizer_self_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.commit_summarizer_self_v1 (
    t uuid NOT NULL,
    display_name text NOT NULL,
    purpose text NOT NULL
);


--
-- Name: commit_summary_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.commit_summary_v1 (
    t uuid NOT NULL,
    repo_id uuid NOT NULL,
    commit_sha text NOT NULL,
    summary text NOT NULL,
    key_files text[] NOT NULL,
    change_kind text NOT NULL
);


--
-- Name: commit_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.commit_v1 (
    t uuid NOT NULL,
    repo_id uuid NOT NULL,
    sha text NOT NULL,
    parents text[] NOT NULL,
    author_name text NOT NULL,
    author_email text NOT NULL,
    author_time timestamp with time zone NOT NULL,
    committer_name text NOT NULL,
    committer_email text NOT NULL,
    committer_time timestamp with time zone NOT NULL,
    message text NOT NULL
);


--
-- Name: development_perspective_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.development_perspective_v1 (
    t uuid NOT NULL,
    repo_id uuid,
    summary text NOT NULL,
    pattern text NOT NULL,
    risk text NOT NULL,
    recommended_posture text NOT NULL,
    confidence real NOT NULL,
    CONSTRAINT development_perspective_v1_confidence_check CHECK (((confidence >= (0)::double precision) AND (confidence <= (1)::double precision)))
);


--
-- Name: engineer_self_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.engineer_self_v1 (
    t uuid NOT NULL,
    display_name text NOT NULL,
    purpose text NOT NULL
);


--
-- Name: execution_plan_item_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.execution_plan_item_v1 (
    plan_memory_id uuid NOT NULL,
    item_index integer NOT NULL,
    item_key text NOT NULL,
    kind proxima_code.execution_plan_item_kind NOT NULL,
    title text NOT NULL,
    depends_on text[] NOT NULL,
    request_key text NOT NULL,
    request_memory_id uuid NOT NULL,
    CONSTRAINT execution_plan_item_v1_depends_chk CHECK ((array_position(depends_on, NULL::text) IS NULL)),
    CONSTRAINT execution_plan_item_v1_index_chk CHECK ((item_index >= 0)),
    CONSTRAINT execution_plan_item_v1_key_chk CHECK (((char_length(item_key) >= 1) AND (char_length(item_key) <= 120))),
    CONSTRAINT execution_plan_item_v1_request_key_chk CHECK (((char_length(request_key) >= 1) AND (char_length(request_key) <= 240))),
    CONSTRAINT execution_plan_item_v1_title_chk CHECK (((char_length(title) >= 1) AND (char_length(title) <= 240)))
);


--
-- Name: execution_plan_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.execution_plan_v1 (
    t uuid NOT NULL,
    repo_id uuid NOT NULL,
    plan_key text NOT NULL,
    goal_activated_memory_id uuid NOT NULL,
    summary text NOT NULL,
    item_count integer NOT NULL,
    evidence_memory_ids uuid[] NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT execution_plan_v1_item_count_chk CHECK ((item_count > 0)),
    CONSTRAINT execution_plan_v1_plan_key_chk CHECK (((char_length(plan_key) >= 1) AND (char_length(plan_key) <= 240))),
    CONSTRAINT execution_plan_v1_summary_chk CHECK (((char_length(summary) >= 1) AND (char_length(summary) <= 4000)))
);


--
-- Name: execution_result_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.execution_result_v1 (
    t uuid NOT NULL,
    work_requested_memory_id uuid NOT NULL,
    repo_id uuid NOT NULL,
    status proxima_code.work_result_status NOT NULL,
    summary text NOT NULL,
    artifact_refs text[] NOT NULL,
    log_excerpt text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT execution_result_v1_artifacts_chk CHECK ((array_position(artifact_refs, NULL::text) IS NULL)),
    CONSTRAINT execution_result_v1_summary_chk CHECK (((char_length(summary) >= 1) AND (char_length(summary) <= 4000)))
);


--
-- Name: file_revision_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.file_revision_v1 (
    t uuid NOT NULL,
    repo_id uuid NOT NULL,
    file_path text NOT NULL,
    language text,
    content_sha256 bytea NOT NULL,
    size_bytes bigint NOT NULL,
    indexed_commit_sha text NOT NULL,
    state proxima_code.file_state NOT NULL
);


--
-- Name: repo_ingestion_runs; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.repo_ingestion_runs (
    run_id uuid NOT NULL,
    owner_kind proxima_core.owner_kind NOT NULL,
    owner_id uuid,
    repo_id uuid NOT NULL,
    status proxima_code.repo_ingestion_run_status NOT NULL,
    stage proxima_code.repo_ingestion_run_stage NOT NULL,
    commits_emitted integer DEFAULT 0 NOT NULL,
    files_emitted integer DEFAULT 0 NOT NULL,
    chunks_emitted integer DEFAULT 0 NOT NULL,
    chunks_reused integer DEFAULT 0 NOT NULL,
    chunks_tombstoned integer DEFAULT 0 NOT NULL,
    ast_edges_emitted integer DEFAULT 0 NOT NULL,
    abstractions_emitted integer DEFAULT 0 NOT NULL,
    embeddings_landed integer DEFAULT 0 NOT NULL,
    citations_emitted integer DEFAULT 0 NOT NULL,
    error_message text,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    finished_at timestamp with time zone,
    CONSTRAINT repo_ingestion_runs_owner_ref_shape_chk CHECK ((((owner_kind = 'world'::proxima_core.owner_kind) AND (owner_id IS NULL)) OR ((owner_kind = ANY (ARRAY['personal'::proxima_core.owner_kind, 'group'::proxima_core.owner_kind])) AND (owner_id IS NOT NULL)))),
    CONSTRAINT repo_ingestion_runs_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_kind)),
    CONSTRAINT runs_finished_when_terminal_chk CHECK ((((status = ANY (ARRAY['succeeded'::proxima_code.repo_ingestion_run_status, 'failed'::proxima_code.repo_ingestion_run_status])) AND (finished_at IS NOT NULL)) OR ((status = ANY (ARRAY['queued'::proxima_code.repo_ingestion_run_status, 'running'::proxima_code.repo_ingestion_run_status])) AND (finished_at IS NULL))))
);


--
-- Name: repos; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.repos (
    owner_kind proxima_core.owner_kind NOT NULL,
    owner_id uuid NOT NULL,
    repo_id uuid NOT NULL,
    canonical_path text NOT NULL,
    display_name text NOT NULL,
    last_cursor bytea,
    last_polled_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    target_branch text,
    include_globs text[] DEFAULT '{}'::text[] NOT NULL,
    exclude_globs text[] DEFAULT '{}'::text[] NOT NULL,
    CONSTRAINT repos_owner_ref_shape_chk CHECK ((((owner_kind = 'world'::proxima_core.owner_kind) AND (owner_id IS NULL)) OR ((owner_kind = ANY (ARRAY['personal'::proxima_core.owner_kind, 'group'::proxima_core.owner_kind])) AND (owner_id IS NOT NULL)))),
    CONSTRAINT repos_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_kind))
);


--
-- Name: COLUMN repos.include_globs; Type: COMMENT; Schema: proxima_code; Owner: -
--

COMMENT ON COLUMN proxima_code.repos.include_globs IS 'Gitignore-shaped globs limiting ingest to matching paths. Empty means every path is a candidate. Evaluated before exclude_globs.';


--
-- Name: COLUMN repos.exclude_globs; Type: COMMENT; Schema: proxima_code; Owner: -
--

COMMENT ON COLUMN proxima_code.repos.exclude_globs IS 'Gitignore-shaped globs removing paths from ingest. Beats include_globs on conflict. A path that leaves scope is tombstoned by the next snapshot, exactly as a deleted file is.';


--
-- Name: test_requested_criterion_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.test_requested_criterion_v1 (
    test_requested_memory_id uuid NOT NULL,
    criterion_index integer NOT NULL,
    criterion_key text NOT NULL,
    description text NOT NULL,
    required boolean NOT NULL,
    verifier_kind proxima_code.acceptance_verifier_kind NOT NULL,
    verifier_path text,
    verifier_command text[],
    verifier_pattern text,
    verifier_note text,
    CONSTRAINT test_requested_criterion_v1_command_chk CHECK (((verifier_command IS NULL) OR (array_position(verifier_command, NULL::text) IS NULL))),
    CONSTRAINT test_requested_criterion_v1_description_chk CHECK (((char_length(description) >= 1) AND (char_length(description) <= 4000))),
    CONSTRAINT test_requested_criterion_v1_index_chk CHECK ((criterion_index >= 0)),
    CONSTRAINT test_requested_criterion_v1_key_chk CHECK (((char_length(criterion_key) >= 1) AND (char_length(criterion_key) <= 80)))
);


--
-- Name: test_requested_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.test_requested_v1 (
    t uuid NOT NULL,
    repo_id uuid NOT NULL,
    title text NOT NULL,
    instructions text NOT NULL,
    test_key text NOT NULL,
    criteria_count integer NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    depends_on_memory_ids uuid[] DEFAULT ARRAY[]::uuid[] NOT NULL,
    CONSTRAINT test_requested_v1_depends_on_chk
        CHECK ((array_position(depends_on_memory_ids, NULL::uuid) IS NULL)),
    CONSTRAINT test_requested_v1_criteria_count_chk CHECK ((criteria_count > 0)),
    CONSTRAINT test_requested_v1_instructions_chk CHECK (((char_length(instructions) >= 1) AND (char_length(instructions) <= 20000))),
    CONSTRAINT test_requested_v1_test_key_chk CHECK (((char_length(test_key) >= 1) AND (char_length(test_key) <= 240))),
    CONSTRAINT test_requested_v1_title_chk CHECK (((char_length(title) >= 1) AND (char_length(title) <= 240)))
);


--
-- Name: test_result_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.test_result_v1 (
    t uuid NOT NULL,
    test_requested_memory_id uuid NOT NULL,
    repo_id uuid NOT NULL,
    status proxima_code.work_result_status NOT NULL,
    summary text NOT NULL,
    artifact_refs text[] NOT NULL,
    log_excerpt text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT test_result_v1_artifacts_chk CHECK ((array_position(artifact_refs, NULL::text) IS NULL)),
    CONSTRAINT test_result_v1_summary_chk CHECK (((char_length(summary) >= 1) AND (char_length(summary) <= 4000)))
);


--
-- Name: work_requested_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.work_requested_v1 (
    t uuid NOT NULL,
    repo_id uuid NOT NULL,
    title text NOT NULL,
    instructions text NOT NULL,
    request_key text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    depends_on_memory_ids uuid[] DEFAULT ARRAY[]::uuid[] NOT NULL,
    CONSTRAINT work_requested_v1_depends_on_chk
        CHECK ((array_position(depends_on_memory_ids, NULL::uuid) IS NULL)),
    CONSTRAINT work_requested_v1_instructions_chk CHECK (((char_length(instructions) >= 1) AND (char_length(instructions) <= 20000))),
    CONSTRAINT work_requested_v1_request_key_chk CHECK (((char_length(request_key) >= 1) AND (char_length(request_key) <= 240))),
    CONSTRAINT work_requested_v1_title_chk CHECK (((char_length(title) >= 1) AND (char_length(title) <= 240)))
);


--
-- Name: acceptance_criteria_v1 acceptance_criteria_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_criteria_v1
    ADD CONSTRAINT acceptance_criteria_v1_pkey PRIMARY KEY (t);


--
-- Name: acceptance_criterion_v1 acceptance_criterion_v1_criteria_memory_id_criterion_key_key; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_criterion_v1
    ADD CONSTRAINT acceptance_criterion_v1_criteria_memory_id_criterion_key_key UNIQUE (criteria_memory_id, criterion_key);


--
-- Name: acceptance_criterion_v1 acceptance_criterion_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_criterion_v1
    ADD CONSTRAINT acceptance_criterion_v1_pkey PRIMARY KEY (criteria_memory_id, criterion_index);


--
-- Name: acceptance_summary_v1 acceptance_summary_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_summary_v1
    ADD CONSTRAINT acceptance_summary_v1_pkey PRIMARY KEY (t);


--
-- Name: acceptance_verification_v1 acceptance_verification_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_verification_v1
    ADD CONSTRAINT acceptance_verification_v1_pkey PRIMARY KEY (t);


--
-- Name: code_chunk_call_v1 code_chunk_call_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.code_chunk_call_v1
    ADD CONSTRAINT code_chunk_call_v1_pkey PRIMARY KEY (caller_memory_id, callee_memory_id, site_index);


--
-- Name: work_assignment_v1 work_assignment_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.work_assignment_v1
    ADD CONSTRAINT work_assignment_v1_pkey PRIMARY KEY (t);


--
-- Name: code_chunk_v1 code_chunk_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.code_chunk_v1
    ADD CONSTRAINT code_chunk_v1_pkey PRIMARY KEY (t);


--
-- Name: commit_summarizer_self_v1 commit_summarizer_self_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.commit_summarizer_self_v1
    ADD CONSTRAINT commit_summarizer_self_v1_pkey PRIMARY KEY (t);


--
-- Name: commit_summary_v1 commit_summary_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.commit_summary_v1
    ADD CONSTRAINT commit_summary_v1_pkey PRIMARY KEY (t);


--
-- Name: commit_v1 commit_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.commit_v1
    ADD CONSTRAINT commit_v1_pkey PRIMARY KEY (t);


--
-- Name: development_perspective_v1 development_perspective_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.development_perspective_v1
    ADD CONSTRAINT development_perspective_v1_pkey PRIMARY KEY (t);


--
-- Name: engineer_self_v1 engineer_self_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.engineer_self_v1
    ADD CONSTRAINT engineer_self_v1_pkey PRIMARY KEY (t);


--
-- Name: execution_plan_item_v1 execution_plan_item_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.execution_plan_item_v1
    ADD CONSTRAINT execution_plan_item_v1_pkey PRIMARY KEY (plan_memory_id, item_index);


--
-- Name: execution_plan_item_v1 execution_plan_item_v1_plan_memory_id_item_key_key; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.execution_plan_item_v1
    ADD CONSTRAINT execution_plan_item_v1_plan_memory_id_item_key_key UNIQUE (plan_memory_id, item_key);


--
-- Name: execution_plan_v1 execution_plan_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.execution_plan_v1
    ADD CONSTRAINT execution_plan_v1_pkey PRIMARY KEY (t);


--
-- Name: execution_result_v1 execution_result_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.execution_result_v1
    ADD CONSTRAINT execution_result_v1_pkey PRIMARY KEY (t);


--
-- Name: file_revision_v1 file_revision_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.file_revision_v1
    ADD CONSTRAINT file_revision_v1_pkey PRIMARY KEY (t);


--
-- Name: repo_ingestion_runs repo_ingestion_runs_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.repo_ingestion_runs
    ADD CONSTRAINT repo_ingestion_runs_pkey PRIMARY KEY (run_id);


--
-- Name: repos repos_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.repos
    ADD CONSTRAINT repos_pkey PRIMARY KEY (owner_kind, owner_id, repo_id);


--
-- Name: repos repos_unique_path; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.repos
    ADD CONSTRAINT repos_unique_path UNIQUE (owner_kind, owner_id, canonical_path);


--
-- Name: test_requested_criterion_v1 test_requested_criterion_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.test_requested_criterion_v1
    ADD CONSTRAINT test_requested_criterion_v1_pkey PRIMARY KEY (test_requested_memory_id, criterion_index);


--
-- Name: test_requested_criterion_v1 test_requested_criterion_v1_test_requested_memory_id_criter_key; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.test_requested_criterion_v1
    ADD CONSTRAINT test_requested_criterion_v1_test_requested_memory_id_criter_key UNIQUE (test_requested_memory_id, criterion_key);


--
-- Name: test_requested_v1 test_requested_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.test_requested_v1
    ADD CONSTRAINT test_requested_v1_pkey PRIMARY KEY (t);


--
-- Name: test_result_v1 test_result_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.test_result_v1
    ADD CONSTRAINT test_result_v1_pkey PRIMARY KEY (t);


--
-- Name: work_requested_v1 work_requested_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.work_requested_v1
    ADD CONSTRAINT work_requested_v1_pkey PRIMARY KEY (t);


--
-- Name: idx_acceptance_criteria_v1_item; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_acceptance_criteria_v1_item ON proxima_code.acceptance_criteria_v1 USING btree (work_item_memory_id);


--
-- Name: idx_acceptance_summary_v1_item; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_acceptance_summary_v1_item ON proxima_code.acceptance_summary_v1 USING btree (work_item_memory_id, created_at DESC);


--
-- Name: idx_acceptance_verification_v1_item; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_acceptance_verification_v1_item ON proxima_code.acceptance_verification_v1 USING btree (work_item_memory_id, criterion_key, status);


--
-- Name: idx_code_chunk_v1_chunk_type; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_code_chunk_v1_chunk_type ON proxima_code.code_chunk_v1 USING btree (chunk_type);


--
-- Name: idx_code_chunk_v1_file_path_trgm; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_code_chunk_v1_file_path_trgm ON proxima_code.code_chunk_v1 USING gin (lower(file_path) public.gin_trgm_ops);


--
-- Name: idx_code_chunk_v1_nk; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_code_chunk_v1_nk ON proxima_code.code_chunk_v1 USING btree (repo_id, file_path, chunk_index);


--
-- Name: idx_code_chunk_v1_search_tsv; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_code_chunk_v1_search_tsv ON proxima_code.code_chunk_v1 USING gin (search_tsv);


--
-- Name: idx_code_chunk_v1_text_trgm; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_code_chunk_v1_text_trgm ON proxima_code.code_chunk_v1 USING gin (lower(text) public.gin_trgm_ops);


--
-- Name: idx_commit_summary_v1_repo_sha; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_commit_summary_v1_repo_sha ON proxima_code.commit_summary_v1 USING btree (repo_id, commit_sha);


--
-- Name: idx_commit_summary_v1_search; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_commit_summary_v1_search ON proxima_code.commit_summary_v1 USING gin (to_tsvector('simple'::regconfig, ((((commit_sha || ' '::text) || summary) || ' '::text) || proxima_code.text_array_search(key_files))));


--
-- Name: idx_commit_v1_message_search; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_commit_v1_message_search ON proxima_code.commit_v1 USING gin (to_tsvector('simple'::regconfig, ((sha || ' '::text) || message)));


--
-- Name: idx_commit_v1_repo_sha; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_commit_v1_repo_sha ON proxima_code.commit_v1 USING btree (repo_id, sha);


--
-- Name: idx_development_perspective_v1_repo; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_development_perspective_v1_repo ON proxima_code.development_perspective_v1 USING btree (repo_id);


--
-- Name: idx_execution_plan_v1_goal; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_execution_plan_v1_goal ON proxima_code.execution_plan_v1 USING btree (goal_activated_memory_id);


--
-- Name: idx_execution_plan_v1_repo_key; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_execution_plan_v1_repo_key ON proxima_code.execution_plan_v1 USING btree (repo_id, plan_key);


--
-- Name: idx_execution_result_v1_work; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_execution_result_v1_work ON proxima_code.execution_result_v1 USING btree (work_requested_memory_id, created_at DESC);


--
-- Name: idx_file_revision_v1_nk; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_file_revision_v1_nk ON proxima_code.file_revision_v1 USING btree (repo_id, file_path);


--
-- Name: idx_file_revision_v1_path_search; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_file_revision_v1_path_search ON proxima_code.file_revision_v1 USING gin (to_tsvector('simple'::regconfig, file_path));


--
-- Name: idx_test_requested_v1_repo; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_test_requested_v1_repo ON proxima_code.test_requested_v1 USING btree (repo_id);


--
-- Name: idx_test_requested_v1_repo_key; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE UNIQUE INDEX idx_test_requested_v1_repo_key ON proxima_code.test_requested_v1 USING btree (repo_id, test_key);


--
-- Name: idx_test_result_v1_test; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_test_result_v1_test ON proxima_code.test_result_v1 USING btree (test_requested_memory_id, created_at DESC);


--
-- Name: idx_work_requested_v1_repo; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_work_requested_v1_repo ON proxima_code.work_requested_v1 USING btree (repo_id);


--
-- Name: idx_work_requested_v1_repo_key; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE UNIQUE INDEX idx_work_requested_v1_repo_key ON proxima_code.work_requested_v1 USING btree (repo_id, request_key);


--
-- Name: repo_ingestion_runs_by_repo; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX repo_ingestion_runs_by_repo ON proxima_code.repo_ingestion_runs USING btree (owner_kind, owner_id, repo_id, started_at DESC);


--
-- Name: repo_ingestion_runs_one_active; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE UNIQUE INDEX repo_ingestion_runs_one_active ON proxima_code.repo_ingestion_runs USING btree (owner_kind, owner_id, repo_id) WHERE (status = ANY (ARRAY['queued'::proxima_code.repo_ingestion_run_status, 'running'::proxima_code.repo_ingestion_run_status]));


--
-- Name: acceptance_criteria_v1 acceptance_criteria_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER acceptance_criteria_v1_append_only BEFORE UPDATE ON proxima_code.acceptance_criteria_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: acceptance_criterion_v1 acceptance_criterion_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER acceptance_criterion_v1_append_only BEFORE UPDATE ON proxima_code.acceptance_criterion_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: acceptance_summary_v1 acceptance_summary_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER acceptance_summary_v1_append_only BEFORE UPDATE ON proxima_code.acceptance_summary_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: acceptance_verification_v1 acceptance_verification_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER acceptance_verification_v1_append_only BEFORE UPDATE ON proxima_code.acceptance_verification_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: code_chunk_v1 code_chunk_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER code_chunk_v1_append_only BEFORE UPDATE ON proxima_code.code_chunk_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: code_chunk_call_v1 code_chunk_call_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER code_chunk_call_v1_append_only BEFORE UPDATE ON proxima_code.code_chunk_call_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: work_assignment_v1 work_assignment_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER work_assignment_v1_append_only BEFORE UPDATE ON proxima_code.work_assignment_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: commit_summarizer_self_v1 commit_summarizer_self_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER commit_summarizer_self_v1_append_only BEFORE UPDATE ON proxima_code.commit_summarizer_self_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: commit_summary_v1 commit_summary_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER commit_summary_v1_append_only BEFORE UPDATE ON proxima_code.commit_summary_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: commit_v1 commit_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER commit_v1_append_only BEFORE UPDATE ON proxima_code.commit_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: development_perspective_v1 development_perspective_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER development_perspective_v1_append_only BEFORE UPDATE ON proxima_code.development_perspective_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: engineer_self_v1 engineer_self_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER engineer_self_v1_append_only BEFORE UPDATE ON proxima_code.engineer_self_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: execution_plan_item_v1 execution_plan_item_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER execution_plan_item_v1_append_only BEFORE UPDATE ON proxima_code.execution_plan_item_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: execution_plan_v1 execution_plan_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER execution_plan_v1_append_only BEFORE UPDATE ON proxima_code.execution_plan_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: execution_result_v1 execution_result_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER execution_result_v1_append_only BEFORE UPDATE ON proxima_code.execution_result_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: file_revision_v1 file_revision_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER file_revision_v1_append_only BEFORE UPDATE ON proxima_code.file_revision_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: test_requested_criterion_v1 test_requested_criterion_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER test_requested_criterion_v1_append_only BEFORE UPDATE ON proxima_code.test_requested_criterion_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: test_requested_v1 test_requested_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER test_requested_v1_append_only BEFORE UPDATE ON proxima_code.test_requested_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: test_result_v1 test_result_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER test_result_v1_append_only BEFORE UPDATE ON proxima_code.test_result_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: work_requested_v1 work_requested_v1_append_only; Type: TRIGGER; Schema: proxima_code; Owner: -
--

CREATE TRIGGER work_requested_v1_append_only BEFORE UPDATE ON proxima_code.work_requested_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


--
-- Name: acceptance_criteria_v1 acceptance_criteria_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_criteria_v1
    ADD CONSTRAINT acceptance_criteria_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: acceptance_criteria_v1 acceptance_criteria_v1_work_item_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_criteria_v1
    ADD CONSTRAINT acceptance_criteria_v1_work_item_memory_id_fkey FOREIGN KEY (work_item_memory_id) REFERENCES proxima_core.memory(t);


--
-- Name: acceptance_criterion_v1 acceptance_criterion_v1_criteria_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_criterion_v1
    ADD CONSTRAINT acceptance_criterion_v1_criteria_memory_id_fkey FOREIGN KEY (criteria_memory_id) REFERENCES proxima_code.acceptance_criteria_v1(t) ON DELETE CASCADE;


--
-- Name: acceptance_summary_v1 acceptance_summary_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_summary_v1
    ADD CONSTRAINT acceptance_summary_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: acceptance_summary_v1 acceptance_summary_v1_work_item_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_summary_v1
    ADD CONSTRAINT acceptance_summary_v1_work_item_memory_id_fkey FOREIGN KEY (work_item_memory_id) REFERENCES proxima_core.memory(t);


--
-- Name: acceptance_verification_v1 acceptance_verification_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_verification_v1
    ADD CONSTRAINT acceptance_verification_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: acceptance_verification_v1 acceptance_verification_v1_verifier_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_verification_v1
    ADD CONSTRAINT acceptance_verification_v1_verifier_memory_id_fkey FOREIGN KEY (verifier_memory_id) REFERENCES proxima_core.memory(t);


--
-- Name: acceptance_verification_v1 acceptance_verification_v1_work_item_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.acceptance_verification_v1
    ADD CONSTRAINT acceptance_verification_v1_work_item_memory_id_fkey FOREIGN KEY (work_item_memory_id) REFERENCES proxima_core.memory(t);


--
-- Name: code_chunk_call_v1 code_chunk_call_v1_caller_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.code_chunk_call_v1
    ADD CONSTRAINT code_chunk_call_v1_caller_memory_id_fkey FOREIGN KEY (caller_memory_id) REFERENCES proxima_code.code_chunk_v1(t) ON DELETE CASCADE;


--
-- Name: work_assignment_v1 work_assignment_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.work_assignment_v1
    ADD CONSTRAINT work_assignment_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: work_assignment_v1 work_assignment_v1_target_perspective_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.work_assignment_v1
    ADD CONSTRAINT work_assignment_v1_target_perspective_memory_id_fkey FOREIGN KEY (target_perspective_memory_id) REFERENCES proxima_core.memory(t);


--
-- Name: work_assignment_v1 work_assignment_v1_work_item_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.work_assignment_v1
    ADD CONSTRAINT work_assignment_v1_work_item_memory_id_fkey FOREIGN KEY (work_item_memory_id) REFERENCES proxima_core.memory(t);


--
-- Name: idx_code_chunk_call_callee; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_code_chunk_call_callee ON proxima_code.code_chunk_call_v1 USING btree (callee_memory_id);


--
-- Name: idx_work_assignment_work_item; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_work_assignment_work_item ON proxima_code.work_assignment_v1 USING btree (work_item_memory_id);


--
-- Name: code_chunk_v1 code_chunk_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.code_chunk_v1
    ADD CONSTRAINT code_chunk_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: commit_summarizer_self_v1 commit_summarizer_self_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.commit_summarizer_self_v1
    ADD CONSTRAINT commit_summarizer_self_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: commit_summary_v1 commit_summary_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.commit_summary_v1
    ADD CONSTRAINT commit_summary_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: commit_v1 commit_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.commit_v1
    ADD CONSTRAINT commit_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: development_perspective_v1 development_perspective_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.development_perspective_v1
    ADD CONSTRAINT development_perspective_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: engineer_self_v1 engineer_self_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.engineer_self_v1
    ADD CONSTRAINT engineer_self_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: execution_plan_item_v1 execution_plan_item_v1_plan_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.execution_plan_item_v1
    ADD CONSTRAINT execution_plan_item_v1_plan_memory_id_fkey FOREIGN KEY (plan_memory_id) REFERENCES proxima_code.execution_plan_v1(t) ON DELETE CASCADE;


--
-- Name: execution_plan_v1 execution_plan_v1_goal_activated_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.execution_plan_v1
    ADD CONSTRAINT execution_plan_v1_goal_activated_memory_id_fkey FOREIGN KEY (goal_activated_memory_id) REFERENCES proxima_core.memory(t);


--
-- Name: execution_plan_v1 execution_plan_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.execution_plan_v1
    ADD CONSTRAINT execution_plan_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: execution_result_v1 execution_result_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.execution_result_v1
    ADD CONSTRAINT execution_result_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: execution_result_v1 execution_result_v1_work_requested_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.execution_result_v1
    ADD CONSTRAINT execution_result_v1_work_requested_memory_id_fkey FOREIGN KEY (work_requested_memory_id) REFERENCES proxima_core.memory(t);


--
-- Name: file_revision_v1 file_revision_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.file_revision_v1
    ADD CONSTRAINT file_revision_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: repo_ingestion_runs runs_repo_fk; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.repo_ingestion_runs
    ADD CONSTRAINT runs_repo_fk FOREIGN KEY (owner_kind, owner_id, repo_id) REFERENCES proxima_code.repos(owner_kind, owner_id, repo_id) ON DELETE CASCADE;


--
-- Name: test_requested_criterion_v1 test_requested_criterion_v1_test_requested_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.test_requested_criterion_v1
    ADD CONSTRAINT test_requested_criterion_v1_test_requested_memory_id_fkey FOREIGN KEY (test_requested_memory_id) REFERENCES proxima_code.test_requested_v1(t) ON DELETE CASCADE;


--
-- Name: test_requested_v1 test_requested_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.test_requested_v1
    ADD CONSTRAINT test_requested_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: test_result_v1 test_result_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.test_result_v1
    ADD CONSTRAINT test_result_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


--
-- Name: test_result_v1 test_result_v1_test_requested_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.test_result_v1
    ADD CONSTRAINT test_result_v1_test_requested_memory_id_fkey FOREIGN KEY (test_requested_memory_id) REFERENCES proxima_core.memory(t);


--
-- Name: work_requested_v1 work_requested_v1_t_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.work_requested_v1
    ADD CONSTRAINT work_requested_v1_t_fkey FOREIGN KEY (t) REFERENCES proxima_core.memory(t);


