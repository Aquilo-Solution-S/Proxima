//! When a `personality_wake_config.wake_filters` row fails strict
//! deserialization (e.g. an envelope is missing the `version` field
//! after a schema migration), the dispatcher must mark the row
//! `needs_repair` and refuse to fire. Calling `set_wake_config(..)`
//! with valid filters returns the row to `active` and wakes resume.

mod common;

use std::sync::Arc;

use common::personality::{
    apply_test_schemas, build_test_engine, ingest_test_fact, instantiate_test_personality,
    TestPersonality, TEST_FACT_SCHEMA, TEST_PERSONALITY_TYPE_ID,
};
use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::llm::scripted::{ScriptedAnthropicClient, ScriptedTurn};
use proxima_core::personality::{SetWakeConfigRequest, WakeFilter};
use proxima_core::SchemaId;

#[tokio::test(flavor = "multi_thread")]
async fn dispatcher_marks_needs_repair_and_recovers_on_set_wake_config() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = owner_fixture();
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![
            ScriptedTurn::end_turn(),
            ScriptedTurn::end_turn(),
        ]));
        let engine = build_test_engine(pg.clone(), TestPersonality::new(), scripted);
        let inst = instantiate_test_personality(&engine, &owner).await?;

        // Inject a malformed wake_filters JSONB by direct UPDATE.
        // Missing `version` causes strict deserialization to fail.
        sqlx::query(
            "UPDATE proxima_core.personality_wake_config
             SET wake_filters = $1::jsonb
             WHERE personality_type_id = $2
               AND personality_instance_id = $3",
        )
        .bind(serde_json::json!([{
            "kind": "on_memory",
            "schema_id": TEST_FACT_SCHEMA,
            "authored_by": { "kind": "any" },
            "probability": 1.0
            // version field intentionally missing
        }]))
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .execute(pg.pool())
        .await?;

        // Author a fact that would normally match.
        ingest_test_fact(&pg, &owner, "match-1").await;

        let _ = engine.run_dispatcher_tick().await?;

        let status: String = sqlx::query_scalar(
            "SELECT status FROM proxima_core.personality_wake_config
             WHERE personality_type_id = $1
               AND personality_instance_id = $2",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(status, "needs_repair");

        let invocations: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.personality_wake_invocations
             WHERE personality_type_id = $1
               AND personality_instance_id = $2",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            invocations, 0,
            "no wakes should fire while config is needs_repair"
        );

        // Repair via set_wake_config.
        engine
            .set_wake_config(SetWakeConfigRequest {
                owner: owner.clone(),
                personality_type_id: TEST_PERSONALITY_TYPE_ID.into(),
                personality_instance_id: inst.instance_id,
                wake_filters: vec![WakeFilter::on_memory(SchemaId::new(
                    TEST_FACT_SCHEMA.into(),
                ))],
            })
            .await?;

        let status_after: String = sqlx::query_scalar(
            "SELECT status FROM proxima_core.personality_wake_config
             WHERE personality_type_id = $1
               AND personality_instance_id = $2",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(status_after, "active");

        ingest_test_fact(&pg, &owner, "match-2").await;
        let _ = engine.run_dispatcher_tick().await?;

        let invocations_after: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.personality_wake_invocations
             WHERE personality_type_id = $1
               AND personality_instance_id = $2",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert!(
            invocations_after >= 1,
            "wake should fire after repair, got {invocations_after}"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("needs_repair test failed");
}
