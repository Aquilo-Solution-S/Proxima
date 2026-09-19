use proxima_core::owner_inverse::{
    EraseAuthorization, ExportAuthorization, OwnerEraseOutcome, OwnerExportBundle,
};
use proxima_core::storage_ports::OwnerInversePort;
use proxima_core::{GroupId, SourceId, StorageError, UserId};

use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl OwnerInversePort for PgStorage {
    async fn erase_group_owner(
        &self,
        auth: &EraseAuthorization,
        group_id: GroupId,
        tables: &proxima_core::owner_inverse::OwnerSurfaces,
    ) -> Result<OwnerEraseOutcome, StorageError> {
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        if let Some(lifecycle) = lifecycle {
            verbs::owner_erase::erase_group_owner_with_lifecycle(
                &self.pool,
                self.cold.as_ref(),
                auth,
                group_id,
                tables,
                Some(lifecycle),
            )
            .await
        } else {
            verbs::owner_erase::erase_group_owner(
                &self.pool,
                self.cold.as_ref(),
                auth,
                group_id,
                tables,
            )
            .await
        }
    }

    async fn erase_personal_owner(
        &self,
        auth: &EraseAuthorization,
        user_id: UserId,
        tables: &proxima_core::owner_inverse::OwnerSurfaces,
    ) -> Result<OwnerEraseOutcome, StorageError> {
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        if let Some(lifecycle) = lifecycle {
            verbs::owner_erase::erase_personal_owner_with_lifecycle(
                &self.pool,
                self.cold.as_ref(),
                auth,
                user_id,
                tables,
                Some(lifecycle),
            )
            .await
        } else {
            verbs::owner_erase::erase_personal_owner(
                &self.pool,
                self.cold.as_ref(),
                auth,
                user_id,
                tables,
            )
            .await
        }
    }

    async fn erase_group_source_scope(
        &self,
        auth: &EraseAuthorization,
        group_id: GroupId,
        source_id: &SourceId,
        tables: &proxima_core::owner_inverse::OwnerSurfaces,
    ) -> Result<OwnerEraseOutcome, StorageError> {
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        if let Some(lifecycle) = lifecycle {
            verbs::owner_erase::erase_group_source_scope_with_lifecycle(
                &self.pool,
                self.cold.as_ref(),
                auth,
                group_id,
                source_id,
                tables,
                Some(lifecycle),
            )
            .await
        } else {
            verbs::owner_erase::erase_group_source_scope(
                &self.pool,
                self.cold.as_ref(),
                auth,
                group_id,
                source_id,
                tables,
            )
            .await
        }
    }

    async fn erase_personal_source_scope(
        &self,
        auth: &EraseAuthorization,
        user_id: UserId,
        source_id: &SourceId,
        tables: &proxima_core::owner_inverse::OwnerSurfaces,
    ) -> Result<OwnerEraseOutcome, StorageError> {
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        if let Some(lifecycle) = lifecycle {
            verbs::owner_erase::erase_personal_source_scope_with_lifecycle(
                &self.pool,
                self.cold.as_ref(),
                auth,
                user_id,
                source_id,
                tables,
                Some(lifecycle),
            )
            .await
        } else {
            verbs::owner_erase::erase_personal_source_scope(
                &self.pool,
                self.cold.as_ref(),
                auth,
                user_id,
                source_id,
                tables,
            )
            .await
        }
    }

    async fn export_owner_bundle(
        &self,
        auth: &ExportAuthorization,
        tables: &proxima_core::owner_inverse::OwnerSurfaces,
    ) -> Result<OwnerExportBundle, StorageError> {
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        verbs::owner_export::export_owner_bundle(&self.pool, auth, tables, lifecycle).await
    }
}
