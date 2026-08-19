use proxima_core::{
    AbstractionPayload, AuthorDerivedRequestInput, EdgeEndpoint, EntityKind, MemoryId,
    MemoryOperatorKind, Owner, SchemaVersion, SidecarPayload, ToolCtx, ToolError, UnitOfWork,
};
use uuid::Uuid;

use crate::payloads::CodeExecutionPlanV1;

use super::types::ExecutionPlanItemArgs;
use super::{execution_plan_input_contract_id, execution_plan_operator_id};

#[derive(Debug)]
pub(super) struct PlanAppendOutcome {
    pub(super) memory_id: MemoryId,
    /// Pins the plan write declared: one `origin` to the Abstraction it
    /// was derived from, plus one `ref` per target its payload names. A
    /// count, not handles — a pin is a column value, not a row with an id.
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
/// referring back to them.
pub(super) async fn append_execution_plan(
    uow: &mut UnitOfWork<'_>,
    ctx: &ToolCtx,
    plan_source_memory_id: MemoryId,
    plan_key: &str,
    plan_summary: &str,
    payload: &CodeExecutionPlanV1,
) -> Result<PlanAppendOutcome, ToolError> {
    let owner = ctx.owner();
    let caller = ctx
        .caller()
        .ok_or_else(|| ToolError::Other("code flavor tools require caller metadata".into()))?;
    let memory_id = execution_plan_memory_id(
        &owner,
        payload.repo_id,
        MemoryId::new(payload.goal_activated_memory_id),
        plan_key,
    );
    // The plan's Abstraction input is what it was made from; everything the
    // payload names — activation Fact, evidence, item requests — is what it
    // points at. Two lists, no kinds.
    let origins = [EdgeEndpoint::memory(
        EntityKind::Abstraction,
        plan_source_memory_id,
    )];
    let outcome = uow
        .author_derived(AuthorDerivedRequestInput {
            memory_id,
            owner,
            kind: EntityKind::Abstraction,
            text: plan_summary.to_string(),
            schema_id: <CodeExecutionPlanV1 as AbstractionPayload>::schema_id(),
            schema_version: SchemaVersion::new(CodeExecutionPlanV1::SCHEMA_VERSION),
            operator_kind: MemoryOperatorKind::AtoA,
            operator_id: execution_plan_operator_id(),
            input_contract_id: execution_plan_input_contract_id(),
            model_id: caller.model_id.as_str(),
            sidecar_payload: SidecarPayload::abstraction(payload.clone()),
            derived_from: &origins,
            extra_refs: &[],
            supersedes: None,
            lexical_language: None,
        })
        .await
        .map_err(ToolError::Protocol)?;
    Ok(PlanAppendOutcome {
        memory_id: outcome.memory_id,
        edge_count: outcome.edge_count,
        idempotent_replay: outcome.idempotent_replay,
    })
}
