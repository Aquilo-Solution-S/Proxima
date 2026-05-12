# Task 7.5 — `persist_wake_trace` integration test (Postgres-backed)

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Create: `crates/storage-pg/tests/persist_wake_trace.rs`

- [ ] **Step 1: Write the test**

```rust
//! Integration test for the `persist_wake_trace` verb.
//! Requires the same test-Postgres harness the rest of `storage-pg`
//! uses (look at `crates/storage-pg/tests/event_ingest.rs` for the
//! existing setup — copy the harness verbatim).

use proxima_core::flavor::FlavorRegistry;
use proxima_core::verbs::persist_wake_trace::WakeTracePersistInput;
use proxima_core::wake::trace::WakeTracePayload;
use proxima_storage_pg::verbs::persist_wake_trace::persist_wake_trace_atomic;

mod common; // copy/adapt the test-harness module from event_ingest test

#[tokio::test]
async fn persist_writes_fact_jsonl_citation_sidecars_and_authored_edge() {
    let (pool, owner) = common::fresh_db().await;
    let registry = FlavorRegistry::default().freeze();

    let invocation_id = uuid::Uuid::now_v7();
    let personality_instance_id = uuid::Uuid::now_v7();
    let root_p = common::insert_test_perspective_memory(&pool, &owner).await;
    let trigger = common::insert_test_fact_memory(&pool, &owner).await;
    let jsonl: Vec<u8> = b"{\"record\":\"start\"}\n{\"record\":\"finish\"}\n".to_vec();
    let content_hash = *blake3::hash(&jsonl).as_bytes();

    let input = WakeTracePersistInput {
        owner: owner.clone(),
        authoring_personality_instance_id: personality_instance_id,
        root_perspective_memory_id: root_p,
        triggering_memory_id: trigger,
        active_goal_ids: vec![],
        jsonl_bytes: jsonl.clone(),
        jsonl_content_hash: content_hash,
        jsonl_line_count: 2,
        jsonl_truncated: false,
        citation_byte_range: None,
        wake_trace: WakeTracePayload {
            invocation_id,
            wake_entry_id: uuid::Uuid::now_v7(),
            personality_instance_id,
            model_target_ref: "mistral-default".into(),
            model_id: "mistral-medium-3.5".into(),
            started_at: time::OffsetDateTime::now_utc(),
            finished_at: time::OffsetDateTime::now_utc(),
            outcome_kind: "succeeded".into(),
            failure_reason: None,
            rounds_used: 3,
            finish_reason: Some("stop".into()),
            total_prompt_tokens: Some(2048),
            total_completion_tokens: Some(512),
            tool_call_count: 4,
            jsonl_truncated: false,
        },
        source_id: proxima_core::SourceId::new("test/wake-trace".into()),
        source_batch_id: proxima_core::SourceBatchId::new(uuid::Uuid::now_v7()),
        observed_at: time::OffsetDateTime::now_utc(),
        occurred_at: time::OffsetDateTime::now_utc(),
    };

    let outcome = persist_wake_trace_atomic(&pool, &registry, &input).await.expect("persist");

    assert!(!outcome.idempotent_replay);

    // 1. The memory row carries the authoring personality instance.
    let memory_row: (Option<uuid::Uuid>,) = sqlx::query_as(
        "SELECT personality_instance_id FROM proxima_core.memories WHERE memory_id = $1",
    )
    .bind(outcome.fact_memory_id.into_inner())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(memory_row.0, Some(personality_instance_id));

    // 2. wake_trace_v1 sidecar populated.
    let sidecar: (uuid::Uuid, String) = sqlx::query_as(
        "SELECT invocation_id, outcome_kind FROM proxima_core.wake_trace_v1 \
         WHERE memory_id = $1",
    )
    .bind(outcome.fact_memory_id.into_inner())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(sidecar.0, invocation_id);
    assert_eq!(sidecar.1, "succeeded");

    // 3. cited_wake_trace_jsonl_v1 sidecar holds the bytes.
    let jsonl_row: (Vec<u8>, i64) = sqlx::query_as(
        "SELECT body, byte_len FROM proxima_core.cited_wake_trace_jsonl_v1 \
         WHERE cited_object_id = $1",
    )
    .bind(outcome.cited_object_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(jsonl_row.0, jsonl);
    assert_eq!(jsonl_row.1 as usize, jsonl.len());

    // 4. citation_mappings row links Fact ↔ JSONL.
    let cm_row: (uuid::Uuid, uuid::Uuid) = sqlx::query_as(
        "SELECT memory_id, cited_object_id FROM proxima_core.citation_mappings \
         WHERE citation_mapping_id = $1",
    )
    .bind(outcome.citation_mapping_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cm_row.0, outcome.fact_memory_id.into_inner());
    assert_eq!(cm_row.1, outcome.cited_object_id);

    // 5. core/authored edge — Root P → wake-trace Fact.
    let authored: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM proxima_core.edges \
         WHERE relation = 'core/authored' \
           AND source_memory_id = $1 AND target_memory_id = $2",
    )
    .bind(root_p.into_inner())
    .bind(outcome.fact_memory_id.into_inner())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(authored.0, 1, "core/authored edge from Root P to Fact must exist");

    // 6. core/derived-from to triggering memory.
    let derived: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM proxima_core.edges \
         WHERE relation = 'core/derived-from' \
           AND source_memory_id = $1 AND target_memory_id = $2",
    )
    .bind(outcome.fact_memory_id.into_inner())
    .bind(trigger.into_inner())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(derived.0, 1);
}

#[tokio::test]
async fn active_goal_ids_emit_goal_kind_edges_targeting_goal_id() {
    // Goal-entity boundary regression: target_kind='Goal',
    // target_goal_id IS NOT NULL, target_memory_id IS NULL.
    let (pool, owner) = common::fresh_db().await;
    let registry = FlavorRegistry::default().freeze();
    let goal_a = common::insert_test_goal(&pool, &owner).await; // returns GoalId
    let goal_b = common::insert_test_goal(&pool, &owner).await;

    let mut input = common::sample_persist_input(&pool, &owner).await;
    input.active_goal_ids = vec![goal_a, goal_b];

    let outcome = persist_wake_trace_atomic(&pool, &registry, &input).await.unwrap();

    let goal_edges: Vec<(Option<uuid::Uuid>, Option<uuid::Uuid>, String)> = sqlx::query_as(
        "SELECT target_memory_id, target_goal_id, target_kind \
         FROM proxima_core.edges \
         WHERE relation = 'core/derived-from' \
           AND source_memory_id = $1 \
           AND target_kind = 'Goal' \
         ORDER BY target_goal_id",
    )
    .bind(outcome.fact_memory_id.into_inner())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(goal_edges.len(), 2);
    for (tm, tg, kind) in &goal_edges {
        assert_eq!(kind, "Goal");
        assert!(tm.is_none(), "target_memory_id must be NULL for Goal edges");
        assert!(tg.is_some(), "target_goal_id must be set for Goal edges");
    }
}

#[tokio::test]
async fn idempotent_replay_returns_same_ids() {
    let (pool, owner) = common::fresh_db().await;
    let registry = FlavorRegistry::default().freeze();
    let input = common::sample_persist_input(&pool, &owner).await;

    let first = persist_wake_trace_atomic(&pool, &registry, &input).await.unwrap();
    let second = persist_wake_trace_atomic(&pool, &registry, &input).await.unwrap();

    assert!(!first.idempotent_replay);
    assert!(second.idempotent_replay);
    assert_eq!(first.fact_memory_id, second.fact_memory_id);
    assert_eq!(first.cited_object_id, second.cited_object_id);
    assert_eq!(first.citation_mapping_id, second.citation_mapping_id);

    // Exactly one row in each table.
    let n_facts: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM proxima_core.wake_trace_v1 WHERE invocation_id = $1",
    )
    .bind(input.wake_trace.invocation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n_facts.0, 1);
}
```

