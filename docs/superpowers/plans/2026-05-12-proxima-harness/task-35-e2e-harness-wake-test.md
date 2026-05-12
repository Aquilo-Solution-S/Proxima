# Task 8.8 — End-to-end harness wake test

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Create: `crates/harness/tests/end_to_end_wake.rs`

- [ ] **Step 1: Write the test**

The test spins up a Postgres instance (use the existing test-harness pattern in `crates/storage-pg/tests/` — look for `sqlx::PgPool::connect` against `DATABASE_URL` or a per-test tempdir Postgres), seeds an Engineer personality with a MistralChat inference target pointing at the in-process mock from `mistral_chat_replay.rs`, fires one wake against a synthetic ChangeEvent, asserts:

1. `wake_invocations` row finalised with status `succeeded`.
2. `wake-trace-v1` Fact memory exists with `outcome_kind = "succeeded"` (lowercase — the `HarnessOutcome.kind.as_str()` mapping returns lowercase strings).
3. The Fact memory row carries `personality_instance_id = Engineer_instance_id` (not the external/nil uuid that `EventIngest` would have stamped — this is the load-bearing check that proves `persist_wake_trace` ran, not `event_ingest`).
4. `cited_wake_trace_jsonl_v1.body` is non-empty and the BLAKE3 hash matches the `cited_objects.content_hash` for the same `cited_object_id`.
5. `citation_wake_trace_v1` row exists keyed on the trace Fact's `citation_mapping_id`.
6. `proxima_core.edges` has exactly one row with `relation = 'core/authored'`, `source_memory_id = Engineer Root Perspective memory id`, `target_memory_id = wake-trace Fact memory id`.
7. `proxima_core.edges` has at least one `core/derived-from` row with `source_memory_id = wake-trace Fact memory id` and `target_memory_id = triggering ChangeEvent's memory id`.
8. If `active_goal_ids` is non-empty at wake time, every Goal-target edge satisfies `target_kind = 'Goal'`, `target_goal_id IS NOT NULL`, `target_memory_id IS NULL` (Goal-entity boundary regression).

Sketch:

```rust
use std::sync::Arc;
use proxima_core::Engine;
use proxima_harness::HarnessLoop;

#[tokio::test(flavor = "multi_thread")]
async fn engineer_wake_emits_succeeded_trace_fact_with_authored_edge() {
    let (pool, owner, engineer_instance_id, engineer_root_p_memory_id) =
        test_db_with_seeded_engineer().await;
    let engine = Arc::new(Engine::new(pool.clone()).await.unwrap());
    let mock_url = spawn_mistral_chat_mock_returning_stop().await;
    register_mistral_chat_inference_target(&engine, &owner, mock_url).await;

    // HarnessLoop needs a HarnessSubstrateBridge — DevMcpServer implements
    // it (Task 4.2). Build the dev MCP server alongside the engine
    // (its existing helper takes the same registry the engine froze).
    let dev_mcp = Arc::new(
        proxima_mcp_server::DevMcpServer::from_pool(
            pool.clone(),
            owner.clone(),
            engine.registry().clone().into(),
        )
        .with_engine(engine.clone()),
    );
    let adapter = Arc::new(HarnessLoop::new(
        engine.clone(),
        dev_mcp.clone() as Arc<dyn proxima_core::mcp::HarnessSubstrateBridge>,
    ));
    engine.set_harness_adapter(adapter.clone()).await;

    let (seq, triggering_memory_id) = ingest_commit_change_event(&engine, &owner).await;
    let fired = engine.fire_due_wakes(&owner, seq).await.unwrap();
    assert!(fired >= 1);

    // 1. Invocation finalised.
    let inv: (String, uuid::Uuid) = sqlx::query_as(
        "SELECT status, wake_invocation_id FROM proxima_core.wake_invocations \
         WHERE change_event_seq = $1",
    )
    .bind(seq)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(inv.0, "succeeded");
    let invocation_id = inv.1;

    // 2. wake-trace Fact memory exists, lowercase outcome kind.
    let trace: (uuid::Uuid, String, bool) = sqlx::query_as(
        "SELECT memory_id, outcome_kind, jsonl_truncated FROM proxima_core.wake_trace_v1 \
         WHERE invocation_id = $1",
    )
    .bind(invocation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let fact_memory_id = trace.0;
    assert_eq!(trace.1, "succeeded");
    assert!(!trace.2);

    // 3. Fact memory authored by the Engineer instance — NOT nil.
    let pi: (Option<uuid::Uuid>,) = sqlx::query_as(
        "SELECT personality_instance_id FROM proxima_core.memories WHERE memory_id = $1",
    )
    .bind(fact_memory_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pi.0, Some(engineer_instance_id),
        "wake-trace Fact must be authored by the Engineer instance, not external/nil");

    // 4. JSONL CitedObject body non-empty + hash consistent.
    let jsonl: (Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT cwj.body, co.content_hash \
         FROM proxima_core.cited_wake_trace_jsonl_v1 cwj \
         JOIN proxima_core.cited_objects co USING (cited_object_id) \
         JOIN proxima_core.citation_mappings cm ON cm.cited_object_id = co.cited_object_id \
         JOIN proxima_core.memories m ON m.citation_mapping_id = cm.citation_mapping_id \
         WHERE m.memory_id = $1",
    )
    .bind(fact_memory_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!jsonl.0.is_empty());
    assert_eq!(jsonl.1, blake3::hash(&jsonl.0).as_bytes().as_slice());

    // 5. citation_wake_trace_v1 row exists.
    let cm_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM proxima_core.citation_wake_trace_v1 cwt \
         JOIN proxima_core.citation_mappings cm USING (citation_mapping_id) \
         JOIN proxima_core.memories m ON m.citation_mapping_id = cm.citation_mapping_id \
         WHERE m.memory_id = $1",
    )
    .bind(fact_memory_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cm_count.0, 1);

    // 6. core/authored edge Root P → Fact.
    let authored: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM proxima_core.edges \
         WHERE relation = 'core/authored' \
           AND source_memory_id = $1 \
           AND target_memory_id = $2",
    )
    .bind(engineer_root_p_memory_id)
    .bind(fact_memory_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(authored.0, 1,
        "Root P → wake-trace Fact must have exactly one core/authored edge");

    // 7. core/derived-from Fact → triggering memory.
    let derived: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM proxima_core.edges \
         WHERE relation = 'core/derived-from' \
           AND source_memory_id = $1 \
           AND target_memory_id = $2",
    )
    .bind(fact_memory_id)
    .bind(triggering_memory_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(derived.0 >= 1);
}
```

