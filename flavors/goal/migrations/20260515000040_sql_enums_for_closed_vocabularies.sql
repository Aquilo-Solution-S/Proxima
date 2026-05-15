CREATE TYPE proxima_goal.task_priority AS ENUM ('Low', 'Medium', 'High');

ALTER TABLE IF EXISTS proxima_goal.task_goal_v1
    DROP CONSTRAINT IF EXISTS task_goal_v1_priority_check;

ALTER TABLE IF EXISTS proxima_goal.task_goal_v1
    ALTER COLUMN priority TYPE proxima_goal.task_priority
    USING priority::text::proxima_goal.task_priority;
