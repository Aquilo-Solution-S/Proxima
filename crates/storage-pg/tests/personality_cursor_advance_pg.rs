//! Cursor must advance past events that don't match the wake filter.
//! Uses a wake_filter with `probability = 0.0` so no event can match;
//! after the dispatcher tick, no invocation rows exist and the cursor
//! is parked at the last considered seq.

mod common;

use std::sync::Arc;

use common::personality::{
    apply_test_schemas, build_test_engine, ingest_test_fact, instantiate_test_personality,
    TestPersonality, TEST_PERSONALITY_TYPE_ID,
};
use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::llm::scripted::{ScriptedAnthropicClient, ScriptedTurn};
use proxima_core::personality::{AuthorFilter, SetWakeConfigRequest, WakeFilter};
use proxima_core::SchemaId;

#[tokio::test(flavor = "multi_thread")]
async fn cursor_advances_past_unmatched_events() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = owner_fixture();
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![ScriptedTurn::end_turn()]));
        let engine = build_test_engine(pg.clone(), TestPersonality::new(), scripted);
        let inst = instantiate_test_personality(&engine, &owner).await?;

        // Re-set wake config to use probability = 0.0 so nothing
        // matches.
        engine
            .set_wake_config(SetWakeConfigRequest {
                owner: owner.clone(),
                personality_type_id: TEST_PERSONALITY_TYPE_ID.into(),
                personality_instance_id: inst.instance_id,
                wake_filters: vec![WakeFilter::OnMemory {
                    version: 1,
                    schema_id: SchemaId::new(common::personality::TEST_FACT_SCHEMA.into()),
                    authored_by: AuthorFilter::Any,
                    probability: 0.0,
                }],
            })
            .await?;

        for i in 0..50 {
            ingest_test_fact(&pg, &owner, &format!("noop-{i}")).await;
        }
        let max_seq: uuid::Uuid = sqlx::query_scalar(
            "SELECT seq FROM proxima_core.change_event ORDER BY seq DESC LIMIT 1",
        )
        .fetch_one(pg.pool())
        .await?;

        let _ = engine.run_dispatcher_tick().await?;

        let invocation_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.personality_wake_invocations
             WHERE personality_type_id = $1
               AND personality_instance_id = $2",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            invocation_count, 0,
            "no wake invocations expected when probability == 0.0"
        );

        let cursor_seq: uuid::Uuid = sqlx::query_scalar(
            "SELECT last_considered_seq
             FROM proxima_core.personality_wake_cursor
             WHERE personality_type_id = $1
               AND personality_instance_id = $2",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            cursor_seq, max_seq,
            "cursor must advance to max seq even when no events matched"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("cursor_advance test failed");
}
