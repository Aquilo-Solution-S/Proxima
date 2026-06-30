use proxima_core::relation::{CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION};
use proxima_core::{
    AbstractionPayload, DerivedEdgeSpec, EdgeAuthorshipKind, EdgeId, EntityKind, MemoryId,
    MemoryOperatorKind, Owner, RegisteredRelation, SchemaVersion, ToolCtx, ToolError,
};
use proxima_storage_pg::sidecars::PgMemorySidecar;
use proxima_storage_pg::verbs::derive_append::{DerivedDraft, append_derived_with_edges_in_tx};
use proxima_storage_pg::verbs::edge_write::{MemoryEndpoint, append_owner_checked_memory_edge};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::payloads::CodeExecutionPlanV1;

use super::super::caller;
use super::types::ExecutionPlanItemArgs;
use super::{execution_plan_input_contract_id, execution_plan_operator_id};

#[derive(Debug)]
pub(super) struct PlanAppendOutcome {
    pub(super) memory_id: MemoryId,
    pub(super) edge_ids: Vec<Uuid>,
    pub(super) idempotent_replay: bool,
}

pub(super) fn default_plan_key(
    goal_activated_memory_id: MemoryId,
    items: &[ExecutionPlanItemArgs],
) -> String {
    let item_keys = items
        .iter()
        .map(|item| item.key.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let candidate = format!("plan:{}:{item_keys}", goal_activated_memory_id.into_inner());
    if candidate.len() <= 240 {
        candidate
    } else {
        format!(
            "plan:{}",
            Uuid::new_v5(&Uuid::NAMESPACE_OID, candidate.as_bytes())
        )
    }
}

/// Deterministic, idempotent `MemoryId` for an execution plan.
///
/// Org-free (Track B / S0): the key folds the owner *principal* id, the
/// repo, the activated-goal memory, and the plan key — never a tenant/org
/// scalar. Re-issuing the same plan under the same principal reproduces
/// this id by construction.
pub(super) fn execution_plan_memory_id(
    owner: &Owner,
    repo_id: Uuid,
    goal_activated_memory_id: MemoryId,
    plan_key: &str,
) -> MemoryId {
    let mut key = Vec::new();
    key.extend_from_slice(owner.stable_key_uuid().as_bytes());
    key.extend_from_slice(repo_id.as_bytes());
    key.extend_from_slice(goal_activated_memory_id.into_inner().as_bytes());
    key.extend_from_slice(plan_key.as_bytes());
    MemoryId::new(Uuid::new_v5(&Uuid::NAMESPACE_OID, &key))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn append_execution_plan(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    planner_root: MemoryId,
    goal_activated_memory_id: MemoryId,
    plan_source_memory_id: MemoryId,
    evidence: &[MemoryId],
    plan_key: &str,
    plan_summary: &str,
    payload: &CodeExecutionPlanV1,
) -> Result<PlanAppendOutcome, ToolError> {
    let owner = ctx.owner();
    let caller = caller(ctx)?;
    let memory_id =
        execution_plan_memory_id(&owner, payload.repo_id, goal_activated_memory_id, plan_key);
    let draft = DerivedDraft {
        memory_id: memory_id.into_inner(),
        owner,
        kind: EntityKind::Abstraction,
        schema_id: <CodeExecutionPlanV1 as AbstractionPayload>::schema_id(),
        schema_version: SchemaVersion::new(CodeExecutionPlanV1::SCHEMA_VERSION),
        text: plan_summary.to_string(),
        operator_kind: MemoryOperatorKind::AtoA,
        operator_id: execution_plan_operator_id(),
        input_contract_id: execution_plan_input_contract_id(),
        source_batch_id: None,
        model_id: caller.model_id(),
        prompt_version: "proxima-code/emit_execution_plan-v1",
        supersedes: None,
        embedding: None,
        embedding_model_id: None,
    };
    let derived_relation = ctx
        .registry()
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| ToolError::Other("core/derived-from relation not registered".into()))?;
    let proof_edges = [execution_plan_proof_edge(
        &owner,
        memory_id,
        plan_source_memory_id,
        derived_relation,
    )];
    let sidecar_payload = payload.clone();
    let outcome = append_derived_with_edges_in_tx(tx, &draft, &proof_edges, move |tx, outcome| {
        Box::pin(async move {
            sidecar_payload
                .insert_memory_sidecar(tx, outcome.memory_id)
                .await
        })
    })
    .await
    .map_err(ToolError::Storage)?;
    let mut edge_ids = Vec::new();
    if !outcome.idempotent_replay {
        edge_ids.push(append_plan_authored_edge(tx, ctx, planner_root, memory_id).await?);
        for memory_id in evidence {
            edge_ids.push(
                append_plan_fact_evidence_edge(tx, ctx, outcome.memory_id, *memory_id).await?,
            );
        }
    }
    Ok(PlanAppendOutcome {
        memory_id: outcome.memory_id,
        edge_ids,
        idempotent_replay: outcome.idempotent_replay,
    })
}

async fn append_plan_authored_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    planner_root: MemoryId,
    plan_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| ToolError::Other("core/authored relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::perspective(planner_root),
        MemoryEndpoint::abstraction(plan_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        Some(planner_root),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

fn execution_plan_proof_edge<'a>(
    owner: &'a Owner,
    plan_memory_id: MemoryId,
    plan_source_memory_id: MemoryId,
    relation: RegisteredRelation<'a>,
) -> DerivedEdgeSpec<'a> {
    DerivedEdgeSpec {
        owner,
        relation,
        source_kind: EntityKind::Abstraction,
        source_memory_id: plan_memory_id,
        target_kind: EntityKind::Abstraction,
        target_memory_id: plan_source_memory_id,
        authorship_kind: EdgeAuthorshipKind::OperatorAtoA,
        authorship_owner_memory_id: Some(plan_source_memory_id),
        sidecar_payload: None,
    }
}

pub(super) async fn append_plan_fact_evidence_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    plan_memory_id: MemoryId,
    target_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| ToolError::Other("core/derived-from relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::abstraction(plan_memory_id),
        MemoryEndpoint::fact(target_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective(),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}
