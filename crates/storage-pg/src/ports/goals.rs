use proxiima_core::goal_collaboration::GoalEditPermit;
use proxiima_core::read_models::ActiveGoalSummary;
use proxiima_core::storage_ports::{GoalReadPort, GoalWritePort, OwnerWritePermit};
use proxiima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalReplayOutcome, GoalReplayRequest, GoalWriteOutcome,
    ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use proxiima_core::{MemoryId, OwnerId, StorageError};

use super::validate_permit_owner;
use crate::{PgStorage, verbs};

fn validate_edit_permit(permit: &GoalEditPermit<'_>, owner: &OwnerId) -> Result<(), StorageError> {
    if permit.owner() != *owner {
        return Err(StorageError::ConstraintViolation(
            "Goal edit permit owner does not match request".into(),
        ));
    }
    if let Some(owner_permit) = permit.owner_write() {
        super::validate_permit_owner(owner_permit, owner)?;
    }
    Ok(())
}

fn edit_request_key(
    permit: GoalEditPermit<'_>,
    original: &proxiima_core::verbs::goal_write::IdempotencyKey,
) -> Result<proxiima_core::verbs::goal_write::IdempotencyKey, StorageError> {
    match permit {
        GoalEditPermit::Owner(_) => Ok(original.clone()),
        GoalEditPermit::Goal(goal) => {
            let digest = blake3::hash(original.as_str().as_bytes());
            proxiima_core::verbs::goal_write::IdempotencyKey::new(format!(
                "goal-edit:{}:{digest}",
                goal.goal().into_inner()
            ))
            .map_err(StorageError::ConstraintViolation)
        }
    }
}

async fn resolve_edit_replay(
    storage: &PgStorage,
    request: GoalReplayRequest<'_, '_>,
    permit: GoalEditPermit<'_>,
    prior: proxiima_core::GoalId,
) -> Result<Option<GoalReplayOutcome>, StorageError> {
    use crate::verbs::goal_write::authority::{begin_goal_edit, revalidate_goal_edit};
    let (mut tx, fence) = begin_goal_edit(storage, permit, prior, &[], None).await?;
    let result = verbs::goal_write::resolve_goal_command_replay_on(&mut tx, request).await?;
    revalidate_goal_edit(storage, &mut tx, fence.as_ref()).await?;
    crate::owner_scope::finish_transaction(tx, Ok(result)).await
}

#[async_trait::async_trait]
impl GoalWritePort for PgStorage {
    async fn resolve_goal_replay(
        &self,
        req: GoalReplayRequest<'_, '_>,
        permit: GoalEditPermit<'_>,
    ) -> Result<Option<GoalReplayOutcome>, StorageError> {
        validate_edit_permit(&permit, &req.owner())?;
        match permit {
            GoalEditPermit::Owner(owner) => {
                let mut tx = crate::owner_scope::begin_owner_transaction(
                    &self.pool,
                    crate::owner_scope::require_owner_scope(owner.owner_scope())?,
                )
                .await?;
                let result = verbs::goal_write::resolve_goal_command_replay_on(&mut tx, req).await;
                crate::owner_scope::finish_transaction(tx, result).await
            }
            GoalEditPermit::Goal(_) => match req {
                GoalReplayRequest::Transition(request) => {
                    let mut request = request.clone();
                    request.request_id = edit_request_key(permit, &request.request_id)?;
                    resolve_edit_replay(
                        self,
                        GoalReplayRequest::Transition(&request),
                        permit,
                        request.prior_goal_id,
                    )
                    .await
                }
                GoalReplayRequest::Achieve(request) => {
                    let mut request = request.clone();
                    request.request_id = edit_request_key(permit, &request.request_id)?;
                    resolve_edit_replay(
                        self,
                        GoalReplayRequest::Achieve(&request),
                        permit,
                        request.prior_goal_id,
                    )
                    .await
                }
                GoalReplayRequest::Modify(request) => {
                    let mut request = request.clone();
                    request.request_id = edit_request_key(permit, &request.request_id)?;
                    resolve_edit_replay(
                        self,
                        GoalReplayRequest::Modify(&request),
                        permit,
                        request.prior_goal_id,
                    )
                    .await
                }
                GoalReplayRequest::Create(_) | GoalReplayRequest::Decompose(_) => {
                    Err(StorageError::NotFound)
                }
            },
        }
    }

    async fn create_goal_atomic(
        &self,
        req: &CreateGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        validate_permit_owner(permit, &req.draft.owner())?;
        verbs::goal_write::create_goal_atomic(
            &self.pool,
            &self.sidecars,
            req,
            permit,
            self.store_number,
        )
        .await
    }

    async fn transition_goal_atomic(
        &self,
        req: &TransitionGoalAtomicRequest<'_>,
        permit: GoalEditPermit<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        validate_edit_permit(&permit, &req.owner)?;
        let mut req = req.clone();
        req.request_id = edit_request_key(permit, &req.request_id)?;
        verbs::goal_write::transition_goal_atomic(self, &req, &permit, self.store_number).await
    }

    async fn achieve_goal_atomic(
        &self,
        req: &AchieveGoalAtomicRequest<'_>,
        permit: GoalEditPermit<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        validate_edit_permit(&permit, &req.owner)?;
        let mut req = req.clone();
        req.request_id = edit_request_key(permit, &req.request_id)?;
        verbs::goal_write::achieve_goal_atomic(self, &req, &permit, self.store_number).await
    }

    async fn modify_goal_atomic(
        &self,
        req: &ModifyGoalAtomicRequest<'_>,
        permit: GoalEditPermit<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        validate_edit_permit(&permit, &req.owner)?;
        let mut req = req.clone();
        req.request_id = edit_request_key(permit, &req.request_id)?;
        verbs::goal_write::modify_goal_atomic(self, &req, &permit, self.store_number).await
    }

    async fn decompose_goal_atomic(
        &self,
        req: &DecomposeGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<DecomposeGoalOutcome, StorageError> {
        validate_permit_owner(permit, &req.owner)?;
        verbs::goal_write::decompose_goal_atomic(
            &self.pool,
            &self.sidecars,
            req,
            permit,
            self.store_number,
        )
        .await
    }
}

#[async_trait::async_trait]
impl GoalReadPort for PgStorage {
    async fn list_active_goals(
        &self,
        owner_scope: Option<&proxiima_core::OwnerScope>,
        read_owners: &[OwnerId],
        self_perspective_memory_id: MemoryId,
        limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result = async {
            verbs::active_goals::list_active_goals_on_connection(
                &mut tx,
                read_owners,
                self_perspective_memory_id,
                limit,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn load_goal_evidence(
        &self,
        owner_scope: Option<&proxiima_core::OwnerScope>,
        owner: &OwnerId,
        goal_id: proxiima_core::GoalId,
    ) -> Result<Option<Vec<MemoryId>>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result = async {
            verbs::goal_reads::load_goal_evidence_on_connection(&mut tx, owner, goal_id).await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}
