use crate::read_models::ChangeEventForWake;
use crate::storage::StorageError;
use crate::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};
use crate::{Owner, OwnerRef};

#[async_trait::async_trait]
pub trait ChangeEventPort: Send + Sync {
    async fn change_history(
        &self,
        read_owners: &[OwnerRef],
        req: &ChangeHistoryRequest,
    ) -> Result<ChangeHistoryResponse, StorageError>;

    async fn list_change_events_after(
        &self,
        read_owners: &[OwnerRef],
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError>;

    async fn list_change_events_for_replay(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        until: Option<uuid::Uuid>,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        let rows = self
            .list_change_events_after(std::slice::from_ref(owner), after, limit)
            .await?;
        Ok(rows
            .into_iter()
            .filter(|row| until.is_none_or(|until| row.event.seq <= until))
            .collect())
    }
}
