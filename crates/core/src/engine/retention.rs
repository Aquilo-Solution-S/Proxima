//! The `retention` rows (docs/data-and-erasure.md): set by whoever may erase
//! that owner's memory by hand, read for every owner the caller may read,
//! and read whole by the host's clock.

use super::Engine;
use crate::authz::{AuthzContext, SystemAuthority};
use crate::error::ProtocolError;
use crate::{KeepDays, OwnerId, Retention, SchemaId};

impl Engine {
    /// Set `owner`'s retention for `schema` to `keep_days`; `None` removes
    /// the row, so the schema keeps for good.
    ///
    /// The owner-admin gate `erase_own_series` takes: a person for their own
    /// rows, a group's admin for the group's. An editor, viewer or ingest
    /// member is refused.
    ///
    /// # Errors
    /// `Forbidden` without owner admin on `owner`; `Internal` for storage
    /// faults.
    pub async fn set_retention(
        &self,
        authz: &AuthzContext,
        owner: &OwnerId,
        schema: &SchemaId,
        keep_days: Option<KeepDays>,
    ) -> Result<(), ProtocolError> {
        let permit = self.authorize_owner_admin(authz, owner).await?;
        self.storage
            .retention
            .retention
            .set_retention(permit.owner_write_permit(), schema, keep_days)
            .await
            .map_err(|e| ProtocolError::internal(format!("set_retention: {e}")))
    }

    /// The rows of every owner the caller may read: its own, and each group
    /// it belongs to, subject to the host's read vetoes.
    ///
    /// # Errors
    /// `Forbidden` when the context authorizes no reads; `Internal` for
    /// storage faults.
    pub async fn read_retention(
        &self,
        authz: &AuthzContext,
    ) -> Result<Vec<Retention>, ProtocolError> {
        let owners = self.authorize_read(authz).await?;
        if owners.is_empty() {
            return Ok(Vec::new());
        }
        self.storage
            .retention
            .retention
            .owner_retention(authz.owner_scope(), &owners)
            .await
            .map_err(|e| ProtocolError::internal(format!("read_retention: {e}")))
    }

    /// Every row of every owner, for the host's retention clock.
    ///
    /// # Errors
    /// `Forbidden` when `authority` belongs to another engine boot;
    /// `Internal` for storage faults.
    pub async fn all_retention(
        &self,
        authority: &SystemAuthority,
    ) -> Result<Vec<Retention>, ProtocolError> {
        if !authority.authorizes(&self.system_authority_binding) {
            return Err(ProtocolError::forbidden(
                "system authority belongs to a different engine boot",
            ));
        }
        self.storage
            .retention
            .retention
            .all_retention()
            .await
            .map_err(|e| ProtocolError::internal(format!("all_retention: {e}")))
    }
}
