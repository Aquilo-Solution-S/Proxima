use proxima_core::compliance::{
    ComplianceAuditContext, ComplianceEraseOutcome, ComplianceExportBundle, EraseAuthorization,
    ExportAuthorization,
};
use proxima_core::storage_ports::ComplianceErasePort;
use proxima_core::{GroupId, SourceId, StorageError, UserId};

use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl ComplianceErasePort for PgStorage {
    async fn record_compliance_outcome(
        &self,
        audit: &ComplianceAuditContext,
        outcome: &ComplianceEraseOutcome,
    ) -> Result<(), StorageError> {
        verbs::compliance_erase::record_compliance_outcome(&self.pool, audit, outcome).await
    }

    async fn erase_group_owner_if_abandoned(
        &self,
        auth: &EraseAuthorization,
        group_id: GroupId,
        object_purge_planned: bool,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<ComplianceEraseOutcome, StorageError> {
        verbs::compliance_erase::erase_group_owner_if_abandoned(
            &self.pool,
            self.cold.as_ref(),
            auth,
            group_id,
            object_purge_planned,
            fact_sidecar_tables,
            goal_sidecar_tables,
            citation_mapping_sidecar_tables,
            cited_object_sidecar_tables,
        )
        .await
    }

    async fn erase_personal_owner_if_drop_verified(
        &self,
        auth: &EraseAuthorization,
        user_id: UserId,
        object_purge_planned: bool,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<ComplianceEraseOutcome, StorageError> {
        verbs::compliance_erase::erase_personal_owner_if_drop_verified(
            &self.pool,
            self.cold.as_ref(),
            auth,
            user_id,
            object_purge_planned,
            fact_sidecar_tables,
            goal_sidecar_tables,
            citation_mapping_sidecar_tables,
            cited_object_sidecar_tables,
        )
        .await
    }

    async fn erase_group_source_scope_if_owner_abandoned(
        &self,
        auth: &EraseAuthorization,
        group_id: GroupId,
        source_id: &SourceId,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<ComplianceEraseOutcome, StorageError> {
        verbs::compliance_erase::erase_group_source_scope_if_owner_abandoned(
            &self.pool,
            self.cold.as_ref(),
            auth,
            group_id,
            source_id,
            fact_sidecar_tables,
            goal_sidecar_tables,
            citation_mapping_sidecar_tables,
            cited_object_sidecar_tables,
        )
        .await
    }

    async fn erase_personal_source_scope_if_drop_verified(
        &self,
        auth: &EraseAuthorization,
        user_id: UserId,
        source_id: &SourceId,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<ComplianceEraseOutcome, StorageError> {
        verbs::compliance_erase::erase_personal_source_scope_if_drop_verified(
            &self.pool,
            self.cold.as_ref(),
            auth,
            user_id,
            source_id,
            fact_sidecar_tables,
            goal_sidecar_tables,
            citation_mapping_sidecar_tables,
            cited_object_sidecar_tables,
        )
        .await
    }

    async fn export_owner_bundle(
        &self,
        auth: &ExportAuthorization,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<ComplianceExportBundle, StorageError> {
        verbs::compliance_export::export_owner_bundle(
            &self.pool,
            auth,
            fact_sidecar_tables,
            goal_sidecar_tables,
            citation_mapping_sidecar_tables,
            cited_object_sidecar_tables,
        )
        .await
    }

    async fn clear_cited_object_purge_pending(
        &self,
        operation_id: uuid::Uuid,
    ) -> Result<(), StorageError> {
        verbs::compliance_erase::clear_cited_object_purge_pending(&self.pool, operation_id).await
    }
}
