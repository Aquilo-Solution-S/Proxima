use super::Engine;
use crate::authz::{AuthzContext, Role};
use crate::error::ProtocolError;
use crate::owner::Owner;

fn retention_seconds_to_i64(seconds: u64) -> Result<i64, ProtocolError> {
    if seconds == 0 {
        return Err(ProtocolError::invalid_argument(
            "retention_seconds",
            "must be greater than 0",
        ));
    }
    i64::try_from(seconds).map_err(|_| {
        ProtocolError::invalid_argument("retention_seconds", "must fit in signed 64-bit integer")
    })
}

impl Engine {
    /// Set the owner-scoped Fact retention duration.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `owner` or
    /// lacks `Admin`, `InvalidArgument` for a zero/out-of-range duration,
    /// and `Internal` for storage failures.
    pub async fn set_fact_retention(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
        seconds: u64,
    ) -> Result<(), ProtocolError> {
        super::authorize(authz, &owner.principal, Role::Admin)?;
        let seconds = retention_seconds_to_i64(seconds)?;
        let owner = authz.scoped_owner(owner.principal.clone());
        self.storage
            .upsert_fact_retention(&owner, seconds)
            .await
            .map_err(|e| ProtocolError::internal(format!("set_fact_retention: {e}")))
    }

    /// Read the owner-scoped Fact retention duration.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `owner` or
    /// lacks `Admin`, and `Internal` for storage failures.
    pub async fn get_fact_retention(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
    ) -> Result<Option<i64>, ProtocolError> {
        super::authorize(authz, &owner.principal, Role::Admin)?;
        let owner = authz.scoped_owner(owner.principal.clone());
        self.storage
            .get_fact_retention(&owner)
            .await
            .map_err(|e| ProtocolError::internal(format!("get_fact_retention: {e}")))
    }

    /// Clear the owner-scoped Fact retention duration.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `owner` or
    /// lacks `Admin`, and `Internal` for storage failures.
    pub async fn clear_fact_retention(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
    ) -> Result<bool, ProtocolError> {
        super::authorize(authz, &owner.principal, Role::Admin)?;
        let owner = authz.scoped_owner(owner.principal.clone());
        self.storage
            .clear_fact_retention(&owner)
            .await
            .map_err(|e| ProtocolError::internal(format!("clear_fact_retention: {e}")))
    }
}
