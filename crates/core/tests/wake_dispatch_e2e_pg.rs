//! Harness-backed wake dispatch persists a wake-trace Fact with JSONL
//! citation and provenance edges.

mod common;

use std::time::Duration;

use proxima_core::storage::Storage;
use proxima_core::{
    CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind, EntityKind,
    INTERVENTION_SOURCE_ID, InterventionDecisionKind, InterventionDecisionV1,
    InterventionRequestedV1, MemoryId, Owner, OwnerPrincipalKind, Principal, RelationClass,
    SourceBatchId, SourceId, WakeTraceOutcomeKind, intervention_decision_event_draft,
    intervention_request_event_draft,
};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

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

        let program = fixture.mock.latest_program().expect("captured program");
        let contract = program
            .context_params
            .get("wake_contract")
            .expect("wake contract context");
        assert_eq!(contract["label"], "smoke-trigger");
        assert_eq!(contract["trigger_id"], "proxima-test/wake-context-fact-v1");
        assert_eq!(contract["execution_mode"], "substrate_only");
        assert_eq!(
            contract["tool_palettes"]["substrate_tool_palette"][0],
            "core/fetch_memory"
        );

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

#[tokio::test]
async fn intervention_continue_decision_fires_on_next_dispatch_tick() {
    let Some(fixture) =
        common::seed_dispatch_fixture_with_match_and_engine(Duration::from_millis(100)).await
    else {
        panic!("PG required for tests but unavailable");
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        assert_eq!(fixture.engine.run_dispatcher_tick().await?, 1);

        let original = sqlx::query(
            "SELECT i.invocation_id, wt.memory_id AS wake_trace_memory_id,
                    ce.entity_memory_id AS triggering_memory_id
               FROM proxima_core.personality_wake_invocations i
               JOIN proxima_core.wake_trace_v1 wt
                 ON wt.invocation_id = i.invocation_id
               JOIN proxima_core.change_event ce
                 ON ce.seq = i.change_event_seq
              WHERE i.personality_instance_id = $1
                AND i.wake_entry_id = $2
                AND i.change_event_seq = $3",
        )
        .bind(fixture.instance_id.into_inner())
        .bind(fixture.wake_entry_id)
        .bind(fixture.change_event_seq)
        .fetch_one(fixture.pg.pg.pool())
        .await?;
        let original_invocation_id: Uuid = original.try_get("invocation_id")?;
        let wake_trace_memory_id: Uuid = original.try_get("wake_trace_memory_id")?;
        let triggering_memory_id: Uuid = original.try_get("triggering_memory_id")?;

        let (request_memory_id, decision_memory_id) = seed_continue_decision_graph(
            &fixture.pg.pg,
            &fixture.owner,
            ContinueGraphSeed {
                original_invocation_id,
                original_wake_entry_id: fixture.wake_entry_id,
                original_personality_instance_id: fixture.instance_id.into_inner(),
                original_change_event_seq: fixture.change_event_seq,
                triggering_memory_id,
                wake_trace_memory_id,
            },
        )
        .await?;
        insert_decision_request_edge(
            &fixture.pg.pg,
            &fixture.owner,
            decision_memory_id,
            request_memory_id,
        )
        .await?;

        assert_eq!(fixture.engine.run_dispatcher_tick().await?, 1);
        assert_eq!(fixture.engine.run_dispatcher_tick().await?, 0);

        let continuation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)
               FROM proxima_core.personality_wake_invocations
              WHERE continuation_intervention_decision_memory_id = $1
                AND continuation_original_invocation_id = $2",
        )
        .bind(decision_memory_id.into_inner())
        .bind(original_invocation_id)
        .fetch_one(fixture.pg.pg.pool())
        .await?;
        assert_eq!(continuation_count, 1);

        let continuation_event: Uuid = sqlx::query_scalar(
            "SELECT change_event_seq
               FROM proxima_core.personality_wake_invocations
              WHERE continuation_intervention_decision_memory_id = $1",
        )
        .bind(decision_memory_id.into_inner())
        .fetch_one(fixture.pg.pg.pool())
        .await?;
        let decision_event: Uuid = sqlx::query_scalar(
            "SELECT seq
               FROM proxima_core.change_event
              WHERE entity_memory_id = $1",
        )
        .bind(decision_memory_id.into_inner())
        .fetch_one(fixture.pg.pg.pool())
        .await?;
        assert_eq!(continuation_event, decision_event);

        Ok(())
    }
    .await;

    fixture.cleanup().await;
    result.expect("intervention continue dispatch failed");
}

struct ContinueGraphSeed {
    original_invocation_id: Uuid,
    original_wake_entry_id: Uuid,
    original_personality_instance_id: Uuid,
    original_change_event_seq: Uuid,
    triggering_memory_id: Uuid,
    wake_trace_memory_id: Uuid,
}

