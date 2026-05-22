//! Characterization tests for `core/emit_intervention_decision`.
//!
//! The tool's database path had no integration coverage before the
//! `InterventionStore` refactor. These tests pin its behavior — the
//! decision Fact, the typed sidecar row, the provenance edges, and
//! idempotent replay — so the raw-SQL-to-verb move is verifiable.

mod common;

use std::sync::Arc;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::auth::NoAuth;
use proxima_core::mcp::McpAuthorContext;
use proxima_core::mcp::core_tools::intervention::{
    EmitInterventionDecisionArgs, EmitInterventionDecisionTool,
};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    Engine, FlavorRegistry, INTERVENTION_SOURCE_ID, InstantiatePersonalityRequest,
    InterventionDecisionKind, InterventionRequestedV1, McpTool, McpToolCtx, MemoryId, OutputMode,
    Owner, SourceBatchId, SourceId, Storage, intervention_request_event_draft,
};
use uuid::Uuid;

#[tokio::test]
async fn intervention_continue_decision_writes_fact_sidecar_and_edges()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let (supervisor_instance, supervisor_self) = instantiate_supervisor(&pg, &owner).await?;
    let request =
        insert_intervention_request(&pg, &owner, supervisor_instance, supervisor_self).await?;
    let ctx = ctx(&pg, owner.clone(), Some(supervisor_self));

    let output = EmitInterventionDecisionTool::call(
        ctx,
        EmitInterventionDecisionArgs {
            intervention_request: request.into_inner().to_string(),
            decision: InterventionDecisionKind::Continue,
            grant_rounds: Some(2),
            redirect_personality: None,
            rationale: "prior trace shows progress".into(),
            idempotency_key: "decision-continue".into(),
        },
    )
    .await?;
    assert!(!output.intervention_decision.is_empty());
    assert_eq!(output.decision, InterventionDecisionKind::Continue);

    let sidecars: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.intervention_decision_v1
          WHERE intervention_request_memory_id = $1",
    )
    .bind(request.into_inner())
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(sidecars, 1);

    let authored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.edges WHERE relation = 'core/authored'",
    )
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(authored, 1);
    let derived: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.edges WHERE relation = 'core/derived-from'",
    )
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(derived, 1);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn intervention_decision_replays_on_idempotency_key()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let (supervisor_instance, supervisor_self) = instantiate_supervisor(&pg, &owner).await?;
    let request =
        insert_intervention_request(&pg, &owner, supervisor_instance, supervisor_self).await?;

    let args = || EmitInterventionDecisionArgs {
        intervention_request: request.into_inner().to_string(),
        decision: InterventionDecisionKind::Stop,
        grant_rounds: None,
        redirect_personality: None,
        rationale: "no progress, stop".into(),
        idempotency_key: "decision-stop".into(),
    };

    let first =
        EmitInterventionDecisionTool::call(ctx(&pg, owner.clone(), Some(supervisor_self)), args())
            .await?;
    let second =
        EmitInterventionDecisionTool::call(ctx(&pg, owner.clone(), Some(supervisor_self)), args())
            .await?;
    assert_eq!(first.intervention_decision, second.intervention_decision);

    let sidecars: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.intervention_decision_v1
          WHERE intervention_request_memory_id = $1",
    )
    .bind(request.into_inner())
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(sidecars, 1);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn intervention_decision_rejects_non_supervisor_caller()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let (supervisor_instance, supervisor_self) = instantiate_supervisor(&pg, &owner).await?;
    // A second personality whose Self is not the targeted supervisor.
    let (_other_instance, other_self) = instantiate_supervisor(&pg, &owner).await?;
    let request =
        insert_intervention_request(&pg, &owner, supervisor_instance, supervisor_self).await?;

    let err = EmitInterventionDecisionTool::call(
        ctx(&pg, owner.clone(), Some(other_self)),
        EmitInterventionDecisionArgs {
            intervention_request: request.into_inner().to_string(),
            decision: InterventionDecisionKind::Stop,
            grant_rounds: None,
            redirect_personality: None,
            rationale: "not my call".into(),
            idempotency_key: "decision-wrong-caller".into(),
        },
    )
    .await
    .expect_err("non-supervisor caller should be rejected");
    assert!(err.to_string().contains("Wake Supervisor"));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn instantiate_supervisor(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<(Uuid, MemoryId), Box<dyn std::error::Error>> {
    let personality = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Wake Supervisor".into(),
            purpose: "Decide on intervention requests".into(),
        })
        .await?;
    let root = pg
        .list_personality_instances(owner, false)
        .await?
        .into_iter()
        .find(|row| row.personality_instance_id == personality.instance_id)
        .expect("personality row")
        .current_root_perspective_memory_id;
    Ok((personality.instance_id.into_inner(), root))
}

async fn insert_intervention_request(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    target_personality: Uuid,
    existing_memory: MemoryId,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let now = time::OffsetDateTime::now_utc();
    // `triggering_memory_id` / `wake_trace_memory_id` carry FKs to
    // `memories`; the tool never reads them, so an existing memory id
    // satisfies the constraint.
    let request = InterventionRequestedV1 {
        original_invocation_id: Uuid::now_v7(),
        original_wake_entry_id: Uuid::now_v7(),
        original_personality_instance_id: Uuid::now_v7(),
        original_change_event_seq: Uuid::now_v7(),
        triggering_memory_id: existing_memory.into_inner(),
        wake_trace_memory_id: existing_memory.into_inner(),
        target_intervention_personality_instance_id: target_personality,
        max_rounds: 4,
        rounds_used: 4,
        intervention_extension_rounds: 4,
        intervention_hard_cap_rounds: 8,
        continued_rounds_used: 0,
        active_goal_ids: Vec::new(),
        progress_contract: "continue if graph state shows progress".into(),
        idempotency_key: format!("request-{}", Uuid::now_v7()),
        requested_at: now,
    };
    let mut payload = Vec::new();
    ciborium::ser::into_writer(&request, &mut payload)?;
    let outcome = pg
        .ingest_event_atomic(&intervention_request_event_draft(
            owner.clone(),
            &payload,
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
    .bind(outcome.memory_id.into_inner())
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
    Ok(outcome.memory_id)
}

fn ctx(
    pg: &proxima_storage_pg::PgStorage,
    owner: Owner,
    caller_self_perspective: Option<MemoryId>,
) -> McpToolCtx {
    let registry = Arc::new(FlavorRegistry::new().freeze());
    let engine = Engine::new(
        (*registry).clone(),
        MemoryStore::new(),
        Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
    )
    .with_storage(pg.clone().into_handle());
    McpToolCtx {
        pool: pg.pool().clone(),
        owner,
        handles: None,
        mode: OutputMode::RawIds,
        registry,
        author: McpAuthorContext {
            model_id: "test/model".into(),
            client_name: "test".into(),
            client_version: "1".into(),
            caller_self_perspective,
        },
        caller_self_perspective,
        master_token_id: None,
        engine: Some(Arc::new(engine)),
    }
}
