-- Phase 1c: system prompts move from per-personality compiled-in strings to
-- bundled Goose recipe YAMLs. The substrate sidecar no longer stores them.

ALTER TABLE proxima_core.root_personality_perspective_v1
    DROP CONSTRAINT IF EXISTS root_personality_perspective_system_prompt_chk;

ALTER TABLE proxima_core.root_personality_perspective_v1
    DROP COLUMN IF EXISTS system_prompt;
