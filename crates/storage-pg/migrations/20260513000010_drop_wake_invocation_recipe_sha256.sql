-- Drop the recipe_sha256 column from personality_wake_invocations.
--
-- The harness cut removed Goose + recipe YAML, so no row written by the
-- new fire path carries a meaningful recipe hash. The column is no
-- longer read or written by the runtime; this migration removes it.

ALTER TABLE proxima_core.personality_wake_invocations
    DROP COLUMN IF EXISTS recipe_sha256;
