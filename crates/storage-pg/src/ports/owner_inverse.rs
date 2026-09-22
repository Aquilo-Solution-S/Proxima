use proxima_core::owner_inverse::{
    EraseAuthorization, ExportAuthorization, OwnerEraseOutcome, OwnerEraseTarget, OwnerExportBundle,
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
        if !matches!(auth.audit().target(), OwnerEraseTarget::GroupOwner { group_id: authorized } if *authorized == group_id)
        {
            return Err(StorageError::ConstraintViolation(
                "erase target differs from sealed authorization".into(),
            ));
        }
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        verbs::owner_erase::erase_group_owner_with_lifecycle(
            &self.pool,
            self.platform_scope.as_ref(),
            self.cold.as_ref(),
            auth,
            group_id,
            tables,
            lifecycle,
        )
        .await
    }

    async fn erase_personal_owner(
        &self,
        auth: &EraseAuthorization,
        user_id: UserId,
        tables: &proxima_core::owner_inverse::OwnerSurfaces,
    ) -> Result<OwnerEraseOutcome, StorageError> {
        if !matches!(auth.audit().target(), OwnerEraseTarget::PersonalOwner { user_id: authorized, .. } if *authorized == user_id)
        {
            return Err(StorageError::ConstraintViolation(
                "erase target differs from sealed authorization".into(),
            ));
        }
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        verbs::owner_erase::erase_personal_owner_with_lifecycle(
            &self.pool,
            self.platform_scope.as_ref(),
            self.cold.as_ref(),
            auth,
            user_id,
            tables,
            lifecycle,
        )
        .await
    }

    async fn erase_group_source_scope(
        &self,
        auth: &EraseAuthorization,
        group_id: GroupId,
        source_id: &SourceId,
        tables: &proxima_core::owner_inverse::OwnerSurfaces,
    ) -> Result<OwnerEraseOutcome, StorageError> {
        if !matches!(auth.audit().target(), OwnerEraseTarget::GroupSourceScope { group_id: authorized, source_id: source } if *authorized == group_id && source == source_id)
        {
            return Err(StorageError::ConstraintViolation(
                "erase target differs from sealed authorization".into(),
            ));
        }
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        verbs::owner_erase::erase_group_source_scope_with_lifecycle(
            &self.pool,
            self.platform_scope.as_ref(),
            self.cold.as_ref(),
            auth,
            source_id,
            tables,
            lifecycle,
        )
        .await
    }

    async fn erase_personal_source_scope(
        &self,
        auth: &EraseAuthorization,
        user_id: UserId,
        source_id: &SourceId,
        tables: &proxima_core::owner_inverse::OwnerSurfaces,
    ) -> Result<OwnerEraseOutcome, StorageError> {
        if !matches!(auth.audit().target(), OwnerEraseTarget::PersonalSourceScope { user_id: authorized, source_id: source, .. } if *authorized == user_id && source == source_id)
        {
            return Err(StorageError::ConstraintViolation(
                "erase target differs from sealed authorization".into(),
            ));
        }
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        verbs::owner_erase::erase_personal_source_scope_with_lifecycle(
            &self.pool,
            self.platform_scope.as_ref(),
            self.cold.as_ref(),
            auth,
            source_id,
            tables,
            lifecycle,
        )
        .await
    }

    async fn export_owner_bundle(
        &self,
        auth: &ExportAuthorization,
        tables: &proxima_core::owner_inverse::OwnerSurfaces,
    ) -> Result<OwnerExportBundle, StorageError> {
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        verbs::owner_export::export_owner_bundle(
            &self.pool,
            self.platform_scope.as_ref(),
            auth,
            tables,
            lifecycle,
        )
        .await
    }
}
