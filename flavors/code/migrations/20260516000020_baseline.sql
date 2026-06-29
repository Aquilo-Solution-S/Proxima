-- Proxima code-flavor schema — destructive v0.0.4 baseline.
-- Generated from the folded proxima_code schema and hand-corrected in PR2 for
-- direct OwnerRef columns and removal of pre-v0.0.4 owner-org compatibility.
-- Existing pre-v0.0.4 databases must export/reset before this baseline runs.

CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE SCHEMA proxima_code;


--
-- Name: file_state; Type: TYPE; Schema: proxima_code; Owner: -
--

CREATE TYPE proxima_code.file_state AS ENUM (
    'Present',
    'Tombstone'
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

CREATE TYPE proxima_code.execution_plan_item_kind AS ENUM (
    'work',
    'test'
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
-- Name: acceptance_verification_status; Type: TYPE; Schema: proxima_code; Owner: -
--

CREATE TYPE proxima_code.acceptance_verification_status AS ENUM (
    'passed',
    'failed',
    'skipped',
    'blocked'
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
-- Name: text_array_search(text[]); Type: FUNCTION; Schema: proxima_code; Owner: -
--

CREATE FUNCTION proxima_code.text_array_search(items text[]) RETURNS text
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$
    SELECT array_to_string(items, ' ')
$$;




--
-- Name: code_calls_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.code_calls_v1 (
    edge_id uuid NOT NULL,
    callsite_byte_start integer NOT NULL,
    callsite_byte_end integer NOT NULL,
    callee_name text NOT NULL,
    is_dynamic boolean NOT NULL,
    CONSTRAINT code_calls_v1_byte_range_chk CHECK ((callsite_byte_end >= callsite_byte_start))
);


--
-- Name: code_chunk_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.code_chunk_v1 (
    memory_id uuid NOT NULL,
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
    CONSTRAINT code_chunk_v1_chunk_index_chk CHECK ((chunk_index >= 0))
);


--
-- Name: commit_summarizer_self_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.commit_summarizer_self_v1 (
    memory_id uuid NOT NULL,
    display_name text NOT NULL,
    purpose text NOT NULL
);


--
-- Name: commit_summary_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.commit_summary_v1 (
    memory_id uuid NOT NULL,
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
    memory_id uuid NOT NULL,
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
    memory_id uuid NOT NULL,
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
    memory_id uuid NOT NULL,
    display_name text NOT NULL,
    purpose text NOT NULL
);


--
-- Name: work_requested_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.work_requested_v1 (
    memory_id uuid NOT NULL,
    repo_id uuid NOT NULL,
    title text NOT NULL,
    instructions text NOT NULL,
    request_key text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT work_requested_v1_instructions_chk CHECK (((char_length(instructions) >= 1) AND (char_length(instructions) <= 20000))),
    CONSTRAINT work_requested_v1_request_key_chk CHECK (((char_length(request_key) >= 1) AND (char_length(request_key) <= 240))),
    CONSTRAINT work_requested_v1_title_chk CHECK (((char_length(title) >= 1) AND (char_length(title) <= 240)))
);


--
-- Name: file_revision_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.file_revision_v1 (
    memory_id uuid NOT NULL,
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
    owner_kind proxima_core.owner_ref_kind NOT NULL,
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
    CONSTRAINT runs_finished_when_terminal_chk CHECK ((((status = ANY (ARRAY['succeeded'::proxima_code.repo_ingestion_run_status, 'failed'::proxima_code.repo_ingestion_run_status])) AND (finished_at IS NOT NULL)) OR ((status = ANY (ARRAY['queued'::proxima_code.repo_ingestion_run_status, 'running'::proxima_code.repo_ingestion_run_status])) AND (finished_at IS NULL)))),
    CONSTRAINT repo_ingestion_runs_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT repo_ingestion_runs_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


--
-- Name: repos; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.repos (
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    repo_id uuid NOT NULL,
    canonical_path text NOT NULL,
    display_name text NOT NULL,
    last_cursor bytea,
    last_polled_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    target_branch text,
    CONSTRAINT repos_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT repos_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind))
);


--
-- Name: code_calls_v1 code_calls_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.code_calls_v1
    ADD CONSTRAINT code_calls_v1_pkey PRIMARY KEY (edge_id);


--
-- Name: code_chunk_v1 code_chunk_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.code_chunk_v1
    ADD CONSTRAINT code_chunk_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: commit_summarizer_self_v1 commit_summarizer_self_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.commit_summarizer_self_v1
    ADD CONSTRAINT commit_summarizer_self_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: commit_summary_v1 commit_summary_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.commit_summary_v1
    ADD CONSTRAINT commit_summary_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: commit_v1 commit_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.commit_v1
    ADD CONSTRAINT commit_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: development_perspective_v1 development_perspective_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.development_perspective_v1
    ADD CONSTRAINT development_perspective_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: engineer_self_v1 engineer_self_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.engineer_self_v1
    ADD CONSTRAINT engineer_self_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: work_requested_v1 work_requested_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.work_requested_v1
    ADD CONSTRAINT work_requested_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: file_revision_v1 file_revision_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.file_revision_v1
    ADD CONSTRAINT file_revision_v1_pkey PRIMARY KEY (memory_id);


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
-- Name: idx_code_chunk_v1_text_search; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_code_chunk_v1_text_search ON proxima_code.code_chunk_v1 USING gin (to_tsvector('simple'::regconfig, ((file_path || ' '::text) || text)));


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
-- Name: idx_work_requested_v1_repo; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_work_requested_v1_repo ON proxima_code.work_requested_v1 USING btree (repo_id);


--
-- Name: idx_work_requested_v1_repo_key; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE UNIQUE INDEX idx_work_requested_v1_repo_key ON proxima_code.work_requested_v1 USING btree (repo_id, request_key);


--
-- Name: idx_file_revision_v1_nk; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_file_revision_v1_nk ON proxima_code.file_revision_v1 USING btree (repo_id, file_path);


--
-- Name: idx_file_revision_v1_path_search; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_file_revision_v1_path_search ON proxima_code.file_revision_v1 USING gin (to_tsvector('simple'::regconfig, file_path));


--
-- Name: repo_ingestion_runs_by_repo; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX repo_ingestion_runs_by_repo ON proxima_code.repo_ingestion_runs USING btree (owner_kind, owner_id, repo_id, started_at DESC);


--
-- Name: repo_ingestion_runs_one_active; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE UNIQUE INDEX repo_ingestion_runs_one_active ON proxima_code.repo_ingestion_runs USING btree (owner_kind, owner_id, repo_id) WHERE (status = ANY (ARRAY['queued'::proxima_code.repo_ingestion_run_status, 'running'::proxima_code.repo_ingestion_run_status]));


--
-- Name: code_calls_v1 code_calls_v1_edge_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.code_calls_v1
    ADD CONSTRAINT code_calls_v1_edge_id_fkey FOREIGN KEY (edge_id) REFERENCES proxima_core.edges(edge_id);


--
-- Name: code_chunk_v1 code_chunk_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.code_chunk_v1
    ADD CONSTRAINT code_chunk_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: commit_summarizer_self_v1 commit_summarizer_self_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.commit_summarizer_self_v1
    ADD CONSTRAINT commit_summarizer_self_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: commit_summary_v1 commit_summary_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.commit_summary_v1
    ADD CONSTRAINT commit_summary_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: commit_v1 commit_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.commit_v1
    ADD CONSTRAINT commit_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: development_perspective_v1 development_perspective_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.development_perspective_v1
    ADD CONSTRAINT development_perspective_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: engineer_self_v1 engineer_self_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.engineer_self_v1
    ADD CONSTRAINT engineer_self_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: work_requested_v1 work_requested_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.work_requested_v1
    ADD CONSTRAINT work_requested_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: file_revision_v1 file_revision_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.file_revision_v1
    ADD CONSTRAINT file_revision_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: repo_ingestion_runs runs_repo_fk; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.repo_ingestion_runs
    ADD CONSTRAINT runs_repo_fk FOREIGN KEY (owner_kind, owner_id, repo_id) REFERENCES proxima_code.repos(owner_kind, owner_id, repo_id) ON DELETE CASCADE;


--

--
-- Name: acceptance_criteria_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.acceptance_criteria_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    work_item_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    criteria_count integer NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT acceptance_criteria_v1_count_chk CHECK (criteria_count > 0)
);

CREATE INDEX idx_acceptance_criteria_v1_item ON proxima_code.acceptance_criteria_v1 USING btree (work_item_memory_id);

CREATE TABLE proxima_code.acceptance_criterion_v1 (
    criteria_memory_id uuid NOT NULL REFERENCES proxima_code.acceptance_criteria_v1(memory_id) ON DELETE CASCADE,
    criterion_index integer NOT NULL,
    criterion_key text NOT NULL,
    description text NOT NULL,
    required boolean NOT NULL,
    verifier_kind proxima_code.acceptance_verifier_kind NOT NULL,
    verifier_path text,
    verifier_command text[],
    verifier_pattern text,
    verifier_note text,
    PRIMARY KEY (criteria_memory_id, criterion_index),
    CONSTRAINT acceptance_criterion_v1_index_chk CHECK (criterion_index >= 0),
    CONSTRAINT acceptance_criterion_v1_key_chk CHECK (((char_length(criterion_key) >= 1) AND (char_length(criterion_key) <= 80))),
    CONSTRAINT acceptance_criterion_v1_description_chk CHECK (((char_length(description) >= 1) AND (char_length(description) <= 4000))),
    CONSTRAINT acceptance_criterion_v1_command_chk CHECK ((verifier_command IS NULL) OR (array_position(verifier_command, NULL) IS NULL)),
    UNIQUE (criteria_memory_id, criterion_key)
);


--
-- Name: test_requested_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.test_requested_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    repo_id uuid NOT NULL,
    title text NOT NULL,
    instructions text NOT NULL,
    test_key text NOT NULL,
    criteria_count integer NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT test_requested_v1_title_chk CHECK (((char_length(title) >= 1) AND (char_length(title) <= 240))),
    CONSTRAINT test_requested_v1_instructions_chk CHECK (((char_length(instructions) >= 1) AND (char_length(instructions) <= 20000))),
    CONSTRAINT test_requested_v1_test_key_chk CHECK (((char_length(test_key) >= 1) AND (char_length(test_key) <= 240))),
    CONSTRAINT test_requested_v1_criteria_count_chk CHECK (criteria_count > 0)
);

