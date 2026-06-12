-- Baseline migration for the proxima_agent_memory schema. Generated with
-- `pg_dump --schema-only --no-owner --no-privileges --no-comments -n proxima_agent_memory`
-- and sanitized (psql session directives stripped).
-- Squashed from pre-2026-05-16 migration history; do not edit by hand.

CREATE SCHEMA proxima_agent_memory;




--
-- Name: agent_derivation_v1; Type: TABLE; Schema: proxima_agent_memory; Owner: -
--

CREATE TABLE proxima_agent_memory.agent_derivation_v1 (
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


--
-- Name: agent_link_v1; Type: TABLE; Schema: proxima_agent_memory; Owner: -
--

CREATE TABLE proxima_agent_memory.agent_link_v1 (
    edge_id uuid NOT NULL,
    reason text NOT NULL,
    confidence smallint NOT NULL,
    CONSTRAINT agent_link_v1_confidence_chk CHECK (((confidence >= 0) AND (confidence <= 100))),
    CONSTRAINT agent_link_v1_reason_nonempty CHECK ((length(btrim(reason)) > 0))
);


--
-- Name: agent_note_v1; Type: TABLE; Schema: proxima_agent_memory; Owner: -
--

CREATE TABLE proxima_agent_memory.agent_note_v1 (
    memory_id uuid NOT NULL,
    note_id uuid NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    tags text[] NOT NULL,
    idempotency_key text,
    CONSTRAINT agent_note_v1_body_nonempty CHECK ((length(btrim(body)) > 0)),
    CONSTRAINT agent_note_v1_title_nonempty CHECK ((length(btrim(title)) > 0))
);


--
-- Name: agent_derivation_v1 agent_derivation_v1_pkey; Type: CONSTRAINT; Schema: proxima_agent_memory; Owner: -
--

ALTER TABLE ONLY proxima_agent_memory.agent_derivation_v1
    ADD CONSTRAINT agent_derivation_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: agent_link_v1 agent_link_v1_pkey; Type: CONSTRAINT; Schema: proxima_agent_memory; Owner: -
--

ALTER TABLE ONLY proxima_agent_memory.agent_link_v1
    ADD CONSTRAINT agent_link_v1_pkey PRIMARY KEY (edge_id);


--
-- Name: agent_note_v1 agent_note_v1_pkey; Type: CONSTRAINT; Schema: proxima_agent_memory; Owner: -
--

ALTER TABLE ONLY proxima_agent_memory.agent_note_v1
    ADD CONSTRAINT agent_note_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: idx_agent_derivation_v1_search; Type: INDEX; Schema: proxima_agent_memory; Owner: -
--

CREATE INDEX idx_agent_derivation_v1_search ON proxima_agent_memory.agent_derivation_v1 USING gin (to_tsvector('simple'::regconfig, ((title || ' '::text) || body)));


--
-- Name: idx_agent_note_v1_note_id; Type: INDEX; Schema: proxima_agent_memory; Owner: -
--

CREATE INDEX idx_agent_note_v1_note_id ON proxima_agent_memory.agent_note_v1 USING btree (note_id);


--
-- Name: idx_agent_note_v1_search; Type: INDEX; Schema: proxima_agent_memory; Owner: -
--

CREATE INDEX idx_agent_note_v1_search ON proxima_agent_memory.agent_note_v1 USING gin (to_tsvector('simple'::regconfig, ((title || ' '::text) || body)));


--
-- Name: agent_derivation_v1 agent_derivation_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_agent_memory; Owner: -
--

ALTER TABLE ONLY proxima_agent_memory.agent_derivation_v1
    ADD CONSTRAINT agent_derivation_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: agent_link_v1 agent_link_v1_edge_id_fkey; Type: FK CONSTRAINT; Schema: proxima_agent_memory; Owner: -
--

ALTER TABLE ONLY proxima_agent_memory.agent_link_v1
    ADD CONSTRAINT agent_link_v1_edge_id_fkey FOREIGN KEY (edge_id) REFERENCES proxima_core.edges(edge_id);


--
-- Name: agent_note_v1 agent_note_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_agent_memory; Owner: -
--

ALTER TABLE ONLY proxima_agent_memory.agent_note_v1
    ADD CONSTRAINT agent_note_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
--
