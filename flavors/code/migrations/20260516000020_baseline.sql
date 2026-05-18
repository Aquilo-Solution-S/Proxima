-- Baseline migration for the proxima_code schema. Generated with
-- `pg_dump --schema-only --no-owner --no-privileges --no-comments -n proxima_code`
-- and sanitized (psql session directives stripped).
-- Squashed from pre-2026-05-16 migration history; do not edit by hand.

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
-- Name: workspace_decision; Type: TYPE; Schema: proxima_code; Owner: -
--

CREATE TYPE proxima_code.workspace_decision AS ENUM (
    'rejected',
    'retry_requested',
    'accepted',
    'merged'
);


--
-- Name: workspace_review_verdict; Type: TYPE; Schema: proxima_code; Owner: -
--

CREATE TYPE proxima_code.workspace_review_verdict AS ENUM (
    'approved',
    'rejected',
    'needs_user'
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
-- Name: execution_request_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.execution_request_v1 (
    memory_id uuid NOT NULL,
    repo_id uuid NOT NULL,
    title text NOT NULL,
    instructions text NOT NULL,
    request_key text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT execution_request_v1_instructions_chk CHECK (((char_length(instructions) >= 1) AND (char_length(instructions) <= 20000))),
    CONSTRAINT execution_request_v1_request_key_chk CHECK (((char_length(request_key) >= 1) AND (char_length(request_key) <= 240))),
    CONSTRAINT execution_request_v1_title_chk CHECK (((char_length(title) >= 1) AND (char_length(title) <= 240)))
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
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
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
    CONSTRAINT runs_finished_when_terminal_chk CHECK ((((status = ANY (ARRAY['succeeded'::proxima_code.repo_ingestion_run_status, 'failed'::proxima_code.repo_ingestion_run_status])) AND (finished_at IS NOT NULL)) OR ((status = ANY (ARRAY['queued'::proxima_code.repo_ingestion_run_status, 'running'::proxima_code.repo_ingestion_run_status])) AND (finished_at IS NULL))))
);


--
-- Name: repos; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.repos (
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    repo_id uuid NOT NULL,
    canonical_path text NOT NULL,
    display_name text NOT NULL,
    last_cursor bytea,
    last_polled_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    target_branch text
);


--
-- Name: workspace_decision_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.workspace_decision_v1 (
    memory_id uuid NOT NULL,
    workspace_run_memory_id uuid NOT NULL,
    decision proxima_code.workspace_decision NOT NULL,
    decided_at timestamp with time zone DEFAULT now() NOT NULL,
    reason_text text,
    decided_by_owner_id uuid NOT NULL
);


--
-- Name: workspace_review_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.workspace_review_v1 (
    memory_id uuid NOT NULL,
    workspace_run_memory_id uuid NOT NULL,
    execution_request_memory_id uuid NOT NULL,
    verdict proxima_code.workspace_review_verdict NOT NULL,
    round_index integer NOT NULL,
    summary text NOT NULL,
    findings_json jsonb NOT NULL,
    correction_instructions text,
    verification_summary text,
    reviewed_at timestamp with time zone DEFAULT now() NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT workspace_review_v1_correction_chk CHECK (((correction_instructions IS NULL) OR ((char_length(correction_instructions) >= 1) AND (char_length(correction_instructions) <= 12000)))),
    CONSTRAINT workspace_review_v1_round_chk CHECK ((round_index >= 0)),
    CONSTRAINT workspace_review_v1_summary_chk CHECK (((char_length(summary) >= 1) AND (char_length(summary) <= 4000))),
    CONSTRAINT workspace_review_v1_verification_chk CHECK (((verification_summary IS NULL) OR ((char_length(verification_summary) >= 1) AND (char_length(verification_summary) <= 4000))))
);


--
-- Name: workspace_run_v1; Type: TABLE; Schema: proxima_code; Owner: -
--

CREATE TABLE proxima_code.workspace_run_v1 (
    memory_id uuid NOT NULL,
    wake_invocation_id uuid NOT NULL,
    repo_id uuid NOT NULL,
    target_branch text NOT NULL,
    worktree_path text NOT NULL,
    branch_name text NOT NULL,
    parent_sha text NOT NULL,
    head_sha text NOT NULL,
    diff_stat_json jsonb NOT NULL,
    exit_code integer,
    stdout_tail text,
    stderr_tail text,
    duration_ms bigint,
    created_at timestamp with time zone DEFAULT now() NOT NULL
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
-- Name: execution_request_v1 execution_request_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.execution_request_v1
    ADD CONSTRAINT execution_request_v1_pkey PRIMARY KEY (memory_id);


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
    ADD CONSTRAINT repos_pkey PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id, repo_id);


--
-- Name: repos repos_unique_path; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.repos
    ADD CONSTRAINT repos_unique_path UNIQUE (owner_principal_kind, owner_principal_id, owner_org_id, canonical_path);


--
-- Name: workspace_decision_v1 workspace_decision_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.workspace_decision_v1
    ADD CONSTRAINT workspace_decision_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: workspace_review_v1 workspace_review_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.workspace_review_v1
    ADD CONSTRAINT workspace_review_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: workspace_run_v1 workspace_run_v1_pkey; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.workspace_run_v1
    ADD CONSTRAINT workspace_run_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: workspace_run_v1 workspace_run_v1_wake_invocation_id_key; Type: CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.workspace_run_v1
    ADD CONSTRAINT workspace_run_v1_wake_invocation_id_key UNIQUE (wake_invocation_id);


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
-- Name: idx_execution_request_v1_repo; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_execution_request_v1_repo ON proxima_code.execution_request_v1 USING btree (repo_id);


--
-- Name: idx_execution_request_v1_repo_key; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE UNIQUE INDEX idx_execution_request_v1_repo_key ON proxima_code.execution_request_v1 USING btree (repo_id, request_key);


--
-- Name: idx_file_revision_v1_nk; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_file_revision_v1_nk ON proxima_code.file_revision_v1 USING btree (repo_id, file_path);


--
-- Name: idx_file_revision_v1_path_search; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_file_revision_v1_path_search ON proxima_code.file_revision_v1 USING gin (to_tsvector('simple'::regconfig, file_path));


--
-- Name: idx_workspace_decision_v1_run; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_workspace_decision_v1_run ON proxima_code.workspace_decision_v1 USING btree (workspace_run_memory_id);


--
-- Name: idx_workspace_review_v1_request; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_workspace_review_v1_request ON proxima_code.workspace_review_v1 USING btree (execution_request_memory_id, round_index);


--
-- Name: idx_workspace_review_v1_run; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_workspace_review_v1_run ON proxima_code.workspace_review_v1 USING btree (workspace_run_memory_id, created_at DESC);


--
-- Name: idx_workspace_run_v1_repo; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX idx_workspace_run_v1_repo ON proxima_code.workspace_run_v1 USING btree (repo_id);


--
-- Name: repo_ingestion_runs_by_repo; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE INDEX repo_ingestion_runs_by_repo ON proxima_code.repo_ingestion_runs USING btree (owner_principal_kind, owner_principal_id, owner_org_id, repo_id, started_at DESC);


--
-- Name: repo_ingestion_runs_one_active; Type: INDEX; Schema: proxima_code; Owner: -
--

CREATE UNIQUE INDEX repo_ingestion_runs_one_active ON proxima_code.repo_ingestion_runs USING btree (owner_principal_kind, owner_principal_id, owner_org_id, repo_id) WHERE (status = ANY (ARRAY['queued'::proxima_code.repo_ingestion_run_status, 'running'::proxima_code.repo_ingestion_run_status]));


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
-- Name: execution_request_v1 execution_request_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.execution_request_v1
    ADD CONSTRAINT execution_request_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: file_revision_v1 file_revision_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.file_revision_v1
    ADD CONSTRAINT file_revision_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: repo_ingestion_runs runs_repo_fk; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.repo_ingestion_runs
    ADD CONSTRAINT runs_repo_fk FOREIGN KEY (owner_principal_kind, owner_principal_id, owner_org_id, repo_id) REFERENCES proxima_code.repos(owner_principal_kind, owner_principal_id, owner_org_id, repo_id) ON DELETE CASCADE;


--
-- Name: workspace_decision_v1 workspace_decision_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.workspace_decision_v1
    ADD CONSTRAINT workspace_decision_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: workspace_decision_v1 workspace_decision_v1_workspace_run_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.workspace_decision_v1
    ADD CONSTRAINT workspace_decision_v1_workspace_run_memory_id_fkey FOREIGN KEY (workspace_run_memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: workspace_review_v1 workspace_review_v1_execution_request_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.workspace_review_v1
    ADD CONSTRAINT workspace_review_v1_execution_request_memory_id_fkey FOREIGN KEY (execution_request_memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: workspace_review_v1 workspace_review_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.workspace_review_v1
    ADD CONSTRAINT workspace_review_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: workspace_review_v1 workspace_review_v1_workspace_run_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.workspace_review_v1
    ADD CONSTRAINT workspace_review_v1_workspace_run_memory_id_fkey FOREIGN KEY (workspace_run_memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: workspace_run_v1 workspace_run_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_code; Owner: -
--

ALTER TABLE ONLY proxima_code.workspace_run_v1
    ADD CONSTRAINT workspace_run_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
--
