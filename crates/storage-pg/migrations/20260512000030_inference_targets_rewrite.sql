ALTER TABLE proxima_core.inference_targets
    DROP CONSTRAINT IF EXISTS inference_targets_kind_chk;

DO $$
DECLARE
    r RECORD;
    new_kind text;
    new_config jsonb;
BEGIN
    FOR r IN SELECT owner_principal_kind, owner_principal_id, owner_org_id,
                    target_ref, kind, config
             FROM proxima_core.inference_targets
    LOOP
        IF r.kind IS DISTINCT FROM r.config->>'kind' THEN
            RAISE EXCEPTION
              'inference_targets row % has mismatched kind column % and config.kind %; hand-fix before re-running the cut',
              r.target_ref, r.kind, r.config->>'kind';
        END IF;

        IF r.kind = 'local_cli' THEN
            RAISE EXCEPTION 'inference_targets row % uses local_cli; hand-map to a native chat target before re-running the cut', r.target_ref;
        ELSIF r.kind = 'remote_model' THEN
            IF r.config->>'vendor' = 'mistral' THEN
                new_kind := 'mistral_chat';
                new_config := jsonb_build_object(
                    'kind', new_kind,
                    'base_url', COALESCE(r.config->>'base_url','https://api.mistral.ai'),
                    'model_id', r.config->>'model_id',
                    'api_key_env', COALESCE(r.config->>'api_key_env','MISTRAL_API_KEY'),
                    'temperature', null::jsonb,
                    'max_completion_tokens', null::jsonb
                );
            ELSIF r.config->>'vendor' = 'openai' AND r.config->>'dialect' = 'chat' THEN
                new_kind := 'openai_chat';
                new_config := jsonb_build_object(
                    'kind', new_kind,
                    'base_url', COALESCE(r.config->>'base_url','https://api.openai.com'),
                    'model_id', r.config->>'model_id',
                    'api_key_env', COALESCE(r.config->>'api_key_env','OPENAI_API_KEY'),
                    'temperature', null::jsonb,
                    'max_completion_tokens', null::jsonb
                );
            ELSIF r.config->>'vendor' = 'openai' AND r.config->>'dialect' = 'responses' THEN
                new_kind := 'openai_responses';
                new_config := jsonb_build_object(
                    'kind', new_kind,
                    'base_url', COALESCE(r.config->>'base_url','https://api.openai.com'),
                    'model_id', r.config->>'model_id',
                    'api_key_env', COALESCE(r.config->>'api_key_env','OPENAI_API_KEY'),
                    'reasoning_effort', null::jsonb
                );
            ELSE
                RAISE EXCEPTION
                  'inference_targets row % has unmappable vendor=% dialect=%; hand-fix before re-running the cut',
                  r.target_ref, r.config->>'vendor', r.config->>'dialect';
            END IF;

            UPDATE proxima_core.inference_targets
            SET kind = new_kind,
                config = new_config,
                updated_at = now()
            WHERE owner_principal_kind = r.owner_principal_kind
              AND owner_principal_id   = r.owner_principal_id
              AND owner_org_id         = r.owner_org_id
              AND target_ref           = r.target_ref;
        ELSE
            RAISE EXCEPTION
              'inference_targets row % has unmappable kind=%; hand-fix before re-running the cut',
              r.target_ref, r.kind;
        END IF;
    END LOOP;
END
$$;

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
