use super::Engine;
use crate::error::ProtocolError;
use crate::personality::{
    InstantiatePersonalityRequest, InstantiatePersonalityResponse, PersonalityInstanceRow,
    TombstonePersonalityRequest, TombstonePersonalityResponse,
};
use crate::storage::StorageError;
use crate::Owner;

impl Engine {
    pub async fn list_personality_instances(
        &self,
        owner: &Owner,
        include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, ProtocolError> {
        self.storage
            .list_personality_instances(owner, include_tombstoned)
            .await
            .map_err(|e| ProtocolError::internal(format!("list_personality_instances: {e}")))
    }

    pub async fn tombstone_personality(
        &self,
        req: TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, ProtocolError> {
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

    pub async fn provision_owner(&self, _owner: &Owner) -> Result<(), ProtocolError> {
        // Auto-seeding removed: personalities are user-composed.
        // Step 5 either deletes this verb or replaces it with a flavor hook.
        Ok(())
    }

    pub async fn instantiate_personality(
        &self,
        req: InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, ProtocolError> {
        if req.display_name.trim().is_empty() {
            return Err(ProtocolError::invalid_argument(
                "display_name",
                "must not be empty",
            ));
        }
        if req.purpose.trim().is_empty() {
            return Err(ProtocolError::invalid_argument(
                "purpose",
                "must not be empty",
            ));
        }
        self.storage
            .instantiate_personality(&req)
            .await
            .map_err(|e| ProtocolError::internal(format!("instantiate_personality: {e}")))
    }

    pub async fn run_dispatcher_tick(&self) -> Result<usize, ProtocolError> {
        crate::wake::dispatch::dispatch_tick(self).await
    }
}