```rust
#[tokio::test]
async fn distinct_invocations_with_identical_jsonl_do_not_collapse() {
    // Idempotency contract regression: whole-verb replay keys on
    // event_id (includes invocation_id). Two *distinct* wake
    // invocations producing byte-identical JSONL must yield two
    // wake-trace Facts; the cited_objects row, however, is shared via
    // the (owner, schema_id, content_hash) UNIQUE constraint.
    let (pool, owner) = common::fresh_db().await;
    let registry = FlavorRegistry::default().freeze();

    let mut input_a = common::sample_persist_input(&pool, &owner).await;
    let mut input_b = input_a.clone();
    // Same JSONL bytes → same content_hash.
    assert_eq!(input_a.jsonl_content_hash, input_b.jsonl_content_hash);
    // Different invocations.
    input_b.wake_trace.invocation_id = uuid::Uuid::now_v7();

    let a = persist_wake_trace_atomic(&pool, &registry, &input_a).await.unwrap();
    let b = persist_wake_trace_atomic(&pool, &registry, &input_b).await.unwrap();

    assert!(!a.idempotent_replay);
    assert!(!b.idempotent_replay, "different invocations must NOT short-circuit");
    assert_ne!(a.fact_memory_id, b.fact_memory_id, "two Facts");
    assert_ne!(a.citation_mapping_id, b.citation_mapping_id, "two mappings");
    assert_eq!(a.cited_object_id, b.cited_object_id, "shared CitedObject row");

    let n_facts: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM proxima_core.wake_trace_v1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n_facts.0, 2);

    let n_cited: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM proxima_core.cited_objects",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n_cited.0, 1);
}
```

`common::fresh_db`, `common::insert_test_perspective_memory`, `common::insert_test_fact_memory`, `common::insert_test_goal`, and `common::sample_persist_input` follow the existing pattern in `crates/storage-pg/tests/`. Read the equivalent `tests/event_ingest.rs` (or whichever Postgres-backed test exists today) and copy-adapt — do **not** invent your own harness. `insert_test_goal` should return a `GoalId` and insert a row into `proxima_core.goals` with the owner's principal/org columns.

- [ ] **Step 2: Run**

Run: `cargo test -p proxima-storage-pg --test persist_wake_trace`
Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/storage-pg/tests/persist_wake_trace.rs
git commit -m "storage(persist_wake_trace): integration tests for atomicity, replay, goal-entity edges"
```
