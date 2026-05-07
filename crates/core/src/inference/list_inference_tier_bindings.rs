use crate::error::ProtocolError;
use crate::storage::Storage;
use crate::{InferenceTierBindingRow, Owner};

pub async fn list_inference_tier_bindings(
    storage: &dyn Storage,
    owner: &Owner,
) -> Result<Vec<InferenceTierBindingRow>, ProtocolError> {
    storage
        .list_inference_tier_bindings(owner)
        .await
        .map_err(|err| ProtocolError::internal(err.to_string()))
}
