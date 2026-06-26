use super::Engine;
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
        super::authorize_action(authz, principal, Role::Admin, MemoryAction::Admin)?;
        let owner = authz.scoped_owner(principal.clone());
        self.storage
            .list_personality_instances(&owner, include_tombstoned)
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
        super::authorize_action(authz, &req.principal, Role::Admin, MemoryAction::Admin)?;
        self.storage
            .tombstone_personality(&req)
            .await
            .map_err(|e| match e {
                StorageError::NotFound => ProtocolError::not_found(format!(
                    "personality instance not found: {}",
                    req.personality_instance_id.into_inner()
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
        super::authorize_action(authz, &req.principal, Role::Admin, MemoryAction::Admin)?;
        if req.display_name.trim().is_empty() {
            return Err(ProtocolError::invalid_argument(
                "display_name",
                "must not be empty",
            ));
        }
        self.storage
            .instantiate_personality(&req)
            .await
            .map_err(|e| ProtocolError::internal(format!("instantiate_personality: {e}")))
    }
}
