CREATE TABLE proxima_goal.task_goal_v1 (
    goal_id  uuid PRIMARY KEY REFERENCES proxima_core.goals(goal_id) ON DELETE CASCADE,
    title    text NOT NULL,
    due_at   timestamptz,
    priority text CHECK (priority IN ('Low', 'Medium', 'High'))
);
