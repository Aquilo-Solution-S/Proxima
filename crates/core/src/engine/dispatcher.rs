use super::Engine;
use crate::Owner;
use crate::error::ProtocolError;
use crate::personality::{
    InstantiatePersonalityRequest, InstantiatePersonalityResponse, ListWakeInvocationsRequest,
    PersonalityInstanceRow, ReplayWakeEventsOutcome, ReplayWakeEventsRequest,
    TombstonePersonalityRequest, TombstonePersonalityResponse, WakeInvocationLogDraft,
    WakeInvocationRow,
};
use crate::storage::StorageError;

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

    pub async fn list_wake_invocations(
        &self,
        req: ListWakeInvocationsRequest,
    ) -> Result<Vec<WakeInvocationRow>, ProtocolError> {
        self.storage
            .list_wake_invocations(&req)
            .await
            .map_err(|e| ProtocolError::internal(format!("list_wake_invocations: {e}")))
    }

    pub async fn append_wake_invocation_log(
        &self,
        log: &WakeInvocationLogDraft,
    ) -> Result<(), ProtocolError> {
        self.storage
            .append_wake_invocation_log(log)
            .await
            .map_err(|e| ProtocolError::internal(format!("append_wake_invocation_log: {e}")))
    }

    pub async fn run_dispatcher_tick(&self) -> Result<usize, ProtocolError> {
        let _guard = self.dispatch_tick_lock.lock().await;
        crate::wake::dispatch::dispatch_tick(self).await
    }

    pub async fn replay_missed_wakes(
        &self,
        req: ReplayWakeEventsRequest,
    ) -> Result<ReplayWakeEventsOutcome, ProtocolError> {
        let _guard = self.dispatch_tick_lock.lock().await;
        crate::wake::dispatch::replay_missed_wakes(self, req).await
    }
}
