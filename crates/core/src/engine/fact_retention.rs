use super::{Engine, pipeline::WritePermit};
use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::compliance::ComplianceEraseTarget;
use crate::error::ProtocolError;
use crate::owner::Owner;

fn compliance_target_for_owner(owner: &Owner) -> ComplianceEraseTarget {
    match *owner {
        crate::OwnerRef::Personal(user_id) => ComplianceEraseTarget::PersonalOwner {
            user_id,
            drop_event_id: String::new(),
        },
        crate::OwnerRef::Group(group_id) => ComplianceEraseTarget::GroupOwner { group_id },
    }
}

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
            .upsert_fact_retention(permit.owner_write_permit(), seconds)
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
            .clear_fact_retention(permit.owner_write_permit())
            .await
            .map_err(|e| ProtocolError::internal(format!("clear_fact_retention: {e}")))
    }

    /// Set an owner-scoped legal/security hold.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller lacks compliance-controller
    /// authority, and `Internal` for storage failures.
    pub async fn set_legal_hold(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
    ) -> Result<(), ProtocolError> {
        let target = compliance_target_for_owner(owner);
        if !self.compliance_controller_authorized(authz, &target).await {
            return Err(ProtocolError::forbidden(
                "compliance controller authorization required",
            ));
        }
        let permit = self.authorize_write(authz, owner, Relation::Admin).await?;
        self.storage
            .fact_retention
            .fact_retention
            .set_legal_hold(permit.owner_write_permit())
            .await
            .map_err(|e| ProtocolError::internal(format!("set_legal_hold: {e}")))
    }

    /// Read whether an owner-scoped legal/security hold is active.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `owner` or
    /// lacks `Admin`, and `Internal` for storage failures.
    pub async fn get_legal_hold(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
    ) -> Result<bool, ProtocolError> {
        let permit = self.authorize_write(authz, owner, Relation::Admin).await?;
        self.storage
            .fact_retention
            .fact_retention
            .get_legal_hold(permit.owner())
            .await
            .map_err(|e| ProtocolError::internal(format!("get_legal_hold: {e}")))
    }

    /// Clear an owner-scoped legal/security hold.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller lacks compliance-controller
    /// authority, and `Internal` for storage failures.
    pub async fn clear_legal_hold(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
    ) -> Result<bool, ProtocolError> {
        let target = compliance_target_for_owner(owner);
        if !self.compliance_controller_authorized(authz, &target).await {
            return Err(ProtocolError::forbidden(
                "compliance controller authorization required",
            ));
        }
        let permit = self.authorize_write(authz, owner, Relation::Admin).await?;
        self.storage
            .fact_retention
            .fact_retention
            .clear_legal_hold(permit.owner_write_permit())
            .await
            .map_err(|e| ProtocolError::internal(format!("clear_legal_hold: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::Role;
    use crate::error::ErrorCode;
    use crate::{AuthPath, AuthzContext, FlavorRegistry, OwnerRef, UserId};
    use uuid::Uuid;

    fn fresh_personal_owner() -> (UserId, Owner) {
        let user = UserId::new(Uuid::now_v7());
        (user, OwnerRef::Personal(user))
    }

    fn boot_engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
    }

    #[tokio::test]
    async fn legal_hold_rejects_owner_the_context_cannot_access() {
        let (_principal, owner) = fresh_personal_owner();
        let (_stranger, stranger_owner) = fresh_personal_owner();
        let engine = boot_engine();
        let stranger = AuthzContext::single_owner(&stranger_owner, AuthPath::HostBearer);

        let err = engine
            .set_legal_hold(&stranger, &owner)
            .await
            .expect_err("non-operator hold set must be rejected");
        assert_eq!(err.code, ErrorCode::Forbidden);

        let err = engine
            .get_legal_hold(&stranger, &owner)
            .await
            .expect_err("foreign owner must be rejected");
        assert_eq!(err.code, ErrorCode::Forbidden);

        let err = engine
            .clear_legal_hold(&stranger, &owner)
            .await
            .expect_err("non-operator hold clear must be rejected");
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn legal_hold_rejects_context_without_admin_role() {
        let (subject, _personal_owner) = fresh_personal_owner();
        let owner = OwnerRef::Group(crate::GroupId::new(Uuid::now_v7()));
        let engine = boot_engine();
        let authz = AuthzContext::for_subject_with_role(
            subject,
            [(owner, Role::viewer())],
            AuthPath::HostBearer,
        );

        let err = engine
            .set_legal_hold(&authz, &owner)
            .await
            .expect_err("missing admin role must be rejected");
        assert_eq!(err.code, ErrorCode::Forbidden);

        let err = engine
            .get_legal_hold(&authz, &owner)
            .await
            .expect_err("missing admin role must be rejected");
        assert_eq!(err.code, ErrorCode::Forbidden);

        let err = engine
            .clear_legal_hold(&authz, &owner)
            .await
            .expect_err("missing admin role must be rejected");
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn legal_hold_system_context_without_authority_is_denied_before_storage() {
        let (_subject, owner) = fresh_personal_owner();
        let engine = boot_engine();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let err = engine
            .set_legal_hold(&authz, &owner)
            .await
            .expect_err("System write without runtime authority must be rejected");
        assert_eq!(err.code, ErrorCode::Forbidden);

        let err = engine
            .get_legal_hold(&authz, &owner)
            .await
            .expect_err("System write without runtime authority must be rejected");
        assert_eq!(err.code, ErrorCode::Forbidden);

        let err = engine
            .clear_legal_hold(&authz, &owner)
            .await
            .expect_err("System write without runtime authority must be rejected");
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn legal_hold_owner_admin_can_get_but_cannot_set_or_clear() {
        let subject = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(crate::GroupId::new(Uuid::now_v7()));
        let engine = boot_engine();
        let owner_admin = AuthzContext::for_subject_with_role(
            subject,
            [(owner, Role::admin())],
            AuthPath::HostBearer,
        );

        let err = engine
            .set_legal_hold(&owner_admin, &owner)
            .await
            .expect_err("owner admin without operator authority cannot set hold");
        assert_eq!(err.code, ErrorCode::Forbidden);

        let err = engine
            .get_legal_hold(&owner_admin, &owner)
            .await
            .expect_err("owner admin get reaches RejectingStorage");
        assert_eq!(err.code, ErrorCode::Internal);

        let err = engine
            .clear_legal_hold(&owner_admin, &owner)
            .await
            .expect_err("owner admin without operator authority cannot clear hold");
        assert_eq!(err.code, ErrorCode::Forbidden);
    }
}
