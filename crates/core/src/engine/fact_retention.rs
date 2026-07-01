use super::{Engine, pipeline::WritePermit};
use crate::access::Relation;
use crate::authz::AuthzContext;
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
        let permit = self.authorize_write(authz, owner, Relation::Admin).await?;
        self.set_fact_retention_authorized(&permit, owner, seconds)
            .await
    }

    async fn set_fact_retention_authorized(
        &self,
        permit: &WritePermit,
        _owner: &Owner,
        seconds: u64,
    ) -> Result<(), ProtocolError> {
        let seconds = retention_seconds_to_i64(seconds)?;
        self.storage
            .fact_retention
            .fact_retention
            .upsert_fact_retention(permit.owner(), seconds)
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
        let permit = self.authorize_write(authz, owner, Relation::Admin).await?;
        self.get_fact_retention_authorized(&permit, owner).await
    }

    async fn get_fact_retention_authorized(
        &self,
        permit: &WritePermit,
        _owner: &Owner,
    ) -> Result<Option<i64>, ProtocolError> {
        self.storage
            .fact_retention
            .fact_retention
            .get_fact_retention(permit.owner())
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
        let permit = self.authorize_write(authz, owner, Relation::Admin).await?;
        self.clear_fact_retention_authorized(&permit, owner).await
    }

    async fn clear_fact_retention_authorized(
        &self,
        permit: &WritePermit,
        _owner: &Owner,
    ) -> Result<bool, ProtocolError> {
        self.storage
            .fact_retention
            .fact_retention
            .clear_fact_retention(permit.owner())
            .await
            .map_err(|e| ProtocolError::internal(format!("clear_fact_retention: {e}")))
    }
}
