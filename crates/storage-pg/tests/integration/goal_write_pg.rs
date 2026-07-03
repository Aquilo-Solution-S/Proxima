//! End-to-end core Goal storage atoms against a transient PG database.

use crate::common::{create_db, db_url, drop_db, owner_parts, owner_write_permit};

use proxima_core::storage_ports::{GoalWritePort, OwnerWritePermit};
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalAssignmentTarget, GoalAtomicContext, GoalAuthorship,
    GoalDependencyRef, GoalDraft, GoalEvidenceRef, GoalPayloadWrite, GoalState, GoalTopologyWrite,
    GoalWakeConfigWrite, GoalWakeToolId, GoalWakeTrigger, GoalWriteOutcome, IdempotencyKey,
    OperatorKind, SystemOrigin, TransitionGoalAtomicRequest,
};
use proxima_core::{
    FlavorRegistryFrozen, GoalPayload, InputContractId, MemoryId, ModelId, OperatorId, Owner,
    PayloadKeyBuilder, PromptVersion, SchemaId, SchemaVersion, StorageError,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct TestCustomGoalPayload {
    note: String,
}

impl GoalPayload for TestCustomGoalPayload {
    const SCHEMA_ID: &'static str = "test/custom-goal-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn goal_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("note", &self.note);
        key.finish()
    }
}

fn fresh_draft(owner: &Owner, request_id: String) -> GoalDraft {
    GoalDraft {
        owner: *owner,
        schema_id: SchemaId::new("core/simple-text-v1".into()),
        schema_version: SchemaVersion::new(1),
        title: "Test goal".to_string(),
        text: "Test goal text".to_string(),
        payload: b"{}".to_vec(),
        sidecar_payload: None,
        state: GoalState::Active,
        topology: goal_topology(MemoryId::new(Uuid::nil()), Vec::new(), Vec::new()),
        wake: None,
        supersedes_goal_id: None,
        authorship: GoalAuthorship::User,
        request_id,
    }
}

fn goal_topology(
    assignment: MemoryId,
    dependencies: Vec<proxima_core::GoalId>,
    evidence: Vec<GoalEvidenceRef>,
) -> GoalTopologyWrite {
    GoalTopologyWrite::new(
        GoalAssignmentTarget::perspective(assignment),
        dependencies
            .into_iter()
            .map(GoalDependencyRef::new)
            .collect(),
        evidence,
    )
    .expect("test goal topology is valid")
}

fn replacement_payload(title: &str, text: &str, payload: &[u8]) -> GoalPayloadWrite {
    GoalPayloadWrite {
        schema_id: SchemaId::new("core/simple-text-v1".into()),
        schema_version: SchemaVersion::new(1),
        title: title.to_string(),
        text: text.to_string(),
        payload: payload.to_vec(),
        sidecar_payload: None,
    }
}

fn goal_context(registry: &FlavorRegistryFrozen, self_id: MemoryId) -> GoalAtomicContext<'_> {
    GoalAtomicContext {
        registry,
        embedding_model_id: None,
        author_self_perspective_id: Some(self_id),
    }
}

async fn goal_permit(owner: &Owner) -> Result<OwnerWritePermit, StorageError> {
    owner_write_permit(owner, proxima_core::AccessKind::Goal)
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))
}

fn operator_authorship() -> GoalAuthorship {
    GoalAuthorship::System(SystemOrigin::Operator {
        operator_id: OperatorId::new(Uuid::now_v7()),
        operator_kind: OperatorKind::AtoGoal,
        input_contract_id: InputContractId::new(Uuid::now_v7()),
        model_id: ModelId::new("test-model"),
        prompt_version: PromptVersion::new("goal-write-pg"),
    })
}

fn wake_config(
    registry: &FlavorRegistryFrozen,
    trigger: GoalWakeTrigger,
    hard_memory_ids: &[MemoryId],
) -> GoalWakeConfigWrite {
    let search =
        GoalWakeToolId::parse("core_search_memories", registry).expect("registered search tool");
    GoalWakeConfigWrite::new(trigger, vec![search], "wake prompt", hard_memory_ids)
        .expect("wake config shape")
}

