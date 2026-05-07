//! Three-layer authorization. The dispatcher must reject:
//!  1. tools outside the personality's palette (palette = substrate +
//!     flavor tools);
//!  2. emit_perspective with a schema_id outside `writeable_schemas`;
//!  3. create_edge with `core/derived-from` or `core/supersedes`
//!     (substrate-only) or any relation outside `writeable_relations`.
//!
//! Each violation is fed back to the agent loop as a `tool_result {
//! is_error: true }` block. The agent ends its turn afterwards and the
//! wake completes — no unauthorized memory or edge is written.

mod common;

use std::sync::Arc;

use common::personality::{
    apply_test_schemas, build_test_engine, ingest_test_fact, instantiate_test_personality,
    TestPersonality, TEST_PERSONALITY_TYPE_ID, TEST_PERSPECTIVE_SCHEMA,
};
use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::llm::scripted::{ScriptedAnthropicClient, ScriptedTurn};
use proxima_core::CORE_DERIVED_FROM_RELATION;

#[tokio::test(flavor = "multi_thread")]
async fn rejects_tool_outside_palette() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = owner_fixture();
        // First turn: bogus tool. Second turn: end_turn. The bad tool
        // returns is_error=true; the loop continues and the personality
        // just ends.
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![
            ScriptedTurn::tool_use("flavor/no-such-tool", serde_json::json!({})),
            ScriptedTurn::end_turn(),
        ]));
        let engine = build_test_engine(pg.clone(), TestPersonality::new(), scripted);
        instantiate_test_personality(&engine, &owner).await?;
        ingest_test_fact(&pg, &owner, "trigger").await;

        let _ = engine.run_dispatcher_tick().await?;
        let unauthorized_memories: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.memories
             WHERE personality_type_id = $1
               AND kind = 'Perspective'
               AND schema_id = $2",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(TEST_PERSPECTIVE_SCHEMA)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(unauthorized_memories, 0, "no perspective should be written");
        let succeeded: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.personality_wake_invocations
             WHERE status = 'succeeded'",
        )
        .fetch_one(pg.pool())
        .await?;
        assert!(
            succeeded >= 1,
            "wake must complete (succeeded) even with a bad tool call"
        );
        Ok(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("rejects_tool_outside_palette failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_emit_outside_writeable_schemas() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = owner_fixture();
        // emit_perspective with a schema NOT in writeable_schemas. The
        // tool returns is_error; agent ends its turn.
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![
            ScriptedTurn::tool_use(
                "core/emit_perspective",
                serde_json::json!({
                    "schema_id": "proxima-test/test-personality-self-v1",
                    "schema_version": 1,
                    "payload": {"display_name": "x", "purpose": "y"},
                }),
            ),
            ScriptedTurn::end_turn(),
        ]));
        let engine = build_test_engine(pg.clone(), TestPersonality::new(), scripted);
        instantiate_test_personality(&engine, &owner).await?;
        ingest_test_fact(&pg, &owner, "trigger").await;

        let _ = engine.run_dispatcher_tick().await?;

        let unauthorized: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.memories
             WHERE personality_type_id = $1
               AND schema_id = 'proxima-test/test-personality-self-v1'
               AND text != 'Test Personality'",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .fetch_one(pg.pool())
        .await?;
        // The only matching row should be the self-Perspective written
        // at instantiation; no NEW unauthorized rows.
        assert_eq!(
            unauthorized, 0,
            "writes outside writeable_schemas must be rejected"
        );
        Ok(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("rejects_emit_outside_writeable_schemas failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_create_edge_for_substrate_only_relations() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = owner_fixture();
        let m1 = uuid::Uuid::now_v7();
        let m2 = uuid::Uuid::now_v7();
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![
            ScriptedTurn::tool_use(
                "core/create_edge",
                serde_json::json!({
                    "source_memory_id": m1,
                    "relation_id": CORE_DERIVED_FROM_RELATION,
                    "target_memory_id": m2,
                }),
            ),
            ScriptedTurn::end_turn(),
        ]));
        let engine = build_test_engine(pg.clone(), TestPersonality::new(), scripted);
        instantiate_test_personality(&engine, &owner).await?;
        ingest_test_fact(&pg, &owner, "trigger").await;

        let edges_before: i64 =
            sqlx::query_scalar("SELECT count(*) FROM proxima_core.edges WHERE relation = $1")
                .bind(CORE_DERIVED_FROM_RELATION)
                .fetch_one(pg.pool())
                .await?;

        let _ = engine.run_dispatcher_tick().await?;

        let edges_after: i64 =
            sqlx::query_scalar("SELECT count(*) FROM proxima_core.edges WHERE relation = $1")
                .bind(CORE_DERIVED_FROM_RELATION)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(
            edges_before, edges_after,
            "no substrate-only edge may be written via core/create_edge"
        );
        Ok(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("rejects_create_edge_for_substrate_only_relations failed");
}
