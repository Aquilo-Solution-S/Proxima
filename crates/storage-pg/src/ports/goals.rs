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
        verbs::goal_write::resolve_goal_command_replay(&self.pool, req).await
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
        read_owners: &[OwnerRef],
        self_perspective_memory_id: MemoryId,
        limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        verbs::active_goals::list_active_goals(
            &self.pool,
            read_owners,
            self_perspective_memory_id,
            limit,
        )
        .await
    }

    async fn load_goal_wake_configs(
        &self,
        read_owners: &[OwnerRef],
        goal_ids: &[proxima_core::GoalId],
    ) -> Result<Vec<proxima_core::read_models::GoalWakeConfigRow>, StorageError> {
        verbs::goal_wake_candidates::load_goal_wake_configs(&self.pool, read_owners, goal_ids).await
    }

    async fn load_goal_evidence(
        &self,
        owner: &OwnerRef,
        goal_id: proxima_core::GoalId,
    ) -> Result<Option<Vec<MemoryId>>, StorageError> {
        verbs::goal_reads::load_goal_evidence(&self.pool, owner, goal_id).await
    }
}

#[async_trait::async_trait]
impl GoalWakeCandidatePort for PgStorage {
    async fn list_goal_wake_candidates(
        &self,
        req: &GoalWakeCandidateRequest<'_>,
    ) -> Result<Vec<GoalWakeCandidate>, StorageError> {
        verbs::goal_wake_candidates::list_goal_wake_candidates(&self.pool, req).await
    }
}
