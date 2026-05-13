ALTER TABLE proxima_core.inference_targets
    DROP CONSTRAINT IF EXISTS inference_targets_kind_chk;

ALTER TABLE proxima_core.inference_targets
    ADD CONSTRAINT inference_targets_kind_chk
    CHECK (kind IN ('mistral_chat', 'openai_chat', 'openai_responses', 'chatgpt_codex'));
