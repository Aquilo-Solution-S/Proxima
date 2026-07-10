use crate::access::AccessError;
use crate::compliance::{ComplianceEraseTarget, ComplianceExportTarget};
use crate::storage::StorageError;
use crate::storage_ports::OwnerWritePermit;
use crate::{GroupId, Owner, SourceId, UserId};

#[async_trait::async_trait]
pub trait FactRetentionPort: Send + Sync {
    async fn upsert_fact_retention(
        &self,
        permit: &OwnerWritePermit,
        seconds: i64,
    ) -> Result<(), StorageError>;

    async fn get_fact_retention(&self, owner: &Owner) -> Result<Option<i64>, StorageError>;

    async fn clear_fact_retention(&self, permit: &OwnerWritePermit) -> Result<bool, StorageError>;

    async fn set_legal_hold(&self, permit: &OwnerWritePermit) -> Result<(), StorageError>;

    async fn get_legal_hold(&self, owner: &Owner) -> Result<bool, StorageError>;

    async fn clear_legal_hold(&self, permit: &OwnerWritePermit) -> Result<bool, StorageError>;
}

#[allow(clippy::too_many_arguments)]
#[async_trait::async_trait]
pub trait ComplianceErasePort: Send + Sync {
    async fn record_compliance_outcome(
        &self,
        audit: &crate::compliance::ComplianceAuditContext,
        outcome: &crate::compliance::ComplianceEraseOutcome,
    ) -> Result<(), StorageError>;

    /// `object_purge_planned` is true iff the engine has a cited-object erase
    /// port configured for this owner-scope erase. The verb persists
    /// `cited_object_purge_pending = object_purge_planned` on the audit row in
    /// the same transaction as the erase and echoes it back on the outcome, so
    /// the durable record never claims a clean erase while a planned purge is
    /// still outstanding.
    async fn erase_group_owner_if_abandoned(
        &self,
        auth: &crate::compliance::EraseAuthorization,
        group_id: GroupId,
        object_purge_planned: bool,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<crate::compliance::ComplianceEraseOutcome, StorageError>;

    /// See [`ComplianceErasePort::erase_group_owner_if_abandoned`] for the
    /// meaning of `object_purge_planned`.
    async fn erase_personal_owner_if_drop_verified(
        &self,
        auth: &crate::compliance::EraseAuthorization,
        user_id: UserId,
        object_purge_planned: bool,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<crate::compliance::ComplianceEraseOutcome, StorageError>;

    async fn erase_group_source_scope_if_owner_abandoned(
        &self,
        auth: &crate::compliance::EraseAuthorization,
        group_id: GroupId,
        source_id: &SourceId,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<crate::compliance::ComplianceEraseOutcome, StorageError>;

    async fn erase_personal_source_scope_if_drop_verified(
        &self,
        auth: &crate::compliance::EraseAuthorization,
        user_id: UserId,
        source_id: &SourceId,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<crate::compliance::ComplianceEraseOutcome, StorageError>;

    async fn export_owner_bundle(
        &self,
        auth: &crate::compliance::ExportAuthorization,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<crate::compliance::ComplianceExportBundle, StorageError>;

    /// Clear the durable purge-pending flag on one audit row after a
    /// cited-object purge has been confirmed to succeed. A single-statement
    /// `UPDATE … WHERE operation_id = $1`; never sets the flag, only clears
    /// it — a failed clear must leave the row over-reporting pending rather
    /// than silently losing the signal.
    async fn clear_cited_object_purge_pending(
        &self,
        operation_id: uuid::Uuid,
    ) -> Result<(), StorageError>;
}

/// Trusted host port for compliance erase authorization.
/// Fail-closed: absence or denial means erase is not authorized.

#[async_trait::async_trait]
pub trait ComplianceAdminPort: Send + Sync {
    async fn may_perform_compliance_erase(
        &self,
        authz: &crate::AuthzContext,
        target: &ComplianceEraseTarget,
    ) -> Result<bool, AccessError>;

    async fn may_perform_compliance_export(
        &self,
        authz: &crate::AuthzContext,
        target: &ComplianceExportTarget,
    ) -> Result<bool, AccessError> {
        self.may_perform_compliance_erase(authz, &target.erase_authority_target())
            .await
    }

    async fn may_perform_operator_maintenance(
        &self,
        _authz: &crate::AuthzContext,
    ) -> Result<bool, AccessError> {
        Ok(false)
    }
}

/// Trusted host port for personal owner drop verification.
/// Fail-closed: absence or denial means drop is not verified.

#[async_trait::async_trait]
pub trait OwnerDropProofPort: Send + Sync {
    async fn verify_personal_owner_dropped(
        &self,
        user_id: UserId,
        drop_event_id: &str,
    ) -> Result<bool, AccessError>;
}
