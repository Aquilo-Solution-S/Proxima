use proxima_core::read_models::{ActiveGoalSummary, GoalWakeCandidate, GoalWakeCandidateRequest};
use proxima_core::storage_ports::{
    GoalReadPort, GoalWakeCandidatePort, GoalWritePort, OwnerWritePermit,
};
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalReplayOutcome, GoalReplayRequest, GoalWriteOutcome,
    ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use proxima_core::{MemoryId, OwnerRef, StorageError};

use super::validate_permit_owner;
use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl GoalWritePort for PgStorage {
    async fn resolve_goal_replay(
        &self,
        req: GoalReplayRequest<'_, '_>,
        permit: &OwnerWritePermit,
    ) -> Result<Option<GoalReplayOutcome>, StorageError> {
        validate_permit_owner(permit, &req.owner())?;
        let mut tx = crate::owner_scope::begin_compatible_owner_transaction(
            &self.pool,
            permit.owner_scope(),
        )
        .await?;
        let result = verbs::goal_write::resolve_goal_command_replay_on(&mut tx, req).await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn create_goal_atomic(
        &self,
        req: &CreateGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        validate_permit_owner(permit, &req.draft.owner())?;
        verbs::goal_write::create_goal_atomic(&self.pool, &self.sidecars, req, permit).await
    }

    async fn transition_goal_atomic(
        &self,
        req: &TransitionGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        validate_permit_owner(permit, &req.owner)?;
        verbs::goal_write::transition_goal_atomic(&self.pool, &self.sidecars, req, permit).await
    }

    async fn achieve_goal_atomic(
        &self,
        req: &AchieveGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        validate_permit_owner(permit, &req.owner)?;
        verbs::goal_write::achieve_goal_atomic(&self.pool, &self.sidecars, req, permit).await
    }

    async fn modify_goal_atomic(
        &self,
        req: &ModifyGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        validate_permit_owner(permit, &req.owner)?;
        verbs::goal_write::modify_goal_atomic(&self.pool, &self.sidecars, req, permit).await
    }

    async fn decompose_goal_atomic(
        &self,
        req: &DecomposeGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<DecomposeGoalOutcome, StorageError> {
        validate_permit_owner(permit, &req.owner)?;
        verbs::goal_write::decompose_goal_atomic(&self.pool, &self.sidecars, req, permit).await
    }
}

#[async_trait::async_trait]
impl GoalReadPort for PgStorage {
    async fn list_active_goals(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        self_perspective_memory_id: MemoryId,
        limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
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

    async fn load_goal_wake_configs(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        goal_ids: &[proxima_core::GoalId],
    ) -> Result<Vec<proxima_core::read_models::GoalWakeConfigRow>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::goal_wake_candidates::load_goal_wake_configs_on_connection(
                &mut tx,
                read_owners,
                goal_ids,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn load_goal_evidence(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerRef,
        goal_id: proxima_core::GoalId,
    ) -> Result<Option<Vec<MemoryId>>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::goal_reads::load_goal_evidence_on_connection(&mut tx, owner, goal_id).await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}

#[async_trait::async_trait]
impl GoalWakeCandidatePort for PgStorage {
    async fn list_goal_wake_candidates(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        req: &GoalWakeCandidateRequest<'_>,
    ) -> Result<Vec<GoalWakeCandidate>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::goal_wake_candidates::list_goal_wake_candidates_on_connection(&mut tx, req).await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}
