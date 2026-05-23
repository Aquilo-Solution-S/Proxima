-- Drop the workspace-mode substrate concept.
--
-- v1 cleanup: the autonomous "workspace runner" execution path (a wake
-- targets a flavor runner that prepares a sandboxed clone, then the harness
-- drives an LLM tool loop with bash/text-editor/list-files tools against
-- it) is being shelved. The supporting Rust code is gone; this migration
-- drops the matching schema artefacts so dev DBs match.

-- Constraint must drop before the columns it references.
ALTER TABLE proxima_core.personality_wake_entries
    DROP CONSTRAINT IF EXISTS personality_wake_entries_workspace_binding_mode_chk;

-- Any in-flight workspace-mode wake entries are unusable now. Drop their
-- rows so the enum cast below succeeds.
DELETE FROM proxima_core.personality_wake_entries
    WHERE execution_mode = 'workspace';

-- Drop the per-wake workspace-run Fact sidecar (added in
-- 20260520000010_core_wake_workspace_binding.sql, extended by
-- 20260522000010_core_workspace_sandbox.sql).
DROP TABLE IF EXISTS proxima_core.workspace_run_v1;

ALTER TABLE proxima_core.personality_wake_entries
    DROP COLUMN IF EXISTS workspace_tool_palette,
    DROP COLUMN IF EXISTS workspace_binding;

-- Postgres has no `ALTER TYPE ... DROP VALUE`. Rebuild the enum without
-- the 'workspace' variant.
CREATE TYPE proxima_core.wake_execution_mode_new AS ENUM (
    'substrate_only'
);

ALTER TABLE proxima_core.personality_wake_entries
    ALTER COLUMN execution_mode DROP DEFAULT,
    ALTER COLUMN execution_mode TYPE proxima_core.wake_execution_mode_new
        USING execution_mode::text::proxima_core.wake_execution_mode_new,
    ALTER COLUMN execution_mode SET DEFAULT 'substrate_only'::proxima_core.wake_execution_mode_new;

DROP TYPE proxima_core.wake_execution_mode;
ALTER TYPE proxima_core.wake_execution_mode_new
    RENAME TO wake_execution_mode;
