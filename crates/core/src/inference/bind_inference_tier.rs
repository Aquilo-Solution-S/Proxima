use crate::error::ProtocolError;
use crate::storage::{Storage, StorageError};
use crate::{BindInferenceTierRequest, BindInferenceTierResponse};

/// Bind a model tier to an inference target ref.
///
/// # Errors
///
/// Returns `ProtocolError::InvalidArgument` when `target_ref` is empty,
/// `InferenceTargetMissing` when the ref does not name a registered
/// target, and `Internal` for other storage failures.
pub async fn bind_inference_tier(
    storage: &dyn Storage,
    req: &BindInferenceTierRequest,
) -> Result<BindInferenceTierResponse, ProtocolError> {
    if req.target_ref.trim().is_empty() {
        return Err(ProtocolError::invalid_argument(
            "target_ref",
            "must be non-empty",
        ));
    }

    storage
        .bind_inference_tier(req)
        .await
        .map_err(|err| match err {
            StorageError::ConstraintViolation(_) | StorageError::NotFound => {
                ProtocolError::inference_target_missing(&req.target_ref)
            }
            other => ProtocolError::internal(other.to_string()),
        })
}
