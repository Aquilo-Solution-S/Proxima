use crate::error::ProtocolError;
use crate::storage::{Storage, StorageError};
use crate::{RemoveInferenceTargetRequest, RemoveInferenceTargetResponse};

/// Remove an inference target by ref.
///
/// # Errors
///
/// Returns `ProtocolError::InvalidArgument` when `target_ref` is empty,
/// `TargetInUse` when bindings or wake entries still reference it, and
/// `Internal` for other storage failures.
pub async fn remove_inference_target(
    storage: &dyn Storage,
    req: &RemoveInferenceTargetRequest,
) -> Result<RemoveInferenceTargetResponse, ProtocolError> {
    if req.target_ref.trim().is_empty() {
        return Err(ProtocolError::invalid_argument(
            "target_ref",
            "must be non-empty",
        ));
    }

    storage
        .remove_inference_target(req)
        .await
        .map_err(|err| match err {
            StorageError::ConstraintViolation(msg) => {
                ProtocolError::target_in_use(&req.target_ref, &[msg])
            }
            other => ProtocolError::internal(other.to_string()),
        })
}