Fill in `test_db_with_seeded_engineer`, `spawn_mistral_chat_mock_returning_stop`, `register_mistral_chat_inference_target`, and `ingest_commit_change_event` using existing helpers (look in `flavors/code/tests/` and `crates/core/tests/` for patterns — the existing `target_adapter_local_cli.rs` test will be deleted but its setup helpers should be ported.)

Note: assertions 3, 6, and 7 are the load-bearing checks that distinguish "the dedicated `persist_wake_trace` verb actually ran" from "we accidentally fell back to `EventIngest`." Do not soften them.

- [ ] **Step 2: Missing-credentials regression**

Add a second test in the same file. Seed the same Engineer wake entry and a `MistralChat` inference target whose `api_key_env` points at a guaranteed-missing env var, e.g. `PROXIMA_TEST_MISSING_MISTRAL_KEY`. Do not start a provider mock — the harness must fail before any HTTP call.

Assertions:

1. `fire_due_wakes` returns successfully with at least one fired wake.
2. The `wake_invocations` row is finalized with status `failed`.
3. The failure reason is exactly `credentials_missing:PROXIMA_TEST_MISSING_MISTRAL_KEY`.
4. `finished_at IS NOT NULL` (or the repo's equivalent finalized timestamp column is set).
5. The wake token minted for the invocation is revoked or absent from the live wake-token store.
6. `wake_trace_v1` exists for the invocation with `outcome_kind = "failed"` and the same failure reason.
7. There is no stuck `started` / open invocation row for the same `wake_entry_id` + `change_event_seq`.

Sketch:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn missing_provider_credentials_finalize_failed_wake_and_revoke_token() {
    std::env::remove_var("PROXIMA_TEST_MISSING_MISTRAL_KEY");

    let (pool, owner, _engineer_instance_id, _engineer_root_p_memory_id) =
        test_db_with_seeded_engineer().await;
    let engine = Arc::new(Engine::new(pool.clone()).await.unwrap());
    register_mistral_chat_inference_target_with_api_key_env(
        &engine,
        &owner,
        "http://127.0.0.1:9",
        "PROXIMA_TEST_MISSING_MISTRAL_KEY",
    )
    .await;

    let dev_mcp = Arc::new(/* same DevMcpServer setup as first test */);
    engine
        .set_harness_adapter(Arc::new(HarnessLoop::new(
            engine.clone(),
            dev_mcp.clone() as Arc<dyn proxima_core::mcp::HarnessSubstrateBridge>,
        )))
        .await;

    let (seq, _triggering_memory_id) = ingest_commit_change_event(&engine, &owner).await;
    let fired = engine.fire_due_wakes(&owner, seq).await.unwrap();
    assert!(fired >= 1);

    let inv: (String, Option<String>, Option<time::OffsetDateTime>, uuid::Uuid) =
        sqlx::query_as(
            "SELECT status, failure_reason, finished_at, wake_invocation_id \
             FROM proxima_core.wake_invocations \
             WHERE change_event_seq = $1",
        )
        .bind(seq)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(inv.0, "failed");
    assert_eq!(
        inv.1.as_deref(),
        Some("credentials_missing:PROXIMA_TEST_MISSING_MISTRAL_KEY")
    );
    assert!(inv.2.is_some(), "missing credentials must finalize the invocation");
    assert!(
        !engine.wake_token_store().contains_invocation(inv.3).await,
        "wake token must be revoked on missing credentials"
    );

    let trace: (String, Option<String>) = sqlx::query_as(
        "SELECT outcome_kind, failure_reason \
         FROM proxima_core.wake_trace_v1 \
         WHERE invocation_id = $1",
    )
    .bind(inv.3)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(trace.0, "failed");
    assert_eq!(
        trace.1.as_deref(),
        Some("credentials_missing:PROXIMA_TEST_MISSING_MISTRAL_KEY")
    );

    let open: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM proxima_core.wake_invocations \
         WHERE change_event_seq = $1 AND finished_at IS NULL",
    )
    .bind(seq)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(open.0, 0);
}
```

If `WakeTokenStore` has no `contains_invocation` helper, add a test-only query/helper rather than weakening the token-revocation assertion. This is the regression for Task 8.5's no-early-`?` rule.
