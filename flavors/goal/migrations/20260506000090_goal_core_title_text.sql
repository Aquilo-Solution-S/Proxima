ALTER TABLE proxima_goal.simple_text_goal_v1
    DROP COLUMN IF EXISTS text;

ALTER TABLE proxima_goal.task_goal_v1
    DROP COLUMN IF EXISTS title;
