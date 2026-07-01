use crate::access::AccessError;
use crate::compliance::ComplianceEraseTarget;
use crate::storage::StorageError;
use crate::{GroupId, Owner, SourceId, UserId};

#[async_trait::async_trait]
pub trait FactRetentionPort: Send + Sync {
    async fn upsert_fact_retention(&self, owner: &Owner, seconds: i64) -> Result<(), StorageError>;

    async fn get_fact_retention(&self, owner: &Owner) -> Result<Option<i64>, StorageError>;

    async fn clear_fact_retention(&self, owner: &Owner) -> Result<bool, StorageError>;
}

#[allow(clippy::too_many_arguments)]
#[async_trait::async_trait]
pub trait ComplianceErasePort: Send + Sync {
    async fn record_compliance_outcome(
        &self,
        audit: &crate::compliance::ComplianceAuditContext,
        outcome: &crate::compliance::ComplianceEraseOutcome,
    ) -> Result<(), StorageError>;

    async fn erase_group_owner_if_abandoned(
        &self,
        auth: &crate::compliance::EraseAuthorization,
        group_id: GroupId,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<crate::compliance::ComplianceEraseOutcome, StorageError>;

    async fn erase_personal_owner_if_drop_verified(
        &self,
        auth: &crate::compliance::EraseAuthorization,
        user_id: UserId,
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
