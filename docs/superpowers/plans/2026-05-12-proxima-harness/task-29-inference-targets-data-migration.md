# Task 8.2 — Data migration: split existing rows by provider adapter

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Create: `crates/storage-pg/migrations/20260512000030_inference_targets_rewrite.sql`

- [ ] **Step 1: Write the migration**

```sql
-- Spec §"InferenceTargetConfig migration".
-- Translates every existing inference_targets row to adapter-selector
-- variants; unmappable rows ABORT the migration. Updates BOTH the
-- discriminator column and the JSON config. Leaving the column as
-- local_cli/remote_model breaks idempotency and rejects new writes
-- under the old CHECK constraint.

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
            RAISE EXCEPTION 'inference_targets row % uses LocalCli; hand-map to a MistralChat/OpenAIChat/OpenAIResponses target before re-running the cut', r.target_ref;
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
```

The `openai_chat` / `openai_responses` spellings are fixed by explicit serde renames in Task 8.1. The unit test below guards those public wire strings before the SQL migration relies on them.

The `kind` column is load-bearing. `crates/storage-pg/src/settings/inference_targets.rs` derives this column from `InferenceTargetConfig`, so Task 8.1 must update that mapping to return `mistral_chat`, `openai_chat`, or `openai_responses`. This migration must keep `inference_targets.kind == config->>'kind'` for every row and must replace the old `inference_targets_kind_chk` constraint in the same migration.

- [ ] **Step 2: Migration-shape test**

Create `crates/core/tests/inference_target_migration.rs`:

```rust
use proxima_core::inference::{
    InferenceTargetConfig, MistralChatConfig, OpenAIChatConfig, OpenAIResponsesConfig,
};

#[test]
fn mistral_chat_variant_serializes_as_kind_mistral_chat() {
    let c = InferenceTargetConfig::MistralChat(MistralChatConfig {
        base_url: "https://api.mistral.ai".into(),
        model_id: "m".into(),
        api_key_env: "K".into(),
        temperature: None,
        max_completion_tokens: None,
    });
    let v = serde_json::to_value(&c).unwrap();
    assert_eq!(v["kind"], "mistral_chat");
}

#[test]
fn openai_chat_kind_string() {
    let c = InferenceTargetConfig::OpenAIChat(OpenAIChatConfig {
        base_url: "x".into(), model_id: "m".into(), api_key_env: "K".into(),
        temperature: None, max_completion_tokens: None,
    });
    let v = serde_json::to_value(&c).unwrap();
    assert_eq!(v["kind"], "openai_chat");
}

#[test]
fn openai_responses_kind_string() {
    let c = InferenceTargetConfig::OpenAIResponses(OpenAIResponsesConfig {
        base_url: "x".into(), model_id: "m".into(), api_key_env: "K".into(),
        reasoning_effort: None,
    });
    let v = serde_json::to_value(&c).unwrap();
    assert_eq!(v["kind"], "openai_responses");
}
```

Run this test **before** finalising the migration SQL. Do not change the SQL to acronym-inferred strings; fix the enum serde renames if the test fails.

- [ ] **Step 3: Storage regression test**

Extend the migration/storage test to assert:

```sql
SELECT kind, config->>'kind'
FROM proxima_core.inference_targets
ORDER BY target_ref;
```

Every returned row must have `kind = config->>'kind'`, and every `kind` must be one of `mistral_chat`, `openai_chat`, `openai_responses`.

Also verify the replacement CHECK rejects an old discriminator:

```sql
INSERT INTO proxima_core.inference_targets (
    owner_principal_kind, owner_principal_id, owner_org_id,
    target_ref, kind, config
) VALUES (
    'User',
    '00000000-0000-7000-8000-000000000001'::uuid,
    '00000000-0000-7000-8000-000000000002'::uuid,
    'bad-old-kind', 'remote_model', '{"kind":"remote_model"}'::jsonb
);
```

Expected: CHECK violation on `inference_targets_kind_chk`.
