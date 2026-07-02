use crate::OwnerRef;
use crate::read_models::{ActiveGoalSummary, GoalWakeCandidate, GoalWakeCandidateRequest};
use crate::storage::StorageError;
use crate::storage_ports::OwnerWritePermit;
use crate::verbs::goal_write::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalWriteOutcome, ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};

#[async_trait::async_trait]
pub trait GoalWritePort: Send + Sync {
    async fn create_goal_atomic(
        &self,
        req: &CreateGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError>;

    async fn transition_goal_atomic(
        &self,
        req: &TransitionGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError>;

    async fn achieve_goal_atomic(
        &self,
        req: &AchieveGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError>;

    async fn modify_goal_atomic(
        &self,
        req: &ModifyGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError>;

    async fn decompose_goal_atomic(
        &self,
        req: &DecomposeGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<DecomposeGoalOutcome, StorageError>;
}

#[async_trait::async_trait]
pub trait GoalReadPort: Send + Sync {
    async fn list_active_goals(
        &self,
        read_owners: &[OwnerRef],
        self_perspective_memory_id: crate::MemoryId,
        limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError>;
}

#[async_trait::async_trait]
pub trait GoalWakeCandidatePort: Send + Sync {
    async fn list_goal_wake_candidates(
        &self,
        req: &GoalWakeCandidateRequest<'_>,
    ) -> Result<Vec<GoalWakeCandidate>, StorageError>;
}
