CREATE TABLE proxima_goal.goal_proposed_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    goal_id   uuid NOT NULL REFERENCES proxima_core.goals(goal_id),
    schema_id text NOT NULL,
    title     text NOT NULL,
    CONSTRAINT goal_proposed_v1_title_nonempty CHECK (length(btrim(title)) > 0)
);
CREATE INDEX idx_goal_proposed_v1_goal
    ON proxima_goal.goal_proposed_v1 (goal_id);

CREATE TABLE proxima_goal.goal_activated_v1 (
    memory_id      uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    goal_id        uuid NOT NULL REFERENCES proxima_core.goals(goal_id),
    schema_id      text NOT NULL,
    title          text NOT NULL,
    accepted_at    timestamptz NOT NULL,
    evidence_count integer NOT NULL,
    CONSTRAINT goal_activated_v1_title_nonempty CHECK (length(btrim(title)) > 0),
    CONSTRAINT goal_activated_v1_evidence_count_chk CHECK (evidence_count >= 0)
);
CREATE INDEX idx_goal_activated_v1_goal
    ON proxima_goal.goal_activated_v1 (goal_id);
