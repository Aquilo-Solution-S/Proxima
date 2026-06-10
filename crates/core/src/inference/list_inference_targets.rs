use crate::error::ProtocolError;
use crate::storage::Storage;
use crate::{InferenceTargetRow, Owner};

/// List the owner's registered inference targets.
///
/// # Errors
///
/// Returns `ProtocolError::Internal` when the storage read fails.
pub async fn list_inference_targets(
    storage: &dyn Storage,
    owner: &Owner,
) -> Result<Vec<InferenceTargetRow>, ProtocolError> {
    storage
        .list_inference_targets(owner)
        .await
        .map_err(|err| ProtocolError::internal(err.to_string()))
}
