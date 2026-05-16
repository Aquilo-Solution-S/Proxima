-- Baseline migration for the proxima_goal schema. Generated with
-- `pg_dump --schema-only --no-owner --no-privileges --no-comments -n proxima_goal`
-- and sanitized (psql session directives stripped).
-- Squashed from pre-2026-05-16 migration history; do not edit by hand.

CREATE SCHEMA proxima_goal;


--
-- Name: task_priority; Type: TYPE; Schema: proxima_goal; Owner: -
--

CREATE TYPE proxima_goal.task_priority AS ENUM (
    'Low',
    'Medium',
    'High'
);




--
-- Name: goal_achieved_v1; Type: TABLE; Schema: proxima_goal; Owner: -
--

CREATE TABLE proxima_goal.goal_achieved_v1 (
    memory_id uuid NOT NULL,
    goal_id uuid NOT NULL,
    schema_id text NOT NULL,
    title text NOT NULL,
    achieved_at timestamp with time zone NOT NULL,
    evidence_count integer NOT NULL,
    CONSTRAINT goal_achieved_v1_evidence_count_chk CHECK ((evidence_count >= 0)),
    CONSTRAINT goal_achieved_v1_title_nonempty CHECK ((length(btrim(title)) > 0))
);


--
-- Name: goal_activated_v1; Type: TABLE; Schema: proxima_goal; Owner: -
--

CREATE TABLE proxima_goal.goal_activated_v1 (
    memory_id uuid NOT NULL,
    goal_id uuid NOT NULL,
    schema_id text NOT NULL,
    title text NOT NULL,
    accepted_at timestamp with time zone NOT NULL,
    evidence_count integer NOT NULL,
    CONSTRAINT goal_activated_v1_evidence_count_chk CHECK ((evidence_count >= 0)),
    CONSTRAINT goal_activated_v1_title_nonempty CHECK ((length(btrim(title)) > 0))
);


--
-- Name: goal_proposed_v1; Type: TABLE; Schema: proxima_goal; Owner: -
--

CREATE TABLE proxima_goal.goal_proposed_v1 (
    memory_id uuid NOT NULL,
    goal_id uuid NOT NULL,
    schema_id text NOT NULL,
    title text NOT NULL,
    CONSTRAINT goal_proposed_v1_title_nonempty CHECK ((length(btrim(title)) > 0))
);


--
-- Name: simple_text_goal_v1; Type: TABLE; Schema: proxima_goal; Owner: -
--

CREATE TABLE proxima_goal.simple_text_goal_v1 (
    goal_id uuid NOT NULL
);


--
-- Name: task_goal_v1; Type: TABLE; Schema: proxima_goal; Owner: -
--

CREATE TABLE proxima_goal.task_goal_v1 (
    goal_id uuid NOT NULL,
    due_at timestamp with time zone,
    priority proxima_goal.task_priority
);


--
-- Name: goal_achieved_v1 goal_achieved_v1_pkey; Type: CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.goal_achieved_v1
    ADD CONSTRAINT goal_achieved_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: goal_activated_v1 goal_activated_v1_pkey; Type: CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.goal_activated_v1
    ADD CONSTRAINT goal_activated_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: goal_proposed_v1 goal_proposed_v1_pkey; Type: CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.goal_proposed_v1
    ADD CONSTRAINT goal_proposed_v1_pkey PRIMARY KEY (memory_id);


--
-- Name: simple_text_goal_v1 simple_text_goal_v1_pkey; Type: CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.simple_text_goal_v1
    ADD CONSTRAINT simple_text_goal_v1_pkey PRIMARY KEY (goal_id);


--
-- Name: task_goal_v1 task_goal_v1_pkey; Type: CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.task_goal_v1
    ADD CONSTRAINT task_goal_v1_pkey PRIMARY KEY (goal_id);


--
-- Name: idx_goal_achieved_v1_goal; Type: INDEX; Schema: proxima_goal; Owner: -
--

CREATE INDEX idx_goal_achieved_v1_goal ON proxima_goal.goal_achieved_v1 USING btree (goal_id);


--
-- Name: idx_goal_activated_v1_goal; Type: INDEX; Schema: proxima_goal; Owner: -
--

CREATE INDEX idx_goal_activated_v1_goal ON proxima_goal.goal_activated_v1 USING btree (goal_id);


--
-- Name: idx_goal_proposed_v1_goal; Type: INDEX; Schema: proxima_goal; Owner: -
--

CREATE INDEX idx_goal_proposed_v1_goal ON proxima_goal.goal_proposed_v1 USING btree (goal_id);


--
-- Name: goal_achieved_v1 goal_achieved_v1_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.goal_achieved_v1
    ADD CONSTRAINT goal_achieved_v1_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id);


--
-- Name: goal_achieved_v1 goal_achieved_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.goal_achieved_v1
    ADD CONSTRAINT goal_achieved_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: goal_activated_v1 goal_activated_v1_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.goal_activated_v1
    ADD CONSTRAINT goal_activated_v1_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id);


--
-- Name: goal_activated_v1 goal_activated_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.goal_activated_v1
    ADD CONSTRAINT goal_activated_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: goal_proposed_v1 goal_proposed_v1_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.goal_proposed_v1
    ADD CONSTRAINT goal_proposed_v1_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id);


--
-- Name: goal_proposed_v1 goal_proposed_v1_memory_id_fkey; Type: FK CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.goal_proposed_v1
    ADD CONSTRAINT goal_proposed_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);


--
-- Name: simple_text_goal_v1 simple_text_goal_v1_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.simple_text_goal_v1
    ADD CONSTRAINT simple_text_goal_v1_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id) ON DELETE CASCADE;


--
-- Name: task_goal_v1 task_goal_v1_goal_id_fkey; Type: FK CONSTRAINT; Schema: proxima_goal; Owner: -
--

ALTER TABLE ONLY proxima_goal.task_goal_v1
    ADD CONSTRAINT task_goal_v1_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id) ON DELETE CASCADE;


--
--