CREATE INDEX idx_test_requested_v1_repo ON proxima_code.test_requested_v1 USING btree (repo_id);
CREATE UNIQUE INDEX idx_test_requested_v1_repo_key ON proxima_code.test_requested_v1 USING btree (repo_id, test_key);

CREATE TABLE proxima_code.test_requested_criterion_v1 (
    test_requested_memory_id uuid NOT NULL REFERENCES proxima_code.test_requested_v1(memory_id) ON DELETE CASCADE,
    criterion_index integer NOT NULL,
    criterion_key text NOT NULL,
    description text NOT NULL,
    required boolean NOT NULL,
    verifier_kind proxima_code.acceptance_verifier_kind NOT NULL,
    verifier_path text,
    verifier_command text[],
    verifier_pattern text,
    verifier_note text,
    PRIMARY KEY (test_requested_memory_id, criterion_index),
    CONSTRAINT test_requested_criterion_v1_index_chk CHECK (criterion_index >= 0),
    CONSTRAINT test_requested_criterion_v1_key_chk CHECK (((char_length(criterion_key) >= 1) AND (char_length(criterion_key) <= 80))),
    CONSTRAINT test_requested_criterion_v1_description_chk CHECK (((char_length(description) >= 1) AND (char_length(description) <= 4000))),
    CONSTRAINT test_requested_criterion_v1_command_chk CHECK ((verifier_command IS NULL) OR (array_position(verifier_command, NULL) IS NULL)),
    UNIQUE (test_requested_memory_id, criterion_key)
);


