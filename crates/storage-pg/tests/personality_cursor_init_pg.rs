//! Cursor-init invariant: instantiating a personality plants the wake
//! cursor at `max(change_event.seq)` for the owner so prior history
//! does NOT trigger wakes when the dispatcher tick runs.

mod common;

use common::personality::{
    apply_test_schemas, build_test_engine, ingest_test_fact, instantiate_test_personality,
    TestPersonality, TEST_PERSONALITY_TYPE_ID,
};
use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::llm::scripted::{ScriptedAnthropicClient, ScriptedTurn};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn cursor_initializes_at_now_so_history_is_not_replayed() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = owner_fixture();

        for i in 0..20 {
            ingest_test_fact(&pg, &owner, &format!("historical-{i}")).await;
        }

        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![ScriptedTurn::end_turn()]));
        let engine = build_test_engine(pg.clone(), TestPersonality::new(), scripted.clone());
        let _instance = instantiate_test_personality(&engine, &owner).await?;

        let fired = engine.run_dispatcher_tick().await?;
        assert_eq!(
            fired, 0,
            "no wakes should fire on prior history because cursor is initialized at now"
        );

        let cursor_seq: uuid::Uuid = sqlx::query_scalar(
            "SELECT last_considered_seq
             FROM proxima_core.personality_wake_cursor
             WHERE personality_type_id = $1",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .fetch_one(pg.pool())
        .await?;
        let max_seq: uuid::Uuid = sqlx::query_scalar(
            "SELECT seq FROM proxima_core.change_event ORDER BY seq DESC LIMIT 1",
        )
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            cursor_seq, max_seq,
            "cursor must be parked at max(change_event.seq) at instantiation"
        );

        ingest_test_fact(&pg, &owner, "after-instantiate").await;

        // Run a fresh tick. The scripted client's first turn already
        // ended; we feed another end_turn so this wake completes
        // cleanly. We rebuild the engine so the scripted queue is
        // refreshed (ScriptedAnthropicClient is single-shot per turn).
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![ScriptedTurn::end_turn()]));
        let engine = build_test_engine(pg.clone(), TestPersonality::new(), scripted.clone());
        // Note: rebuilding the engine resets the registry but the PG
        // wake_config row persists, so the dispatcher sees the same
        // instance.

        let fired_after = engine.run_dispatcher_tick().await?;
        assert!(
            fired_after >= 1,
            "wake should fire on the post-instantiate fact, got fired={fired_after}"
        );

        let invocation_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.personality_wake_invocations
             WHERE personality_type_id = $1",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(invocation_count, 1, "exactly one wake invocation row");

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("cursor_init test failed");
}
