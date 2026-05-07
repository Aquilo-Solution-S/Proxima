//! Per-instance queue cap is hard-coded to 10 in the dispatcher
//! (`MAX_INSTANCE_QUEUE_DEPTH`). When more than 10 events match a
//! single instance in one tick, the dispatcher must drop the OLDEST
//! events (advance the cursor past them without firing) and process
//! exactly the 10 newest matching events. The skipped events have no
//! invocation row.

mod common;

use std::sync::Arc;

use common::personality::{
    apply_test_schemas, build_test_engine, ingest_test_fact, instantiate_test_personality,
    TestPersonality, TEST_PERSONALITY_TYPE_ID,
};
use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::llm::scripted::{ScriptedAnthropicClient, ScriptedTurn};

#[tokio::test(flavor = "multi_thread")]
async fn dispatcher_drops_oldest_when_queue_exceeds_cap() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = owner_fixture();
        // 15 events all match -> 10 wakes, 5 dropped. Each scripted
        // turn returns end_turn so the loop ends after one tick.
        let scripted = Arc::new(ScriptedAnthropicClient::new(
            (0..10).map(|_| ScriptedTurn::end_turn()).collect(),
        ));
        let engine = build_test_engine(pg.clone(), TestPersonality::new(), scripted);
        let inst = instantiate_test_personality(&engine, &owner).await?;

        let mut seqs: Vec<uuid::Uuid> = Vec::with_capacity(15);
        for i in 0..15 {
            let memory_id = ingest_test_fact(&pg, &owner, &format!("match-{i}")).await;
            let seq: uuid::Uuid = sqlx::query_scalar(
                "SELECT seq FROM proxima_core.change_event
                 WHERE entity_memory_id = $1
                 ORDER BY seq DESC LIMIT 1",
            )
            .bind(memory_id.into_inner())
            .fetch_one(pg.pool())
            .await?;
            seqs.push(seq);
        }

        let _ = engine.run_dispatcher_tick().await?;

        let invocation_seqs: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT change_event_seq FROM proxima_core.personality_wake_invocations
             WHERE personality_type_id = $1
               AND personality_instance_id = $2
             ORDER BY change_event_seq",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .fetch_all(pg.pool())
        .await?;
        assert_eq!(
            invocation_seqs.len(),
            10,
            "queue cap must clamp to 10 invocations, got {}",
            invocation_seqs.len()
        );

        // The 5 oldest events should NOT have invocation rows; the 10
        // newest should.
        let dropped_seqs: Vec<uuid::Uuid> = seqs[..5].to_vec();
        let kept_seqs: Vec<uuid::Uuid> = seqs[5..].to_vec();
        for seq in dropped_seqs {
            assert!(
                !invocation_seqs.contains(&seq),
                "oldest 5 events must be dropped, but seq {seq} has an invocation row"
            );
        }
        for seq in kept_seqs {
            assert!(
                invocation_seqs.contains(&seq),
                "newest 10 events must each have an invocation row, but seq {seq} is missing"
            );
        }

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
        let max_seq = *seqs.last().expect("at least one seq");
        assert!(
            cursor_seq >= max_seq,
            "cursor must advance past every event, including the dropped ones"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("queue_overflow test failed");
}
