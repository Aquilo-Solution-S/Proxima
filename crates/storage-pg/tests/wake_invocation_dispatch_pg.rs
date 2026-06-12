//! Phase 1d: Wake invocation dispatch columns survive INSERT/UPDATE
//! roundtrip via the storage trait.

mod common;

use common::personality::ingest_test_fact;
use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::personality::{
    InstantiatePersonalityRequest, ListWakeInvocationsRequest, PersonalityInstanceId,
    SetWakeEntriesRequest, WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryTriggerKind,
    WakeInvocationContinuation, WakeInvocationFinalize, WakeInvocationLogDraft,
    WakeInvocationLogStatus, WakeInvocationStart, WakeInvocationStatus,
};
use proxima_core::storage::Storage;
use proxima_core::{
    CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind, EntityKind, INTERVENTION_SOURCE_ID,
    InterventionDecisionKind, InterventionDecisionV1, InterventionRequestedV1, MemoryId, ModelTier,
    Owner, OwnerPrincipalKind, Principal, RelationClass, SourceBatchId, SourceId,
    intervention_decision_event_draft, intervention_request_event_draft,
};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug)]
struct WakeInvocationDispatchRow {
    wake_token: Option<Uuid>,
    resolved_inference_target_ref: Option<String>,
    failure_reason: Option<String>,
    exit_code: Option<i32>,
    duration_ms: Option<i64>,
    stdout_tail: Option<String>,
    stderr_tail: Option<String>,
    stdout_truncated: bool,
    stderr_truncated: bool,
    status: WakeInvocationStatus,
}

#[tokio::test(flavor = "multi_thread")]
async fn continuation_invocation_can_share_original_wake_natural_key() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let (instance_id, wake_entry_id) = seed_personality_with_entry(&pg, &owner).await?;
        let change_event_seq = Uuid::now_v7();
        let original_invocation_id = Uuid::now_v7();
        let continuation_invocation_id = Uuid::now_v7();
        let intervention_decision_memory_id =
            ingest_test_fact(&pg, &owner, "intervention decision")
                .await
                .into_inner();

        let normal = WakeInvocationStart {
            invocation_id: original_invocation_id,
            owner: owner.clone(),
            personality_instance_id: instance_id,
            wake_entry_id,
            change_event_seq,
            wake_token: Uuid::new_v4(),
            resolved_inference_target_ref: "normal".into(),
            continuation: None,
        };
        assert!(pg.start_wake_invocation(&normal).await?);

        let continuation = WakeInvocationStart {
            invocation_id: continuation_invocation_id,
            owner: owner.clone(),
            personality_instance_id: instance_id,
            wake_entry_id,
            change_event_seq,
            wake_token: Uuid::new_v4(),
            resolved_inference_target_ref: "continuation".into(),
            continuation: Some(WakeInvocationContinuation {
                intervention_decision_memory_id,
                original_invocation_id,
            }),
        };
        assert!(pg.start_wake_invocation(&continuation).await?);
        assert!(!pg.start_wake_invocation(&continuation).await?);

        let listed = pg
            .list_wake_invocations(&ListWakeInvocationsRequest {
                owner,
                personality_instance_id: instance_id,
                wake_entry_id: Some(wake_entry_id),
                triggering_memory_id: None,
                change_event_seq: None,
                limit: 10,
            })
            .await?;
        assert_eq!(listed.len(), 2);
        assert!(
            listed
                .iter()
                .any(|row| row.invocation_id == original_invocation_id)
        );
        let continued = listed
            .iter()
            .find(|row| row.invocation_id == continuation_invocation_id)
            .expect("continuation row");
        assert_eq!(
            continued.continuation_intervention_decision_memory_id,
            Some(intervention_decision_memory_id)
        );
        assert_eq!(
            continued.continuation_original_invocation_id,
            Some(original_invocation_id)
        );
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("continuation invocation identity failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn continuation_candidate_requires_decision_request_derived_edge() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let (request_memory_id, decision_memory_id, expected) =
            seed_intervention_continue_sidecars(&pg, &owner).await?;

        let missing_edge = pg
            .load_intervention_continue_candidate(&owner, decision_memory_id)
            .await?;
        assert!(missing_edge.is_none());

        insert_decision_request_derived_edge(&pg, &owner, decision_memory_id, request_memory_id)
            .await?;
        let candidate = pg
            .load_intervention_continue_candidate(&owner, decision_memory_id)
            .await?
            .expect("derived edge makes continuation candidate visible");
        assert_eq!(
            candidate.intervention_decision_memory_id,
            decision_memory_id
        );
        assert_eq!(candidate.intervention_request_memory_id, request_memory_id);
        assert_eq!(
            candidate.original_invocation_id,
            expected.original_invocation_id
        );
        assert_eq!(
            candidate.original_wake_entry_id,
            expected.original_wake_entry_id
        );
        assert_eq!(
            candidate.original_personality_instance_id.into_inner(),
            expected.original_personality_instance_id
        );
        assert_eq!(
            candidate.original_change_event_seq,
            expected.original_change_event_seq
        );
        assert_eq!(
            candidate.original_triggering_memory_id.into_inner(),
            expected.triggering_memory_id
        );
        assert_eq!(
            candidate.wake_trace_memory_id.into_inner(),
            expected.wake_trace_memory_id
        );
        assert_eq!(candidate.grant_rounds, 3);
        assert_eq!(candidate.rationale, "prior wake made progress");
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("continuation candidate graph-shape test failed");
}

