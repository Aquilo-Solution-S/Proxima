-- Phase 1d: extend personality_wake_invocations with dispatch-time columns.
-- Spec line 645 calls for resolved_inference_target_ref + recipe_sha256 +
-- wake_token; failure_reason surfaces runtime/stress-test failures.
ALTER TABLE proxima_core.personality_wake_invocations
    ADD COLUMN IF NOT EXISTS wake_token uuid NULL;
ALTER TABLE proxima_core.personality_wake_invocations
    ADD COLUMN IF NOT EXISTS recipe_sha256 text NULL;
ALTER TABLE proxima_core.personality_wake_invocations
    ADD COLUMN IF NOT EXISTS resolved_inference_target_ref text NULL;
ALTER TABLE proxima_core.personality_wake_invocations
    ADD COLUMN IF NOT EXISTS failure_reason text NULL;
