//! Harness-backed wake dispatch persists a wake-trace Fact with JSONL
//! citation and provenance edges.

mod common;

use std::time::Duration;

use proxima_core::{CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION};
use sqlx::Row;

/// Rust mirror of the `proxima_core.wake_trace_outcome_kind` SQL enum.
/// Mirrors `crates/storage-pg/migrations/.../baseline.sql` enum variants.
/// Used here for typed decode so the test exercises the same enum
/// boundary the runtime persists.
#[derive(Debug, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "proxima_core.wake_trace_outcome_kind", rename_all = "lowercase")]
enum WakeTraceOutcomeKind {
    Succeeded,
    Truncated,
    Failed,
}

#[tokio::test]
async fn harness_wake_persists_trace_fact_jsonl_and_provenance() {
    let Some(fixture) =
        common::seed_dispatch_fixture_with_match_and_engine(Duration::from_millis(100)).await
    else {
        panic!("PG required for tests but unavailable");
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fired = fixture.engine.run_dispatcher_tick().await?;
        assert_eq!(fired, 1);

        let trigger: uuid::Uuid = sqlx::query_scalar(
            "SELECT entity_memory_id FROM proxima_core.change_event WHERE seq = $1",
        )
        .bind(fixture.change_event_seq)
        .fetch_one(fixture.pg.pg.pool())
        .await?;

        let trace = sqlx::query(
            "SELECT wt.memory_id, wt.outcome_kind, wt.invocation_id, wt.jsonl_truncated, \
                    m.personality_instance_id, cm.cited_object_id \
             FROM proxima_core.wake_trace_v1 wt \
             JOIN proxima_core.memories m ON m.memory_id = wt.memory_id \
             JOIN proxima_core.citation_mappings cm ON cm.memory_id = wt.memory_id \
             WHERE wt.wake_entry_id = $1 AND wt.personality_instance_id = $2",
        )
        .bind(fixture.wake_entry_id)
        .bind(fixture.instance_id.into_inner())
        .fetch_one(fixture.pg.pg.pool())
        .await?;
        let trace_memory: uuid::Uuid = trace.try_get("memory_id")?;
        let cited_object_id: uuid::Uuid = trace.try_get("cited_object_id")?;
        assert_eq!(
            trace.try_get::<WakeTraceOutcomeKind, _>("outcome_kind")?,
            WakeTraceOutcomeKind::Succeeded
        );
        assert!(!trace.try_get::<bool, _>("jsonl_truncated")?);
        assert_eq!(
            trace.try_get::<uuid::Uuid, _>("personality_instance_id")?,
            fixture.instance_id.into_inner()
        );

        let jsonl: (Vec<u8>, i64) = sqlx::query_as(
            "SELECT body, byte_len FROM proxima_core.cited_wake_trace_jsonl_v1 \
             WHERE cited_object_id = $1",
        )
        .bind(cited_object_id)
        .fetch_one(fixture.pg.pg.pool())
        .await?;
        assert_eq!(jsonl.0, b"{\"record\":\"test\"}\n");
        assert_eq!(jsonl.1, jsonl.0.len() as i64);

        let authored: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM proxima_core.edges \
             WHERE relation = $1 AND target_memory_id = $2 AND source_kind = 'Perspective'",
        )
        .bind(CORE_AUTHORED_RELATION)
        .bind(trace_memory)
        .fetch_one(fixture.pg.pg.pool())
        .await?;
        assert_eq!(authored, 1);

        let derived: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM proxima_core.edges \
             WHERE relation = $1 AND source_memory_id = $2 AND target_memory_id = $3",
        )
        .bind(CORE_DERIVED_FROM_RELATION)
        .bind(trace_memory)
        .bind(trigger)
        .fetch_one(fixture.pg.pg.pool())
        .await?;
        assert_eq!(derived, 1);

        Ok(())
    }
    .await;

    fixture.cleanup().await;
    result.expect("harness wake trace persisted");
}