--
-- Name: execution_plan_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.execution_plan_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    repo_id uuid NOT NULL,
    plan_key text NOT NULL,
    goal_activated_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    summary text NOT NULL,
    item_count integer NOT NULL,
    evidence_memory_ids uuid[] NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT execution_plan_v1_plan_key_chk CHECK (((char_length(plan_key) >= 1) AND (char_length(plan_key) <= 240))),
    CONSTRAINT execution_plan_v1_summary_chk CHECK (((char_length(summary) >= 1) AND (char_length(summary) <= 4000))),
    CONSTRAINT execution_plan_v1_item_count_chk CHECK (item_count > 0)
);

CREATE INDEX idx_execution_plan_v1_repo_key ON proxima_code.execution_plan_v1 USING btree (repo_id, plan_key);
CREATE INDEX idx_execution_plan_v1_goal ON proxima_code.execution_plan_v1 USING btree (goal_activated_memory_id);

CREATE TABLE proxima_code.execution_plan_item_v1 (
    plan_memory_id uuid NOT NULL REFERENCES proxima_code.execution_plan_v1(memory_id) ON DELETE CASCADE,
    item_index integer NOT NULL,
    item_key text NOT NULL,
    kind proxima_code.execution_plan_item_kind NOT NULL,
    title text NOT NULL,
    depends_on text[] NOT NULL,
    request_key text NOT NULL,
    PRIMARY KEY (plan_memory_id, item_index),
    CONSTRAINT execution_plan_item_v1_index_chk CHECK (item_index >= 0),
    CONSTRAINT execution_plan_item_v1_key_chk CHECK (((char_length(item_key) >= 1) AND (char_length(item_key) <= 120))),
    CONSTRAINT execution_plan_item_v1_title_chk CHECK (((char_length(title) >= 1) AND (char_length(title) <= 240))),
    CONSTRAINT execution_plan_item_v1_request_key_chk CHECK (((char_length(request_key) >= 1) AND (char_length(request_key) <= 240))),
    CONSTRAINT execution_plan_item_v1_depends_chk CHECK (array_position(depends_on, NULL) IS NULL),
    UNIQUE (plan_memory_id, item_key)
);