async fn seed_personality_with_entry(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<(PersonalityInstanceId, Uuid), Box<dyn std::error::Error>> {
    let response = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Engineer A".into(),
            purpose: "exercise wake invocation dispatch columns".into(),
        })
        .await?;
    let entry = WakeEntryDraft::new(
        Uuid::now_v7(),
        response.instance_id,
        WakeEntryTriggerKind::OnMemory,
        "proxima-test/fact-v1",
        "on_test_fact",
        WakeEntryAuthoredBy::Any,
        1000,
        ModelTier::Fast,
        Some("primary".to_string()),
        vec!["core/query".to_string()],
        4,
    )
    .expect("valid wake entry");
    let wake_entry_id = entry.wake_entry_id;
    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: response.instance_id,
        entries: vec![entry],
    })
    .await?;
    Ok((response.instance_id, wake_entry_id))
}

struct ExpectedInterventionRequest {
    original_invocation_id: Uuid,
    original_wake_entry_id: Uuid,
    original_personality_instance_id: Uuid,
    original_change_event_seq: Uuid,
    triggering_memory_id: Uuid,
    wake_trace_memory_id: Uuid,
}

#[expect(clippy::too_many_lines, reason = "verbatim sidecar seeding fixture")]
async fn seed_intervention_continue_sidecars(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<(MemoryId, MemoryId, ExpectedInterventionRequest), Box<dyn std::error::Error>> {
    let now = OffsetDateTime::now_utc();
    let triggering_memory_id = ingest_test_fact(pg, owner, "original trigger")
        .await
        .into_inner();
    let wake_trace_memory_id = ingest_test_fact(pg, owner, "wake trace").await.into_inner();
    let request = InterventionRequestedV1 {
        original_invocation_id: Uuid::now_v7(),
        original_wake_entry_id: Uuid::now_v7(),
        original_personality_instance_id: Uuid::now_v7(),
        original_change_event_seq: Uuid::now_v7(),
        triggering_memory_id,
        wake_trace_memory_id,
        target_intervention_personality_instance_id: Uuid::now_v7(),
        max_rounds: 4,
        rounds_used: 4,
        intervention_extension_rounds: 4,
        intervention_hard_cap_rounds: 8,
        continued_rounds_used: 0,
        active_goal_ids: Vec::new(),
        progress_contract: "show progress".into(),
        idempotency_key: format!("request-{}", Uuid::now_v7()),
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
        rationale: "prior wake made progress".into(),
        idempotency_key: format!("decision-{}", Uuid::now_v7()),
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

    Ok((
        request_outcome.memory_id,
        decision_outcome.memory_id,
        ExpectedInterventionRequest {
            original_invocation_id: request.original_invocation_id,
            original_wake_entry_id: request.original_wake_entry_id,
            original_personality_instance_id: request.original_personality_instance_id,
            original_change_event_seq: request.original_change_event_seq,
            triggering_memory_id,
            wake_trace_memory_id,
        },
    ))
}

async fn insert_decision_request_derived_edge(
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

async fn fetch_wake_invocation(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    instance_id: PersonalityInstanceId,
    wake_entry_id: Uuid,
    change_event_seq: Uuid,
) -> Result<WakeInvocationDispatchRow, Box<dyn std::error::Error>> {
    type DispatchRow = (
        Option<Uuid>,
        Option<String>,
        Option<String>,
        Option<i32>,
        Option<i64>,
        Option<String>,
        Option<String>,
        bool,
        bool,
        WakeInvocationStatus,
    );
    let principal_id = match owner.principal {
        Principal::User(id) => id.into_inner(),
        Principal::Group(id) => id.into_inner(),
    };
    let (
        wake_token,
        resolved_inference_target_ref,
        failure_reason,
        exit_code,
        duration_ms,
        stdout_tail,
        stderr_tail,
        stdout_truncated,
        stderr_truncated,
        status,
    ): DispatchRow = sqlx::query_as(
        "SELECT wake_token, resolved_inference_target_ref,
                failure_reason, exit_code, duration_ms, stdout_tail,
                stderr_tail, stdout_truncated, stderr_truncated, status
         FROM proxima_core.personality_wake_invocations
         WHERE owner_principal_id = $1
           AND owner_org_id = $2
           AND personality_instance_id = $3
           AND wake_entry_id = $4
           AND change_event_seq = $5",
    )
    .bind(principal_id)
    .bind(owner.org_id.into_inner())
    .bind(instance_id.into_inner())
    .bind(wake_entry_id)
    .bind(change_event_seq)
    .fetch_one(pg.pool())
    .await?;
    Ok(WakeInvocationDispatchRow {
        wake_token,
        resolved_inference_target_ref,
        failure_reason,
        exit_code,
        duration_ms,
        stdout_tail,
        stderr_tail,
        stdout_truncated,
        stderr_truncated,
        status,
    })
}

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)]
async fn wake_invocation_carries_dispatch_columns() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let (instance_id, wake_entry_id) = seed_personality_with_entry(&pg, &owner).await?;
        let change_event_seq = Uuid::now_v7();

        let wake_token = Uuid::new_v4();
        let invocation_id = Uuid::now_v7();
        let resolved_target = "default-standard";

        let start = WakeInvocationStart {
            invocation_id,
            owner: owner.clone(),
            personality_instance_id: instance_id,
            wake_entry_id,
            change_event_seq,
            wake_token,
            resolved_inference_target_ref: resolved_target.to_string(),
            continuation: None,
        };
        pg.start_wake_invocation(&start).await.expect("start ok");

        let finalize = WakeInvocationFinalize {
            invocation_id,
            owner: owner.clone(),
            personality_instance_id: instance_id,
            wake_entry_id,
            change_event_seq,
            status: WakeInvocationStatus::Failed,
            turn_count: None,
            cost_usd: None,
            failure_reason: Some("workspace_mode_not_yet_implemented".to_string()),
            exit_code: Some(2),
            duration_ms: Some(123),
            stdout_tail: Some("stdout tail".to_string()),
            stderr_tail: Some("stderr tail".to_string()),
            stdout_truncated: false,
            stderr_truncated: false,
        };
        pg.finalize_wake_invocation(&finalize)
            .await
            .expect("finalize ok");

        let row = fetch_wake_invocation(&pg, &owner, instance_id, wake_entry_id, change_event_seq)
            .await?;
        assert_eq!(row.wake_token, Some(wake_token));
        assert_eq!(
            row.resolved_inference_target_ref.as_deref(),
            Some(resolved_target)
        );
        assert_eq!(
            row.failure_reason.as_deref(),
            Some("workspace_mode_not_yet_implemented")
        );
        assert_eq!(row.exit_code, Some(2));
        assert_eq!(row.duration_ms, Some(123));
        assert_eq!(row.stdout_tail.as_deref(), Some("stdout tail"));
        assert_eq!(row.stderr_tail.as_deref(), Some("stderr tail"));
        assert!(!row.stdout_truncated);
        assert!(!row.stderr_truncated);
        assert!(matches!(row.status, WakeInvocationStatus::Failed));

        pg.append_wake_invocation_log(&WakeInvocationLogDraft {
            invocation_id,
            owner: owner.clone(),
            personality_instance_id: instance_id,
            wake_entry_id,
            change_event_seq,
            phase: "tool_call".to_string(),
            tool_id: Some("proxima-agent-memory/proxima_derive".to_string()),
            status: WakeInvocationLogStatus::Failed,
            duration_ms: Some(77),
            message_tail: Some("tool failed".to_string()),
        })
        .await?;
        pg.append_wake_invocation_log(&WakeInvocationLogDraft {
            invocation_id,
            owner: owner.clone(),
            personality_instance_id: instance_id,
            wake_entry_id,
            change_event_seq,
            phase: "session_artifact".to_string(),
            tool_id: None,
            status: WakeInvocationLogStatus::Started,
            duration_ms: None,
            message_tail: Some(
                "~/.proxima/wake-runs/user/example/worker-session.jsonl".to_string(),
            ),
        })
        .await?;
        let listed = pg
            .list_wake_invocations(&ListWakeInvocationsRequest {
                owner: owner.clone(),
                personality_instance_id: instance_id,
                wake_entry_id: Some(wake_entry_id),
                triggering_memory_id: None,
                change_event_seq: None,
                limit: 10,
            })
            .await?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].exit_code, Some(2));
        assert_eq!(listed[0].stdout_tail.as_deref(), Some("stdout tail"));
        assert_eq!(listed[0].logs.len(), 2);
        assert_eq!(
            listed[0].logs[0].tool_id.as_deref(),
            Some("proxima-agent-memory/proxima_derive")
        );
        assert_eq!(
            listed[0].logs[0].message_tail.as_deref(),
            Some("tool failed")
        );
        assert_eq!(listed[0].logs[1].phase, "session_artifact");
        assert_eq!(listed[0].logs[1].status, WakeInvocationLogStatus::Started);
        assert_eq!(
            listed[0].logs[1].message_tail.as_deref(),
            Some("~/.proxima/wake-runs/user/example/worker-session.jsonl")
        );
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("wake invocation dispatch columns roundtrip failed");
}
