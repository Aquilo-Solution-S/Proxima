use proxima_core::owner_inverse::{
    EraseAuthorization, ExportAuthorization, OwnerEraseOutcome, OwnerEraseTarget, OwnerExportBundle,
};
use proxima_core::storage_ports::OwnerInversePort;
use proxima_core::{GroupId, OwnerId, SourceId, StorageError, UserId};

use crate::{PgStorage, verbs};

/// The refusal when the port is asked to erase something other than what `auth` is sealed for.
fn sealed_target(matched: bool) -> Result<(), StorageError> {
    if matched {
        return Ok(());
    }
    Err(StorageError::ConstraintViolation(
        "erase target differs from sealed authorization".into(),
    ))
}

#[async_trait::async_trait]
impl OwnerInversePort for PgStorage {
    async fn erase_group_owner(
        &self,
        auth: &EraseAuthorization,
        group_id: GroupId,
        tables: &proxima_core::owner_inverse::OwnerSurfaces,
    ) -> Result<OwnerEraseOutcome, StorageError> {
        sealed_target(
            matches!(auth.audit().target(), OwnerEraseTarget::GroupOwner { group_id: authorized } if *authorized == group_id),
        )?;
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        verbs::owner_erase::erase_with_lifecycle(
            &self.pool,
            &self.platform_scope,
            self.cold.as_ref(),
            &self.vectors,
            auth,
            verbs::owner_erase::ErasePlan::owner(OwnerId::from(group_id)),
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
        sealed_target(
            matches!(auth.audit().target(), OwnerEraseTarget::PersonalOwner { user_id: authorized, .. } if *authorized == user_id),
        )?;
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        verbs::owner_erase::erase_with_lifecycle(
            &self.pool,
            &self.platform_scope,
            self.cold.as_ref(),
            &self.vectors,
            auth,
            verbs::owner_erase::ErasePlan::owner(OwnerId::from(user_id)),
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
        sealed_target(
            matches!(auth.audit().target(), OwnerEraseTarget::GroupSourceScope { group_id: authorized, source_id: source } if *authorized == group_id && source == source_id),
        )?;
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        verbs::owner_erase::erase_with_lifecycle(
            &self.pool,
            &self.platform_scope,
            self.cold.as_ref(),
            &self.vectors,
            auth,
            verbs::owner_erase::ErasePlan::source_scope(auth, source_id),
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
        sealed_target(
            matches!(auth.audit().target(), OwnerEraseTarget::PersonalSourceScope { user_id: authorized, source_id: source, .. } if *authorized == user_id && source == source_id),
        )?;
        let lifecycle = self.host_lifecycle_for_surfaces(tables)?;
        verbs::owner_erase::erase_with_lifecycle(
            &self.pool,
            &self.platform_scope,
            self.cold.as_ref(),
            &self.vectors,
            auth,
            verbs::owner_erase::ErasePlan::source_scope(auth, source_id),
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
            &self.platform_scope,
            auth,
            tables,
            lifecycle,
        )
        .await
    }
}
