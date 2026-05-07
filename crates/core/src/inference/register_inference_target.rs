use crate::error::ProtocolError;
use crate::storage::{Storage, StorageError};
use crate::{RegisterInferenceTargetRequest, RegisterInferenceTargetResponse};

pub async fn register_inference_target(
    storage: &dyn Storage,
    req: &RegisterInferenceTargetRequest,
) -> Result<RegisterInferenceTargetResponse, ProtocolError> {
    if req.target_ref.trim().is_empty() {
        return Err(ProtocolError::invalid_argument(
            "target_ref",
            "must be non-empty",
        ));
    }

    storage
        .register_inference_target(req)
        .await
        .map_err(|err| match err {
            StorageError::ConstraintViolation(_) => {
                ProtocolError::target_ref_conflict(&req.target_ref)
            }
            other => ProtocolError::internal(other.to_string()),
        })
}
