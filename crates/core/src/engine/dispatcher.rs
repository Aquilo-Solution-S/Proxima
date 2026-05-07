use super::Engine;
use crate::error::ProtocolError;
use crate::personality::{
    InstantiatePersonalityRequest, InstantiatePersonalityResponse, PersonalityInstanceRow,
    TombstonePersonalityRequest, TombstonePersonalityResponse,
};
use crate::storage::StorageError;
use crate::verbs::schema::PayloadKind;
use crate::{Owner, SchemaVersion};

impl Engine {
    pub async fn list_personality_instances(
        &self,
        owner: &Owner,
        personality_type_id: Option<&str>,
        include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, ProtocolError> {
        self.storage
            .list_personality_instances(owner, personality_type_id, include_tombstoned)
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
                    "personality instance not found: {}/{}",
                    req.personality_type_id,
                    req.personality_instance_id.into_inner()
                )),
                other => ProtocolError::internal(format!("tombstone_personality: {other}")),
            })
    }

    pub async fn provision_owner(&self, owner: &Owner) -> Result<(), ProtocolError> {
        for personality in self.registry.list_personalities() {
            let existing = self
                .storage
                .list_personality_instances(owner, Some(personality.personality_type_id()), true)
                .await
                .map_err(|e| ProtocolError::internal(format!("list_personality_instances: {e}")))?;
            if existing.is_empty() {
                self.instantiate_personality(InstantiatePersonalityRequest {
                    owner: owner.clone(),
                    personality_type_id: personality.personality_type_id().to_string(),
                    payload_overrides: None,
                })
                .await?;
            }
        }
        Ok(())
    }

    pub async fn instantiate_personality(
        &self,
        req: InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, ProtocolError> {
        let personality = self
            .registry
            .list_personalities()
            .iter()
            .find(|p| p.personality_type_id() == req.personality_type_id)
            .ok_or_else(|| {
                ProtocolError::not_found(format!(
                    "personality type not registered: {}",
                    req.personality_type_id
                ))
            })?;
        let self_schema = personality.self_schema();
        let self_info = self
            .registry
            .lookup(&self_schema, SchemaVersion::new(1))
            .filter(|s| s.kind == PayloadKind::Perspective)
            .ok_or_else(|| {
                ProtocolError::internal(format!(
                    "personality {} self_schema {} is not a registered Perspective",
                    personality.personality_type_id(),
                    self_schema.as_str()
                ))
            })?;
        let self_sidecar = self_info.sidecar_table.as_deref().ok_or_else(|| {
            ProtocolError::internal(format!(
                "personality {} self_schema {} has no sidecar",
                personality.personality_type_id(),
                self_schema.as_str()
            ))
        })?;
        let self_draft = personality
            .default_self_payload(&req.owner, req.payload_overrides.as_ref())
            .map_err(|e| ProtocolError::internal(format!("default_self_payload: {}", e.message)))?;
        self.registry
            .validate_payload(
                &self_draft.schema_id,
                self_draft.schema_version,
                PayloadKind::Perspective,
                &self_draft.typed_payload,
            )
            .map_err(|e| ProtocolError::internal(format!("invalid self payload: {e}")))?;

        self.storage
            .instantiate_personality(&req, &self_draft, self_sidecar)
            .await
            .map_err(|e| ProtocolError::internal(format!("instantiate_personality: {e}")))
    }

    pub async fn run_dispatcher_tick(&self) -> Result<usize, ProtocolError> {
        crate::wake::dispatch::dispatch_tick(self).await
    }
}