--
-- Name: execution_result_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.execution_result_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    work_requested_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    repo_id uuid NOT NULL,
    status proxima_code.work_result_status NOT NULL,
    summary text NOT NULL,
    artifact_refs text[] NOT NULL,
    log_excerpt text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT execution_result_v1_summary_chk CHECK (((char_length(summary) >= 1) AND (char_length(summary) <= 4000))),
    CONSTRAINT execution_result_v1_artifacts_chk CHECK (array_position(artifact_refs, NULL) IS NULL)
);

CREATE INDEX idx_execution_result_v1_work ON proxima_code.execution_result_v1 USING btree (work_requested_memory_id, created_at DESC);


--
-- Name: test_result_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.test_result_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    test_requested_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    repo_id uuid NOT NULL,
    status proxima_code.work_result_status NOT NULL,
    summary text NOT NULL,
    artifact_refs text[] NOT NULL,
    log_excerpt text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT test_result_v1_summary_chk CHECK (((char_length(summary) >= 1) AND (char_length(summary) <= 4000))),
    CONSTRAINT test_result_v1_artifacts_chk CHECK (array_position(artifact_refs, NULL) IS NULL)
);

CREATE INDEX idx_test_result_v1_test ON proxima_code.test_result_v1 USING btree (test_requested_memory_id, created_at DESC);


--
-- Name: acceptance_verification_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.acceptance_verification_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    work_item_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    criterion_key text NOT NULL,
    status proxima_code.acceptance_verification_status NOT NULL,
    summary text NOT NULL,
    artifact_refs text[] NOT NULL,
    verifier_memory_id uuid REFERENCES proxima_core.memories(memory_id),
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT acceptance_verification_v1_criterion_key_chk CHECK (((char_length(criterion_key) >= 1) AND (char_length(criterion_key) <= 80))),
    CONSTRAINT acceptance_verification_v1_summary_chk CHECK (((char_length(summary) >= 1) AND (char_length(summary) <= 4000))),
    CONSTRAINT acceptance_verification_v1_artifacts_chk CHECK (array_position(artifact_refs, NULL) IS NULL)
);

CREATE INDEX idx_acceptance_verification_v1_item ON proxima_code.acceptance_verification_v1 USING btree (work_item_memory_id, criterion_key, status);


--
-- Name: acceptance_summary_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.acceptance_summary_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    work_item_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    repo_id uuid NOT NULL,
    passed_required boolean NOT NULL,
    summary text NOT NULL,
    verification_memory_ids uuid[] NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT acceptance_summary_v1_summary_chk CHECK (((char_length(summary) >= 1) AND (char_length(summary) <= 4000)))
);

CREATE INDEX idx_acceptance_summary_v1_item ON proxima_code.acceptance_summary_v1 USING btree (work_item_memory_id, created_at DESC);
