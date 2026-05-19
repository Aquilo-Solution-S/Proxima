CREATE SCHEMA proxima_intent;

CREATE TYPE proxima_intent.vision_ambition_level AS ENUM (
    'Prototype',
    'Competent',
    'Production',
    'Exceptional'
);

CREATE TABLE proxima_intent.vision_brief_v1 (
    memory_id uuid NOT NULL,
    goal_id uuid NOT NULL,
    goal_activated_memory_id uuid NOT NULL,
    original_goal_text text NOT NULL,
    interpreted_outcome text NOT NULL,
    target_user text NOT NULL,
    use_case text NOT NULL,
    artifact_shape text NOT NULL,
    ambition_level proxima_intent.vision_ambition_level NOT NULL,
    quality_bar text NOT NULL,
    constraints text[] NOT NULL,
    assumptions text[] NOT NULL,
    open_questions text[] NOT NULL,
    acceptance_rubric text[] NOT NULL,
    demo_proof text NOT NULL,
    planner_directive text NOT NULL,
    CONSTRAINT vision_brief_v1_original_goal_text_nonempty CHECK (length(btrim(original_goal_text)) > 0),
    CONSTRAINT vision_brief_v1_interpreted_outcome_nonempty CHECK (length(btrim(interpreted_outcome)) > 0),
    CONSTRAINT vision_brief_v1_target_user_nonempty CHECK (length(btrim(target_user)) > 0),
    CONSTRAINT vision_brief_v1_use_case_nonempty CHECK (length(btrim(use_case)) > 0),
    CONSTRAINT vision_brief_v1_artifact_shape_nonempty CHECK (length(btrim(artifact_shape)) > 0),
    CONSTRAINT vision_brief_v1_quality_bar_nonempty CHECK (length(btrim(quality_bar)) > 0),
    CONSTRAINT vision_brief_v1_demo_proof_nonempty CHECK (length(btrim(demo_proof)) > 0),
    CONSTRAINT vision_brief_v1_planner_directive_nonempty CHECK (length(btrim(planner_directive)) > 0)
);

ALTER TABLE ONLY proxima_intent.vision_brief_v1
    ADD CONSTRAINT vision_brief_v1_pkey PRIMARY KEY (memory_id);

CREATE INDEX idx_vision_brief_v1_goal ON proxima_intent.vision_brief_v1 USING btree (goal_id);
CREATE INDEX idx_vision_brief_v1_goal_activated_memory ON proxima_intent.vision_brief_v1 USING btree (goal_activated_memory_id);

ALTER TABLE ONLY proxima_intent.vision_brief_v1
    ADD CONSTRAINT vision_brief_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);

ALTER TABLE ONLY proxima_intent.vision_brief_v1
    ADD CONSTRAINT vision_brief_v1_goal_id_fkey FOREIGN KEY (goal_id) REFERENCES proxima_core.goals(goal_id);

ALTER TABLE ONLY proxima_intent.vision_brief_v1
    ADD CONSTRAINT vision_brief_v1_goal_activated_memory_id_fkey FOREIGN KEY (goal_activated_memory_id) REFERENCES proxima_core.memories(memory_id);