async fn insert_self(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = owner_parts(owner);
    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/self', 1, $4,
                 'self', $5, '00000000-0000-0000-0000-000000000331'::uuid,
                 '00000000-0000-0000-0000-000000000332'::uuid, NULL,
                 'test-model', 'v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(proxima_core::EntityKind::Perspective)
    .bind(proxima_core::MemoryOperatorKind::AtoP)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(MemoryId::new(memory_id))
}

async fn insert_evidence_abstraction(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = owner_parts(owner);
    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/evidence-abstraction', 1, $4,
                 'evidence', $5, '00000000-0000-0000-0000-000000000333'::uuid,
                 '00000000-0000-0000-0000-000000000334'::uuid, NULL,
                 'test-model', 'v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(proxima_core::EntityKind::Abstraction)
    .bind(proxima_core::MemoryOperatorKind::AtoA)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(MemoryId::new(memory_id))
}

async fn create_goal(
    pg: &PgStorage,
    registry: &proxima_core::FlavorRegistryFrozen,
    self_id: MemoryId,
    mut draft: GoalDraft,
) -> Result<GoalWriteOutcome, proxima_core::StorageError> {
    let permit = goal_permit(&draft.owner()).await?;
    draft.topology = GoalTopologyWrite::new(
        GoalAssignmentTarget::perspective(self_id),
        draft.topology.dependencies().to_vec(),
        draft.topology.evidence().to_vec(),
    )
    .map_err(|err| StorageError::ConstraintViolation(err.message))?;
    pg.create_goal_atomic(
        &CreateGoalAtomicRequest {
            draft,
            context: GoalAtomicContext {
                registry,
                embedding_model_id: None,
                author_self_perspective_id: Some(self_id),
            },
        },
        &permit,
    )
    .await
}

async fn achieve_goal(
    pg: &PgStorage,
    registry: &FlavorRegistryFrozen,
    self_id: MemoryId,
    owner: Owner,
    prior_goal_id: proxima_core::GoalId,
    request_id: &str,
    evidence: Vec<proxima_core::verbs::goal_write::GoalEvidenceRef>,
) -> Result<GoalWriteOutcome, proxima_core::StorageError> {
    let permit = goal_permit(&owner).await?;
    pg.achieve_goal_atomic(
        &AchieveGoalAtomicRequest {
            owner,
            prior_goal_id,
            authorship: GoalAuthorship::User,
            request_id: IdempotencyKey::new(request_id).expect("valid idempotency key"),
            context: goal_context(registry, self_id),
            evidence,
        },
        &permit,
    )
    .await
}

async fn transition_goal(
    pg: &PgStorage,
    registry: &FlavorRegistryFrozen,
    self_id: MemoryId,
    owner: Owner,
    prior_goal_id: proxima_core::GoalId,
    next_state: GoalState,
    request_id: &str,
) -> Result<GoalWriteOutcome, proxima_core::StorageError> {
    let permit = goal_permit(&owner).await?;
    pg.transition_goal_atomic(
        &TransitionGoalAtomicRequest {
            owner,
            prior_goal_id,
            next_state,
            authorship: GoalAuthorship::User,
            request_id: IdempotencyKey::new(request_id).expect("valid idempotency key"),
            context: goal_context(registry, self_id),
        },
        &permit,
    )
    .await
}

async fn decompose_goal(
    pg: &PgStorage,
    registry: &FlavorRegistryFrozen,
    self_id: MemoryId,
    owner: Owner,
    parent_goal_id: proxima_core::GoalId,
    children: Vec<ChildGoalDraft>,
) -> Result<DecomposeGoalOutcome, proxima_core::StorageError> {
    let permit = goal_permit(&owner).await?;
    pg.decompose_goal_atomic(
        &DecomposeGoalAtomicRequest {
            owner,
            parent_goal_id,
            authorship: GoalAuthorship::User,
            context: goal_context(registry, self_id),
            topology: goal_topology(self_id, Vec::new(), Vec::new()),
            children,
        },
        &permit,
    )
    .await
}

mod achieve;
mod create;
mod decompose;
mod modify;
mod transition;
