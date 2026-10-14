use crate::read_models::ChangeEventForWake;
use crate::storage::StorageError;
use crate::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};
use crate::{OwnerId, OwnerScope};

#[async_trait::async_trait]
pub trait ChangeEventPort: Send + Sync {
    async fn change_history(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        req: &ChangeHistoryRequest,
    ) -> Result<ChangeHistoryResponse, StorageError>;

    async fn list_change_events_after(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError>;

    async fn list_change_events_for_replay(
        &self,
        owner_scope: Option<&OwnerScope>,
        owner: &OwnerId,
        after: uuid::Uuid,
        until: Option<uuid::Uuid>,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        let rows = self
            .list_change_events_after(owner_scope, std::slice::from_ref(owner), after, limit)
            .await?;
        Ok(rows
            .into_iter()
            .filter(|row| until.is_none_or(|until| row.event.seq <= until))
            .collect())
    }
}
