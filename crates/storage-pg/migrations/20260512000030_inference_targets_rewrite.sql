ALTER TABLE proxima_core.inference_targets
    DROP CONSTRAINT IF EXISTS inference_targets_kind_chk;

DELETE FROM proxima_core.inference_tier_bindings b
WHERE EXISTS (
    SELECT 1
    FROM proxima_core.inference_targets t
    WHERE t.owner_principal_kind = b.owner_principal_kind
      AND t.owner_principal_id = b.owner_principal_id
      AND t.owner_org_id = b.owner_org_id
      AND t.target_ref = b.target_ref
      AND (
          t.kind IN ('local_cli', 'remote_model')
          OR t.config->>'kind' IN ('local_cli', 'remote_model')
      )
);

DELETE FROM proxima_core.inference_targets
WHERE kind IN ('local_cli', 'remote_model')
   OR config->>'kind' IN ('local_cli', 'remote_model');

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM proxima_core.inference_targets
        WHERE kind NOT IN ('mistral_chat', 'openai_chat', 'openai_responses')
           OR kind IS DISTINCT FROM config->>'kind'
    ) THEN
        RAISE EXCEPTION 'inference_targets rewrite left kind/config mismatch or old discriminator values';
    END IF;
END
$$;

ALTER TABLE proxima_core.inference_targets
    ADD CONSTRAINT inference_targets_kind_chk
    CHECK (kind IN ('mistral_chat', 'openai_chat', 'openai_responses'));