async fn seed_continue_decision_graph(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    seed: ContinueGraphSeed,
) -> Result<(MemoryId, MemoryId), Box<dyn std::error::Error>> {
    let now = OffsetDateTime::now_utc();
    let request = InterventionRequestedV1 {
        original_invocation_id: seed.original_invocation_id,
        original_wake_entry_id: seed.original_wake_entry_id,
        original_personality_instance_id: seed.original_personality_instance_id,
        original_change_event_seq: seed.original_change_event_seq,
        triggering_memory_id: seed.triggering_memory_id,
        wake_trace_memory_id: seed.wake_trace_memory_id,
        target_intervention_personality_instance_id: Uuid::now_v7(),
        max_rounds: 4,
        rounds_used: 4,
        intervention_extension_rounds: 4,
        intervention_hard_cap_rounds: 8,
        continued_rounds_used: 0,
        active_goal_ids: Vec::new(),
        progress_contract: "continue if graph state shows progress".into(),
        idempotency_key: format!("request-{}", seed.original_invocation_id),
        requested_at: now,
    };
    let mut request_payload = Vec::new();
    ciborium::ser::into_writer(&request, &mut request_payload)?;
    let request_outcome = pg
        .ingest_event_atomic(&intervention_request_event_draft(
            owner.clone(),
            &request_payload,
            SourceBatchId::new(Uuid::now_v7()),
            SourceId::new(INTERVENTION_SOURCE_ID),
            now,
        ))
        .await?;
    sqlx::query(
        "INSERT INTO proxima_core.intervention_requested_v1
            (memory_id, original_invocation_id, original_wake_entry_id,
             original_personality_instance_id, original_change_event_seq,
             triggering_memory_id, wake_trace_memory_id,
             target_intervention_personality_instance_id, max_rounds, rounds_used,
             intervention_extension_rounds, intervention_hard_cap_rounds, continued_rounds_used,
             active_goal_ids, progress_contract, requested_at, idempotency_key)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
    )
    .bind(request_outcome.memory_id.into_inner())
    .bind(request.original_invocation_id)
    .bind(request.original_wake_entry_id)
    .bind(request.original_personality_instance_id)
    .bind(request.original_change_event_seq)
    .bind(request.triggering_memory_id)
    .bind(request.wake_trace_memory_id)
    .bind(request.target_intervention_personality_instance_id)
    .bind(i32::from(request.max_rounds))
    .bind(i32::from(request.rounds_used))
    .bind(i32::from(request.intervention_extension_rounds))
    .bind(i32::from(request.intervention_hard_cap_rounds))
    .bind(i32::from(request.continued_rounds_used))
    .bind(&request.active_goal_ids)
    .bind(&request.progress_contract)
    .bind(request.requested_at)
    .bind(&request.idempotency_key)
    .execute(pg.pool())
    .await?;

    let decision = InterventionDecisionV1 {
        intervention_request_memory_id: request_outcome.memory_id.into_inner(),
        decision: InterventionDecisionKind::Continue,
        grant_rounds: Some(3),
        redirect_personality_instance_id: None,
        rationale: "prior trace has useful work".into(),
        idempotency_key: format!("decision-{}", seed.original_invocation_id),
        decided_at: now,
    };
    let mut decision_payload = Vec::new();
    ciborium::ser::into_writer(&decision, &mut decision_payload)?;
    let decision_outcome = pg
        .ingest_event_atomic(&intervention_decision_event_draft(
            owner.clone(),
            &decision_payload,
            SourceBatchId::new(Uuid::now_v7()),
            SourceId::new(INTERVENTION_SOURCE_ID),
            now,
        ))
        .await?;
    sqlx::query(
        "INSERT INTO proxima_core.intervention_decision_v1
            (memory_id, intervention_request_memory_id, decision, grant_rounds,
             redirect_personality_instance_id, rationale, decided_at, idempotency_key)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(decision_outcome.memory_id.into_inner())
    .bind(decision.intervention_request_memory_id)
    .bind(decision.decision)
    .bind(decision.grant_rounds.map(i32::from))
    .bind(decision.redirect_personality_instance_id)
    .bind(&decision.rationale)
    .bind(decision.decided_at)
    .bind(&decision.idempotency_key)
    .execute(pg.pool())
    .await?;

    Ok((request_outcome.memory_id, decision_outcome.memory_id))
}

async fn insert_decision_request_edge(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    decision_memory_id: MemoryId,
    request_memory_id: MemoryId,
) -> Result<(), Box<dyn std::error::Error>> {
    let principal_id = match owner.principal {
        Principal::User(id) => id.into_inner(),
        Principal::Group(id) => id.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, target_kind, target_memory_id,
             authorship_kind, owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(Uuid::now_v7())
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(RelationClass::Provenance)
    .bind(EntityKind::Fact)
    .bind(decision_memory_id.into_inner())
    .bind(EntityKind::Fact)
    .bind(request_memory_id.into_inner())
    .bind(EdgeAuthorshipKind::ExternalAgent)
    .bind(OwnerPrincipalKind::of(&owner.principal))
    .bind(principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pg.pool())
    .await?;
    Ok(())
}
