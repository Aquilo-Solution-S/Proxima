ALTER TABLE proxima_core.personality_wake_entries
    ADD COLUMN IF NOT EXISTS goal_scope text NOT NULL DEFAULT 'none';

ALTER TABLE proxima_core.personality_wake_entries
    DROP CONSTRAINT IF EXISTS personality_wake_entries_goal_scope_chk;

ALTER TABLE proxima_core.personality_wake_entries
    ADD CONSTRAINT personality_wake_entries_goal_scope_chk
        CHECK (goal_scope IN ('none', 'trigger_goal_assigned'));

UPDATE proxima_core.personality_wake_entries
SET goal_scope = 'trigger_goal_assigned',
    updated_at = now()
WHERE trigger_kind = 'on_memory'
  AND trigger_id = 'proxima-goal/goal-activated-v1'
  AND recipe_ref IN (
      'proxima-code/plan_execution_requests',
      'bundled:proxima-code/plan_execution_requests'
  );
