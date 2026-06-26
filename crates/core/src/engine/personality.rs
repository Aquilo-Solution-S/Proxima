use super::{Engine, MemoryPermit};
use crate::authz::{AuthzContext, MemoryAction, Role};
use crate::error::ProtocolError;
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedCaller,
    PersonalityConfigChangedSubject, PersonalityConfigChangedV1, PersonalityConfigChangedVerb,
};
use crate::personality::{
    InstantiatePersonalityRequest, InstantiatePersonalityResponse, PersonalityInstanceRow,
    TombstonePersonalityRequest, TombstonePersonalityResponse, WakeEntryAuthoredBy,
    WakeEntryGoalScope,
};
use crate::storage::StorageError;
use crate::verbs::event_ingest::{Citation, CitationMappingHint, CitedObjectHint, EventDraft};
use crate::{
    ListReadScopeRequest, MemoryId, Principal, SchemaId, SchemaVersion, SetReadScopeRequest,
    SetReadScopeResponse, SetWakeEntriesRequest, SetWakeEntriesResponse, SourceBatchId,
    WakeEntriesMutator, WakeEntryDraft,
};

#[derive(Debug, Clone)]
pub struct PersonalityConfigChangedInput {
    pub caller_self_perspective: MemoryId,
    pub is_master_token: bool,
    pub verb: PersonalityConfigChangedVerb,
    pub subject: PersonalityConfigChangedSubject,
    pub before: Option<PersonalityConfigChangeSnapshot>,
    pub after: Option<PersonalityConfigChangeSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonalityConfigAuditEmit {
    Ok,
    Failed { reason: String },
}

#[derive(Debug, Clone)]
pub struct AddWakeEntryRequest {
    pub principal: Principal,
    pub personality_instance_id: crate::PersonalityInstanceId,
    pub entry: WakeEntryDraft,
    pub audit: Option<PersonalityConfigChangedInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddWakeEntryResponse {
    pub wake_entry_id: uuid::Uuid,
    pub audit_emit: PersonalityConfigAuditEmit,
}

#[derive(Debug, Clone)]
pub struct UpdateWakeEntryRequest {
    pub principal: Principal,
    pub wake_entry_id: uuid::Uuid,
    pub patch: WakeEntryPatchInput,
    pub audit: Option<PersonalityConfigChangedInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateWakeEntryResponse {
    pub wake_entry_id: uuid::Uuid,
    pub audit_emit: PersonalityConfigAuditEmit,
}

#[derive(Debug, Clone)]
pub struct RemoveWakeEntryRequest {
    pub principal: Principal,
    pub wake_entry_id: uuid::Uuid,
    pub audit: Option<PersonalityConfigChangedInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveWakeEntryResponse {
    pub removed: bool,
    pub audit_emit: PersonalityConfigAuditEmit,
}

#[derive(Debug, Clone)]
pub struct SetReadScopeAdminRequest {
    pub principal: Principal,
    pub reader_personality_instance_id: crate::PersonalityInstanceId,
    pub readable_personality_instance_ids: Vec<crate::PersonalityInstanceId>,
    pub audit: Option<PersonalityConfigChangedInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetReadScopeAdminResponse {
    pub response: SetReadScopeResponse,
    pub readable_personality_instance_ids: Vec<crate::PersonalityInstanceId>,
    pub audit_emit: PersonalityConfigAuditEmit,
}

#[derive(Debug, Clone)]
pub struct WakeEntryPatchInput {
    pub label: Option<String>,
    pub enabled: Option<bool>,
    pub instructions: Option<String>,
    pub probability_promille: Option<u16>,
    pub authored_by: Option<WakeEntryAuthoredBy>,
    pub goal_scope: Option<WakeEntryGoalScope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetWakeEntriesAdminResponse {
    pub response: SetWakeEntriesResponse,
    pub audit_emit: PersonalityConfigAuditEmit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstonePersonalityAdminResponse {
    pub response: TombstonePersonalityResponse,
    pub audit_emit: PersonalityConfigAuditEmit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstantiatePersonalityAdminResponse {
    pub response: InstantiatePersonalityResponse,
    pub audit_emit: PersonalityConfigAuditEmit,
}

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

    /// Tombstone one personality and emit the config-change audit under
    /// the same admin permit.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal`
    /// or lacks admin authority; storage errors map as in
    /// [`Self::tombstone_personality`].
    pub async fn tombstone_personality_with_audit(
        &self,
        authz: &AuthzContext,
        req: TombstonePersonalityRequest,
        audit: Option<PersonalityConfigChangedInput>,
    ) -> Result<TombstonePersonalityAdminResponse, ProtocolError> {
        let permit =
            self.authorize_request(authz, &req.principal, Role::Admin, MemoryAction::Admin)?;
        let mut effective = req;
        effective.principal = permit.owner().clone();
        let before = self
            .personality_snapshot(permit.owner(), effective.personality_instance_id)
            .await?;
        let response = self
            .storage
            .tombstone_personality(&effective)
            .await
            .map_err(|e| match e {
                StorageError::NotFound => ProtocolError::not_found(format!(
                    "personality instance not found: {}",
                    effective.personality_instance_id.into_inner()
                )),
                other => ProtocolError::internal(format!("tombstone_personality: {other}")),
            })?;
        let audit = audit.map(|mut input| {
            input.before = before;
            input.after = None;
            input
        });
        let audit_emit = self.emit_audit_status(&permit, audit).await;
        Ok(TombstonePersonalityAdminResponse {
            response,
            audit_emit,
        })
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

    /// Instantiate one personality and emit the config-change audit under
    /// the same admin permit.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal`
    /// or lacks admin authority; storage errors map as in
    /// [`Self::instantiate_personality`].
    pub async fn instantiate_personality_with_audit(
        &self,
        authz: &AuthzContext,
        req: InstantiatePersonalityRequest,
        audit: Option<PersonalityConfigChangedInput>,
    ) -> Result<InstantiatePersonalityAdminResponse, ProtocolError> {
        let display_name = req.display_name.trim().to_string();
        let permit =
            self.authorize_request(authz, &req.principal, Role::Admin, MemoryAction::Admin)?;
        let response = self
            .instantiate_personality_authorized(&permit, req)
            .await?;
        let after = Some(PersonalityConfigChangeSnapshot::Personality {
            personality_instance_id: Some(response.instance_id.into_inner()),
            display_name: Some(display_name),
            status: None,
            wake_entry_count: None,
        });
        let audit = audit.map(|mut input| {
            input.before = None;
            input.after = after;
            input.subject =
                PersonalityConfigChangedSubject::Personality(response.instance_id.into_inner());
            input
        });
        let audit_emit = self.emit_audit_status(&permit, audit).await;
        Ok(InstantiatePersonalityAdminResponse {
            response,
            audit_emit,
        })
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

    /// Replace all wake entries and emit the config-change audit under
    /// the same admin permit.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal`
    /// or lacks admin authority; storage errors map as in
    /// [`Self::set_wake_entries`].
    pub async fn set_wake_entries_with_audit(
        &self,
        authz: &AuthzContext,
        req: &SetWakeEntriesRequest,
        audit: Option<PersonalityConfigChangedInput>,
    ) -> Result<SetWakeEntriesAdminResponse, ProtocolError> {
        let permit =
            self.authorize_request(authz, &req.principal, Role::Admin, MemoryAction::Admin)?;
        let before = self
            .wake_entries_snapshot(permit.owner(), req.personality_instance_id)
            .await?;
        let response = self.set_wake_entries_authorized(&permit, req).await?;
        let after = Some(PersonalityConfigChangeSnapshot::WakeEntries {
            wake_entry_count: req.entries.len(),
            wake_entry_ids: req
                .entries
                .iter()
                .map(|entry| entry.wake_entry_id)
                .collect(),
        });
        let audit = audit.map(|mut input| {
            input.before = before;
            input.after = after;
            input
        });
        let audit_emit = self.emit_audit_status(&permit, audit).await;
        Ok(SetWakeEntriesAdminResponse {
            response,
            audit_emit,
        })
    }

    /// Append one wake entry and emit the config-change audit under the
    /// same admin permit.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal`
    /// or lacks admin authority; `NotFound` when the personality is absent;
    /// `TriggerConflict` or `Internal` from storage.
    pub async fn add_wake_entry(
        &self,
        authz: &AuthzContext,
        req: &AddWakeEntryRequest,
    ) -> Result<AddWakeEntryResponse, ProtocolError> {
        let permit =
            self.authorize_request(authz, &req.principal, Role::Admin, MemoryAction::Admin)?;
        let new_id = req.entry.wake_entry_id;
        let new_draft = req.entry.clone();
        let new_trigger_kind = new_draft.trigger_kind;
        let new_trigger_id = new_draft.trigger_id.clone();
        let mutator: WakeEntriesMutator = Box::new(move |current| {
            if current.iter().any(|entry| {
                entry.trigger_kind == new_trigger_kind && entry.trigger_id == new_trigger_id
            }) {
                return Err(format!(
                    "wake entry with trigger ({new_trigger_kind:?}, {new_trigger_id}) already exists"
                ));
            }
            let mut next = current.to_vec();
            next.push(new_draft);
            crate::personality::validate_wake_entries_detect_config(&next)
                .map_err(|err| err.to_string())?;
            Ok(next)
        });
        self.storage
            .set_wake_entries_within(permit.owner(), req.personality_instance_id, mutator)
            .await
            .map_err(|err| map_granular_wake_storage_err(err, std::slice::from_ref(&req.entry)))?;
        let audit_emit = self.emit_audit_status(&permit, req.audit.clone()).await;
        Ok(AddWakeEntryResponse {
            wake_entry_id: new_id,
            audit_emit,
        })
    }

    /// Patch one wake entry and emit the config-change audit under the
    /// same admin permit.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal`
    /// or lacks admin authority; `NotFound` when the wake entry is absent;
    /// `TriggerConflict` or `Internal` from storage.
    pub async fn update_wake_entry(
        &self,
        authz: &AuthzContext,
        req: &UpdateWakeEntryRequest,
    ) -> Result<UpdateWakeEntryResponse, ProtocolError> {
        let permit =
            self.authorize_request(authz, &req.principal, Role::Admin, MemoryAction::Admin)?;
        let pid = self
            .personality_for_wake_entry(permit.owner(), req.wake_entry_id)
            .await?;
        let wid = req.wake_entry_id;
        let patch = req.patch.clone();
        let mutator: WakeEntriesMutator = Box::new(move |current| {
            let mut next = current.to_vec();
            let entry = next
                .iter_mut()
                .find(|entry| entry.wake_entry_id == wid)
                .ok_or_else(|| format!("wake entry {wid} no longer present"))?;
            apply_wake_entry_patch(entry, patch);
            crate::personality::validate_wake_entries_detect_config(&next)
                .map_err(|err| err.to_string())?;
            Ok(next)
        });
        self.storage
            .set_wake_entries_within(permit.owner(), pid, mutator)
            .await
            .map_err(|err| map_granular_wake_storage_err(err, &[]))?;
        let audit_emit = self.emit_audit_status(&permit, req.audit.clone()).await;
        Ok(UpdateWakeEntryResponse {
            wake_entry_id: wid,
            audit_emit,
        })
    }

    /// Remove one wake entry and emit the config-change audit under the
    /// same admin permit when a row was removed.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal`
    /// or lacks admin authority; `TriggerConflict` or `Internal` from storage.
    pub async fn remove_wake_entry(
        &self,
        authz: &AuthzContext,
        req: &RemoveWakeEntryRequest,
    ) -> Result<RemoveWakeEntryResponse, ProtocolError> {
        let permit =
            self.authorize_request(authz, &req.principal, Role::Admin, MemoryAction::Admin)?;
        let Some(pid) = self
            .find_personality_for_wake_entry(permit.owner(), req.wake_entry_id)
            .await?
        else {
            return Ok(RemoveWakeEntryResponse {
                removed: false,
                audit_emit: PersonalityConfigAuditEmit::Ok,
            });
        };
        let wid = req.wake_entry_id;
        let mutator: WakeEntriesMutator = Box::new(move |current| {
            Ok(current
                .iter()
                .filter(|entry| entry.wake_entry_id != wid)
                .cloned()
                .collect())
        });
        self.storage
            .set_wake_entries_within(permit.owner(), pid, mutator)
            .await
            .map_err(|err| map_granular_wake_storage_err(err, &[]))?;
        let audit_emit = self.emit_audit_status(&permit, req.audit.clone()).await;
        Ok(RemoveWakeEntryResponse {
            removed: true,
            audit_emit,
        })
    }

    /// Replace explicit read-scope grants and emit the config-change audit
    /// under the same admin permit.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal`
    /// or lacks admin authority; storage errors map to `Internal`.
    pub async fn set_read_scope(
        &self,
        authz: &AuthzContext,
        req: &SetReadScopeAdminRequest,
    ) -> Result<SetReadScopeAdminResponse, ProtocolError> {
        let permit =
            self.authorize_request(authz, &req.principal, Role::Admin, MemoryAction::Admin)?;
        let before = self
            .storage
            .list_read_scope(&ListReadScopeRequest {
                principal: permit.owner().clone(),
                reader_personality_instance_id: req.reader_personality_instance_id,
            })
            .await
            .ok()
            .map(|response| PersonalityConfigChangeSnapshot::ReadScope {
                readable_personality_instance_ids: response
                    .readable_personality_instance_ids
                    .into_iter()
                    .map(crate::PersonalityInstanceId::into_inner)
                    .collect(),
            });
        let response = self
            .storage
            .set_read_scope(&SetReadScopeRequest {
                principal: permit.owner().clone(),
                reader_personality_instance_id: req.reader_personality_instance_id,
                readable_personality_instance_ids: req.readable_personality_instance_ids.clone(),
            })
            .await
            .map_err(|err| ProtocolError::internal(format!("set_read_scope: {err}")))?;
        let after = Some(PersonalityConfigChangeSnapshot::ReadScope {
            readable_personality_instance_ids: req
                .readable_personality_instance_ids
                .iter()
                .copied()
                .filter(|id| *id != req.reader_personality_instance_id)
                .map(crate::PersonalityInstanceId::into_inner)
                .collect(),
        });
        let audit = req.audit.clone().map(|mut input| {
            input.before = before;
            input.after = after;
            input
        });
        let audit_emit = self.emit_audit_status(&permit, audit).await;
        Ok(SetReadScopeAdminResponse {
            response,
            readable_personality_instance_ids: req.readable_personality_instance_ids.clone(),
            audit_emit,
        })
    }

    async fn emit_audit_status(
        &self,
        permit: &MemoryPermit,
        audit: Option<PersonalityConfigChangedInput>,
    ) -> PersonalityConfigAuditEmit {
        let Some(audit) = audit else {
            return PersonalityConfigAuditEmit::Ok;
        };
        match self.emit_personality_config_changed(permit, audit).await {
            Ok(()) => PersonalityConfigAuditEmit::Ok,
            Err(err) => PersonalityConfigAuditEmit::Failed {
                reason: err.to_string(),
            },
        }
    }

    async fn emit_personality_config_changed(
        &self,
        permit: &MemoryPermit,
        audit: PersonalityConfigChangedInput,
    ) -> Result<(), ProtocolError> {
        let caller = self
            .resolve_personality_config_changed_caller(
                permit.owner(),
                audit.caller_self_perspective,
                audit.is_master_token,
            )
            .await?;
        let payload = PersonalityConfigChangedV1 {
            verb: audit.verb,
            before: audit.before,
            after: audit.after,
            subject: audit.subject,
            caller,
        };
        let observed_at = time::OffsetDateTime::now_utc();
        let mut draft = EventDraft::from_payload(
            permit.owner(),
            "core/mcp-crud",
            SourceBatchId::new(uuid::Uuid::now_v7()),
            &payload,
            observed_at,
        );
        let body_hash = blake3::hash(&draft.payload);
        draft = draft.with_citation(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("core/personality_config_changed_object_v1".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: *body_hash.as_bytes(),
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("core/personality_config_changed_whole_v1".into()),
                schema_version: SchemaVersion::new(1),
            },
        });
        let embedding_client = self.embed_client();
        let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
        self.storage()
            .ingest_event_atomic(&draft, embedding_model_id)
            .await
            .map_err(|err| {
                ProtocolError::internal(format!("ingest personality config audit: {err}"))
            })?;
        Ok(())
    }

    async fn resolve_personality_config_changed_caller(
        &self,
        owner: &crate::Owner,
        caller_self_perspective: MemoryId,
        is_master_token: bool,
    ) -> Result<PersonalityConfigChangedCaller, ProtocolError> {
        let instances = self
            .storage()
            .list_personality_instances(owner, false)
            .await
            .map_err(|err| ProtocolError::internal(format!("list_personality_instances: {err}")))?;
        let instance_id = instances
            .into_iter()
            .find(|row| row.current_root_perspective_memory_id == caller_self_perspective)
            .map(|row| row.personality_instance_id.into_inner())
            .ok_or_else(|| {
                ProtocolError::internal(format!(
                    "no personality matches caller_self_perspective {caller_self_perspective:?}"
                ))
            })?;
        Ok(if is_master_token {
            PersonalityConfigChangedCaller::MasterToken {
                personality_instance_id: instance_id,
            }
        } else {
            PersonalityConfigChangedCaller::WakePersonality {
                personality_instance_id: instance_id,
            }
        })
    }

    async fn wake_entries_snapshot(
        &self,
        owner: &crate::Owner,
        personality_instance_id: crate::PersonalityInstanceId,
    ) -> Result<Option<PersonalityConfigChangeSnapshot>, ProtocolError> {
        let rows = self
            .storage
            .list_personality_instances(owner, true)
            .await
            .map_err(|err| ProtocolError::internal(format!("list_personality_instances: {err}")))?;
        Ok(rows
            .iter()
            .find(|row| row.personality_instance_id == personality_instance_id)
            .map(|row| PersonalityConfigChangeSnapshot::WakeEntries {
                wake_entry_count: row.wake_entries.len(),
                wake_entry_ids: row
                    .wake_entries
                    .iter()
                    .map(|entry| entry.wake_entry_id)
                    .collect(),
            }))
    }

    async fn personality_snapshot(
        &self,
        owner: &crate::Owner,
        personality_instance_id: crate::PersonalityInstanceId,
    ) -> Result<Option<PersonalityConfigChangeSnapshot>, ProtocolError> {
        let rows = self
            .storage
            .list_personality_instances(owner, true)
            .await
            .map_err(|err| ProtocolError::internal(format!("list_personality_instances: {err}")))?;
        Ok(rows
            .iter()
            .find(|row| row.personality_instance_id == personality_instance_id)
            .map(|row| PersonalityConfigChangeSnapshot::Personality {
                personality_instance_id: Some(row.personality_instance_id.into_inner()),
                display_name: Some(row.display_name.clone()),
                status: Some(row.status.as_str().to_string()),
                wake_entry_count: Some(row.wake_entries.len()),
            }))
    }

    async fn personality_for_wake_entry(
        &self,
        owner: &crate::Owner,
        wake_entry_id: uuid::Uuid,
    ) -> Result<crate::PersonalityInstanceId, ProtocolError> {
        self.find_personality_for_wake_entry(owner, wake_entry_id)
            .await?
            .ok_or_else(|| {
                ProtocolError::not_found(format!("wake entry {wake_entry_id} not found for owner"))
            })
    }

    async fn find_personality_for_wake_entry(
        &self,
        owner: &crate::Owner,
        wake_entry_id: uuid::Uuid,
    ) -> Result<Option<crate::PersonalityInstanceId>, ProtocolError> {
        let rows = self
            .storage
            .list_personality_instances(owner, true)
            .await
            .map_err(|err| ProtocolError::internal(format!("list_personality_instances: {err}")))?;
        Ok(rows
            .iter()
            .find(|row| {
                row.wake_entries
                    .iter()
                    .any(|entry| entry.wake_entry_id == wake_entry_id)
            })
            .map(|row| row.personality_instance_id))
    }
}

fn apply_wake_entry_patch(entry: &mut WakeEntryDraft, patch: WakeEntryPatchInput) {
    if let Some(value) = patch.label {
        entry.label = value;
    }
    if let Some(value) = patch.enabled {
        entry.enabled = value;
    }
    if let Some(value) = patch.instructions {
        entry.instructions = value;
    }
    if let Some(value) = patch.probability_promille {
        entry.probability_promille = value;
    }
    if let Some(value) = patch.authored_by {
        entry.authored_by = value;
    }
    if let Some(value) = patch.goal_scope {
        entry.goal_scope = value;
    }
}

fn map_granular_wake_storage_err(err: StorageError, entries: &[WakeEntryDraft]) -> ProtocolError {
    super::map_set_wake_entries_storage_err(err, entries)
}

#[cfg(test)]
mod tests {
    use crate::authz::AuthzContext;
    use crate::error::ErrorCode;
    use crate::personality::{WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryTriggerKind};
    use crate::{
        Engine, FlavorRegistry, InstantiatePersonalityRequest, Principal, SetWakeEntriesRequest,
        TombstonePersonalityRequest, UserId,
    };

    use super::{
        AddWakeEntryRequest, RemoveWakeEntryRequest, SetReadScopeAdminRequest,
        UpdateWakeEntryRequest, WakeEntryPatchInput,
    };

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze())
    }

    fn owner() -> Principal {
        Principal::User(UserId::new(uuid::Uuid::now_v7()))
    }

    fn personality_id() -> crate::PersonalityInstanceId {
        crate::PersonalityInstanceId::new(uuid::Uuid::now_v7())
    }

    fn wake_entry_draft(personality_instance_id: crate::PersonalityInstanceId) -> WakeEntryDraft {
        WakeEntryDraft::new(
            uuid::Uuid::now_v7(),
            personality_instance_id,
            WakeEntryTriggerKind::OnMemory,
            "core/personality_config_changed_v1",
            "audit",
            WakeEntryAuthoredBy::Any,
            1000,
        )
        .expect("valid wake entry")
    }

    fn assert_forbidden(err: &crate::ProtocolError) {
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn add_wake_entry_denies_denied_context() {
        let owner = owner();
        let pid = personality_id();
        let req = AddWakeEntryRequest {
            principal: owner.clone(),
            personality_instance_id: pid,
            entry: wake_entry_draft(pid),
            audit: None,
        };
        let err = engine()
            .add_wake_entry(&AuthzContext::denied(&owner), &req)
            .await
            .expect_err("denied context must fail before storage");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn update_wake_entry_denies_denied_context() {
        let owner = owner();
        let req = UpdateWakeEntryRequest {
            principal: owner.clone(),
            wake_entry_id: uuid::Uuid::now_v7(),
            patch: WakeEntryPatchInput {
                label: Some("new label".into()),
                enabled: None,
                instructions: None,
                probability_promille: None,
                authored_by: None,
                goal_scope: None,
            },
            audit: None,
        };
        let err = engine()
            .update_wake_entry(&AuthzContext::denied(&owner), &req)
            .await
            .expect_err("denied context must fail before storage");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn remove_wake_entry_denies_denied_context() {
        let owner = owner();
        let req = RemoveWakeEntryRequest {
            principal: owner.clone(),
            wake_entry_id: uuid::Uuid::now_v7(),
            audit: None,
        };
        let err = engine()
            .remove_wake_entry(&AuthzContext::denied(&owner), &req)
            .await
            .expect_err("denied context must fail before storage");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn set_read_scope_denies_denied_context() {
        let owner = owner();
        let req = SetReadScopeAdminRequest {
            principal: owner.clone(),
            reader_personality_instance_id: personality_id(),
            readable_personality_instance_ids: vec![personality_id()],
            audit: None,
        };
        let err = engine()
            .set_read_scope(&AuthzContext::denied(&owner), &req)
            .await
            .expect_err("denied context must fail before storage");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn set_wake_entries_with_audit_denies_denied_context() {
        let owner = owner();
        let pid = personality_id();
        let req = SetWakeEntriesRequest {
            principal: owner.clone(),
            personality_instance_id: pid,
            entries: vec![wake_entry_draft(pid)],
        };
        let err = engine()
            .set_wake_entries_with_audit(&AuthzContext::denied(&owner), &req, None)
            .await
            .expect_err("denied context must fail before storage");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn tombstone_personality_with_audit_denies_denied_context() {
        let owner = owner();
        let req = TombstonePersonalityRequest {
            principal: owner.clone(),
            personality_instance_id: personality_id(),
        };
        let err = engine()
            .tombstone_personality_with_audit(&AuthzContext::denied(&owner), req, None)
            .await
            .expect_err("denied context must fail before storage");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn instantiate_personality_with_audit_denies_denied_context() {
        let owner = owner();
        let req = InstantiatePersonalityRequest {
            principal: owner.clone(),
            display_name: "Engineer".into(),
        };
        let err = engine()
            .instantiate_personality_with_audit(&AuthzContext::denied(&owner), req, None)
            .await
            .expect_err("denied context must fail before storage");
        assert_forbidden(&err);
    }
}
