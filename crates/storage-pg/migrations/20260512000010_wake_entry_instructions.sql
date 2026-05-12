-- Spec: docs/superpowers/specs/2026-05-12-proxima-harness-design.md
--       "Recipe lifecycle: kill the YAML".
--
-- User-authored per-trigger instruction body. The Goose path ignores
-- this column; the harness path reads it after the Phase 8 cut.
ALTER TABLE proxima_core.personality_wake_entries
    ADD COLUMN IF NOT EXISTS instructions text NOT NULL DEFAULT '';
