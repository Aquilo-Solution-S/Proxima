use crate::access::AccessError;
use crate::owner_inverse::{OwnerEraseTarget, OwnerExportTarget};
use crate::storage::StorageError;
use crate::{GroupId, SourceId, UserId};

/// The inverses of storing, at owner scope: destroy an owner's rows, or
/// serialize them.
///
/// Core performs these; it does not decide when they are owed. There is no
/// retention window here, no legal hold, no journal of who asked — v0.0.8
/// carried all three and they are gone. A hosting application that promises
/// its users a right to erasure or a right to a copy calls these verbs at
/// the moment its own rules say to, and records the receipt they hand back
/// if its own rules say to. Core's contribution is that the inverse is
/// COMPLETE and derivable from the declarations, which is the only part a
/// host cannot write for itself.
#[allow(clippy::too_many_arguments)]
#[async_trait::async_trait]
pub trait OwnerInversePort: Send + Sync {
    /// `object_purge_planned` is true iff the engine has a cited-object erase
    /// port configured for this owner-scope erase. It is echoed back on the
    /// outcome so the receipt never claims a clean erase while a planned
    /// purge is still outstanding — a fact about THIS operation, handed to
    /// the caller, not a row core keeps.
    async fn erase_group_owner(
        &self,
        auth: &crate::owner_inverse::EraseAuthorization,
        group_id: GroupId,
        object_purge_planned: bool,
        tables: &crate::owner_inverse::OwnerSurfaces,
    ) -> Result<crate::owner_inverse::OwnerEraseOutcome, StorageError>;

    /// See [`OwnerInversePort::erase_group_owner`] for the
    /// meaning of `object_purge_planned`.
    async fn erase_personal_owner(
        &self,
        auth: &crate::owner_inverse::EraseAuthorization,
        user_id: UserId,
        object_purge_planned: bool,
        tables: &crate::owner_inverse::OwnerSurfaces,
    ) -> Result<crate::owner_inverse::OwnerEraseOutcome, StorageError>;

    async fn erase_group_source_scope(
        &self,
        auth: &crate::owner_inverse::EraseAuthorization,
        group_id: GroupId,
        source_id: &SourceId,
        tables: &crate::owner_inverse::OwnerSurfaces,
    ) -> Result<crate::owner_inverse::OwnerEraseOutcome, StorageError>;

    async fn erase_personal_source_scope(
        &self,
        auth: &crate::owner_inverse::EraseAuthorization,
        user_id: UserId,
        source_id: &SourceId,
        tables: &crate::owner_inverse::OwnerSurfaces,
    ) -> Result<crate::owner_inverse::OwnerEraseOutcome, StorageError>;

    async fn export_owner_bundle(
        &self,
        auth: &crate::owner_inverse::ExportAuthorization,
        tables: &crate::owner_inverse::OwnerSurfaces,
    ) -> Result<crate::owner_inverse::OwnerExportBundle, StorageError>;
}

/// THE provider seam for "who may erase an owner".
///
/// Core has no answer and cannot acquire one. Whether a given caller may
/// destroy a given owner's rows is a fact about the hosting application's
/// tenancy, its contracts and its own legal position — a hosted control plane
/// answers it from a role, a single-tenant deployment from an operator
/// group, and a self-hosted install may answer "always". Wiring nothing is a
/// valid deployment and refuses every erase: fail-closed, because the
/// failure mode of guessing wrong is unrecoverable.
///
/// The port is deliberately narrow. It is asked a yes/no question about one
/// target and returns a yes/no answer; it is not consulted for a reason, a
/// deadline, or a policy, because core would then have to interpret one.
/// [`AuthPath::System`](crate::AuthPath::System) bypasses it — the
/// in-process operator path — and [`AuthPath::Delegated`](crate::AuthPath::Delegated) can never reach
/// it, since a delegated worker holds a user's authority and erasing an
/// owner is not among a user's powers.
///
/// Export shares the seam through [`Self::may_export_owner`], whose default
/// asks the erase question. Export is non-destructive, so a host that wants
/// a looser rule for portability than for erasure overrides it; the default
/// is the conservative one.
#[async_trait::async_trait]
pub trait OwnerEraseAuthorityPort: Send + Sync {
    async fn may_erase_owner(
        &self,
        authz: &crate::AuthzContext,
        target: &OwnerEraseTarget,
    ) -> Result<bool, AccessError>;

    async fn may_export_owner(
        &self,
        authz: &crate::AuthzContext,
        target: &OwnerExportTarget,
    ) -> Result<bool, AccessError> {
        self.may_erase_owner(authz, &target.erase_authority_target())
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
