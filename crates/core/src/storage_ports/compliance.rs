use crate::access::AccessError;
use crate::compliance::{ComplianceEraseTarget, ComplianceExportTarget};
use crate::storage::StorageError;
use crate::{GroupId, SourceId, UserId};

#[allow(clippy::too_many_arguments)]
#[async_trait::async_trait]
pub trait ComplianceErasePort: Send + Sync {
    /// `object_purge_planned` is true iff the engine has a cited-object erase
    /// port configured for this owner-scope erase. It is echoed back on the
    /// outcome so the receipt never claims a clean erase while a planned
    /// purge is still outstanding — a fact about THIS operation, handed to
    /// the caller, not a row core keeps.
    async fn erase_group_owner_if_abandoned(
        &self,
        auth: &crate::compliance::EraseAuthorization,
        group_id: GroupId,
        object_purge_planned: bool,
        tables: &crate::compliance::OwnerSurfaces,
    ) -> Result<crate::compliance::ComplianceEraseOutcome, StorageError>;

    /// See [`ComplianceErasePort::erase_group_owner_if_abandoned`] for the
    /// meaning of `object_purge_planned`.
    async fn erase_personal_owner_if_drop_verified(
        &self,
        auth: &crate::compliance::EraseAuthorization,
        user_id: UserId,
        object_purge_planned: bool,
        tables: &crate::compliance::OwnerSurfaces,
    ) -> Result<crate::compliance::ComplianceEraseOutcome, StorageError>;

    async fn erase_group_source_scope_if_owner_abandoned(
        &self,
        auth: &crate::compliance::EraseAuthorization,
        group_id: GroupId,
        source_id: &SourceId,
        tables: &crate::compliance::OwnerSurfaces,
    ) -> Result<crate::compliance::ComplianceEraseOutcome, StorageError>;

    async fn erase_personal_source_scope_if_drop_verified(
        &self,
        auth: &crate::compliance::EraseAuthorization,
        user_id: UserId,
        source_id: &SourceId,
        tables: &crate::compliance::OwnerSurfaces,
    ) -> Result<crate::compliance::ComplianceEraseOutcome, StorageError>;

    async fn export_owner_bundle(
        &self,
        auth: &crate::compliance::ExportAuthorization,
        tables: &crate::compliance::OwnerSurfaces,
    ) -> Result<crate::compliance::ComplianceExportBundle, StorageError>;
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
