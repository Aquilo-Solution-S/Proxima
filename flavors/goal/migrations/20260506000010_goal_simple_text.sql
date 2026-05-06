CREATE SCHEMA IF NOT EXISTS proxima_goal;

CREATE TABLE proxima_goal.simple_text_goal_v1 (
    goal_id uuid PRIMARY KEY REFERENCES proxima_core.goals(goal_id) ON DELETE CASCADE,
    text    text NOT NULL
);
