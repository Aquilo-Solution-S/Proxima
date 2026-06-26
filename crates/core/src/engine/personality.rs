use super::{Engine, MemoryPermit};
use crate::Principal;
use crate::authz::{AuthzContext, MemoryAction, Role};
use crate::error::ProtocolError;
use crate::personality::{
    InstantiatePersonalityRequest, InstantiatePersonalityResponse, PersonalityInstanceRow,
    TombstonePersonalityRequest, TombstonePersonalityResponse,
};
use crate::storage::StorageError;

impl Engine {
    /// # Errors
    ///
    /// Returns `ProtocolError::Internal` when storage operations fail.
    pub async fn list_personality_instances(
        &self,
        authz: &AuthzContext,
        principal: &Principal,
        include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, ProtocolError> {
        let permit = self.authorize_request(authz, principal, Role::Admin, MemoryAction::Admin)?;
        self.list_personality_instances_authorized(&permit, principal, include_tombstoned)
            .await
    }

    async fn list_personality_instances_authorized(
        &self,
        permit: &MemoryPermit,
        _principal: &Principal,
        include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, ProtocolError> {
        self.storage
            .list_personality_instances(permit.owner(), include_tombstoned)
            .await
            .map_err(|e| ProtocolError::internal(format!("list_personality_instances: {e}")))
    }

    /// # Errors
    ///
    /// Returns `ProtocolError::NotFound` when the personality instance
    /// does not exist, or `ProtocolError::Internal` for other storage
    /// errors.
    pub async fn tombstone_personality(
        &self,
        authz: &AuthzContext,
        req: TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, ProtocolError> {
        let permit =
            self.authorize_request(authz, &req.principal, Role::Admin, MemoryAction::Admin)?;
        self.tombstone_personality_authorized(&permit, req).await
    }

    async fn tombstone_personality_authorized(
        &self,
        permit: &MemoryPermit,
        req: TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, ProtocolError> {
        let mut effective = req;
        effective.principal = permit.owner().clone();
        self.storage
            .tombstone_personality(&effective)
            .await
            .map_err(|e| match e {
                StorageError::NotFound => ProtocolError::not_found(format!(
                    "personality instance not found: {}",
                    effective.personality_instance_id.into_inner()
                )),
                other => ProtocolError::internal(format!("tombstone_personality: {other}")),
            })
    }

    /// # Errors
    ///
    /// Returns `ProtocolError::InvalidArgument` when `display_name` is
    /// empty, or `ProtocolError::Internal` when storage operations fail.
    pub async fn instantiate_personality(
        &self,
        authz: &AuthzContext,
        req: InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, ProtocolError> {
        let permit =
            self.authorize_request(authz, &req.principal, Role::Admin, MemoryAction::Admin)?;
        self.instantiate_personality_authorized(&permit, req).await
    }

    async fn instantiate_personality_authorized(
        &self,
        permit: &MemoryPermit,
        req: InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, ProtocolError> {
        if req.display_name.trim().is_empty() {
            return Err(ProtocolError::invalid_argument(
                "display_name",
                "must not be empty",
            ));
        }
        let mut effective = req;
        effective.principal = permit.owner().clone();
        self.storage
            .instantiate_personality(&effective)
            .await
            .map_err(|e| ProtocolError::internal(format!("instantiate_personality: {e}")))
    }
}
