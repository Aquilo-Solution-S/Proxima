use proxima_core::access::AccessKind;
use proxima_core::{
    AbstractionPayload, DerivedEmbedding, EdgeEndpoint, EntityKind, MemoryId, MemoryOperatorKind,
    Owner, PayloadReference, SchemaVersion, ToolCtx, ToolError,
};
use proxima_storage_pg::sidecars::PgMemorySidecar;
use proxima_storage_pg::verbs::derive_append::{DerivedDraft, append_derived_with_edges_in_tx};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::payloads::CodeExecutionPlanV1;

use super::super::caller;
use super::types::ExecutionPlanItemArgs;
use super::{execution_plan_input_contract_id, execution_plan_operator_id};

#[derive(Debug)]
pub(super) struct PlanAppendOutcome {
    pub(super) memory_id: MemoryId,
    /// Index rows the plan write asserted: one `origin` to the Abstraction
    /// it was derived from, plus one `reference` per target its payload
    /// names. A count, not handles — an edge has no id.
    pub(super) edge_count: usize,
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
/// Org-free: the key folds the owner *principal* id, the
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

/// Append the plan Abstraction after its items exist.
///
/// The order is load-bearing. A plan's payload names the request Fact
/// behind each item, and an index row cannot point at a node that is not
/// there yet — so the items are emitted first and the plan is written last,
/// referring back to them. Under the old model the plan came first and the
/// plan→item edges were appended afterwards, which is exactly the
/// free-standing edge write the model no longer has.
pub(super) async fn append_execution_plan(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    planner_root: MemoryId,
    plan_source_memory_id: MemoryId,
    plan_key: &str,
    plan_summary: &str,
    payload: &CodeExecutionPlanV1,
) -> Result<PlanAppendOutcome, ToolError> {
    let owner = ctx.owner();
    let permit = ctx.owner_write_permit(AccessKind::Perspective).await?;
    let caller = caller(ctx)?;
    let memory_id = execution_plan_memory_id(
        &owner,
        payload.repo_id,
        MemoryId::new(payload.goal_activated_memory_id),
        plan_key,
    );
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
        // "Emitted by the planner" is known at write time, so it is a
        // column on the row rather than a `core/authored` edge.
        authoring_perspective_id: Some(planner_root),
        supersedes: None,
        lexical_language: None,
        embedding: DerivedEmbedding::None,
    };
    // The plan's Abstraction input is what it was made from; everything the
    // payload names — activation Fact, evidence, item requests — is what it
    // points at. Two lists, no kinds.
    let origins = [EdgeEndpoint::memory(
        EntityKind::Abstraction,
        plan_source_memory_id,
    )];
    let references = payload_reference_endpoints(payload)?;
    let edge_count = origins.len() + references.len();
    let sidecar_payload = payload.clone();
    let outcome = append_derived_with_edges_in_tx(
        tx,
        &permit,
        &draft,
        &origins,
        &references,
        move |tx, outcome| {
            Box::pin(async move {
                sidecar_payload
                    .insert_memory_sidecar(tx, outcome.memory_id)
                    .await
            })
        },
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(PlanAppendOutcome {
        memory_id: outcome.memory_id,
        edge_count,
        idempotent_replay: outcome.idempotent_replay,
    })
}

/// Read the payload's schema-declared references, checking each binding
/// against the address form it produced before storage sees the endpoints.
fn payload_reference_endpoints(
    payload: &CodeExecutionPlanV1,
) -> Result<Vec<EdgeEndpoint>, ToolError> {
    <CodeExecutionPlanV1 as AbstractionPayload>::references(payload)
        .into_iter()
        .map(|reference: PayloadReference| {
            reference.validate().map_err(ToolError::Other)?;
            Ok(reference.target)
        })
        .collect()
}
