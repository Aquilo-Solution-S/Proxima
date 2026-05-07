use std::collections::HashMap;
use std::sync::Arc;

use super::Engine;
use super::agent_loop::{StopConditions, run_agent_loop};
use crate::error::ProtocolError;
use crate::outbox::{ChangeEventKind, EntityRef};
use crate::personality::{
    AuthorFilter, ChangeEventForWake, InstantiatePersonalityRequest,
    InstantiatePersonalityResponse, PersonalityInstanceRow, PersonalityRef, PersonalityTool,
    PersonalityToolContext, SetWakeConfigRequest, SetWakeConfigResponse, SidecarSpec,
    TombstonePersonalityRequest, TombstonePersonalityResponse, WakeConfigRow, WakeFilter,
    WakeInvocationStatus, WakeTarget, substrate_pack,
};
use crate::storage::StorageError;
use crate::verbs::schema::PayloadKind;
use crate::{MemoryId, Owner, SchemaId, SchemaVersion};

const DISPATCH_EVENT_BATCH: usize = 1000;
const MAX_INSTANCE_QUEUE_DEPTH: usize = 10;

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

        let mut filters = personality.default_wake_filters();
        filters.push(WakeFilter::on_self_inspires());
        self.storage
            .instantiate_personality(&req, &self_draft, self_sidecar, &filters)
            .await
            .map_err(|e| ProtocolError::internal(format!("instantiate_personality: {e}")))
    }

    pub async fn set_wake_config(
        &self,
        req: SetWakeConfigRequest,
    ) -> Result<SetWakeConfigResponse, ProtocolError> {
        self.validate_wake_filters(&req.wake_filters)?;
        self.storage
            .set_wake_config(&req)
            .await
            .map_err(|e| match e {
                StorageError::NotFound => ProtocolError::not_found(format!(
                    "personality instance not found or tombstoned: {}/{}",
                    req.personality_type_id,
                    req.personality_instance_id.into_inner()
                )),
                other => ProtocolError::internal(format!("set_wake_config: {other}")),
            })
    }

    pub async fn run_dispatcher_tick(&self) -> Result<usize, ProtocolError> {
        let configs = self
            .storage
            .list_active_wake_configs()
            .await
            .map_err(|e| ProtocolError::internal(format!("list_active_wake_configs: {e}")))?;
        let personality_by_type: HashMap<&str, _> = self
            .registry
            .list_personalities()
            .iter()
            .map(|p| (p.personality_type_id(), p.clone()))
            .collect();
        let mut fired = 0usize;

        for config in configs {
            let Some(personality) = personality_by_type.get(config.personality_type_id.as_str())
            else {
                tracing::warn!(
                    personality_type_id = config.personality_type_id,
                    "wake_config references unregistered personality type"
                );
                continue;
            };
            let filters: Vec<WakeFilter> =
                match serde_json::from_value(config.wake_filters_json.clone()) {
                    Ok(filters) => filters,
                    Err(e) => {
                        tracing::warn!(
                            personality_type_id = config.personality_type_id,
                            personality_instance_id = %config.personality_instance_id.into_inner(),
                            error = %e,
                            "wake_config failed strict deserialization; marking needs_repair"
                        );
                        let instance = PersonalityRef::new(
                            config.personality_type_id.clone(),
                            config.personality_instance_id,
                        );
                        self.storage
                            .mark_wake_config_needs_repair(&config.owner, &instance)
                            .await
                            .map_err(|err| {
                                ProtocolError::internal(format!(
                                    "mark_wake_config_needs_repair: {err}"
                                ))
                            })?;
                        continue;
                    }
                };
            self.validate_wake_filters(&filters)?;
            let events = self
                .storage
                .list_change_events_after(
                    &config.owner,
                    config.last_considered_seq,
                    DISPATCH_EVENT_BATCH,
                )
                .await
                .map_err(|e| ProtocolError::internal(format!("list_change_events_after: {e}")))?;
            let instance = PersonalityRef::new(
                config.personality_type_id.clone(),
                config.personality_instance_id,
            );
            let mut matched: Vec<ChangeEventForWake> = Vec::new();
            for event in events {
                if self.should_skip_self_or_depth(
                    &event,
                    &instance,
                    personality.max_wake_chain_depth(),
                ) {
                    self.advance_cursor(&config, event.event.seq).await?;
                    continue;
                }
                let matches = filters
                    .iter()
                    .enumerate()
                    .any(|(idx, filter)| filter_matches(filter, &config, &event, idx));
                if matches {
                    matched.push(event);
                } else {
                    self.advance_cursor(&config, event.event.seq).await?;
                }
            }

            if matched.len() > MAX_INSTANCE_QUEUE_DEPTH {
                let drop_count = matched.len() - MAX_INSTANCE_QUEUE_DEPTH;
                tracing::warn!(
                    personality_type_id = config.personality_type_id,
                    personality_instance_id = %config.personality_instance_id.into_inner(),
                    dropped = drop_count,
                    "wake queue overflow",
                );
                let dropped: Vec<_> = matched.drain(..drop_count).collect();
                for d in dropped {
                    self.advance_cursor(&config, d.event.seq).await?;
                }
            }

            if !matched.is_empty()
                && !self.llm_available_for_wake(&config.owner, personality.tier())
            {
                tracing::debug!(
                    personality_type_id = config.personality_type_id,
                    personality_instance_id = %config.personality_instance_id.into_inner(),
                    pending = matched.len(),
                    "deferring wake batch: no LLM configured for owner+tier"
                );
                continue;
            }

            for event in matched {
                let began = self
                    .storage
                    .try_begin_wake_invocation(&config.owner, &instance, event.event.seq)
                    .await
                    .map_err(|e| {
                        ProtocolError::internal(format!("try_begin_wake_invocation: {e}"))
                    })?;
                if !began {
                    self.advance_cursor(&config, event.event.seq).await?;
                    continue;
                }
                let outcome = self.run_wake(&config, &event).await;
                let (status, turn_count, cost_usd, wrote) = match outcome {
                    Ok((status, turn_count, cost_usd, wrote)) => {
                        if wrote {
                            fired += 1;
                        }
                        (status, turn_count, cost_usd, wrote)
                    }
                    Err(e) => {
                        tracing::warn!(
                            personality_type_id = config.personality_type_id,
                            personality_instance_id = %config.personality_instance_id.into_inner(),
                            seq = %event.event.seq,
                            error = %e.message,
                            "wake failed"
                        );
                        (WakeInvocationStatus::Failed, 0, 0.0, false)
                    }
                };
                let _ = wrote;
                self.storage
                    .finish_wake_invocation(
                        &config.owner,
                        &instance,
                        event.event.seq,
                        status,
                        turn_count,
                        cost_usd,
                    )
                    .await
                    .map_err(|e| ProtocolError::internal(format!("finish_wake_invocation: {e}")))?;
                self.advance_cursor(&config, event.event.seq).await?;
            }
        }
        Ok(fired)
    }

    async fn run_wake(
        &self,
        config: &WakeConfigRow,
        event: &ChangeEventForWake,
    ) -> Result<(WakeInvocationStatus, u16, f64, bool), ProtocolError> {
        let personality = self
            .registry
            .list_personalities()
            .iter()
            .find(|p| p.personality_type_id() == config.personality_type_id)
            .ok_or_else(|| ProtocolError::not_found("personality type not registered"))?
            .clone();
        let anthropic = self
            .anthropic()
            .expect("dispatcher invariant: llm_available_for_wake gate must precede run_wake");
        let instance = PersonalityRef::new(
            config.personality_type_id.clone(),
            config.personality_instance_id,
        );
        let _lock = self
            .storage
            .acquire_wake_lock(&config.owner, &instance)
            .await
            .map_err(|e| ProtocolError::internal(format!("acquire_wake_lock: {e}")))?;

        let triggering_memory = triggering_memory_id(&event.event);
        let triggering_event_memory_id =
            triggering_memory.unwrap_or(config.current_self_perspective_memory_id);
        let palette: Vec<Arc<dyn PersonalityTool>> = substrate_pack()
            .iter()
            .cloned()
            .chain(personality.tools().into_iter())
            .collect();
        let tool_ctx = PersonalityToolContext::new(
            self,
            &config.owner,
            personality.personality_type_id(),
            config.personality_instance_id,
            config.current_self_perspective_memory_id,
            triggering_event_memory_id,
            event.wake_chain_depth,
            personality.writeable_schemas(),
            personality.writeable_relations(),
            &palette,
        );
        let wake_context_json = build_wake_context_json(config, event, triggering_memory);

        let outcome = run_agent_loop(
            anthropic.as_ref(),
            personality.as_ref(),
            wake_context_json,
            &palette,
            &tool_ctx,
            StopConditions::default(),
        )
        .await?;
        let wrote = matches!(outcome.status, WakeInvocationStatus::Succeeded);
        Ok((outcome.status, outcome.turn_count, outcome.cost_usd, wrote))
    }

    fn validate_wake_filters(&self, filters: &[WakeFilter]) -> Result<(), ProtocolError> {
        for filter in filters {
            if filter.version() == 0 {
                return Err(ProtocolError::internal("wake filter version must be > 0"));
            }
            let p = filter.probability();
            if !(0.0..=1.0).contains(&p) {
                return Err(ProtocolError::internal(
                    "wake filter probability must be between 0 and 1",
                ));
            }
            if let WakeFilter::Custom { kind_id, .. } = filter
                && !self
                    .registry
                    .list_wake_filter_kinds()
                    .iter()
                    .any(|k| k.kind_id() == kind_id)
            {
                return Err(ProtocolError::internal(format!(
                    "unknown wake filter kind: {kind_id}"
                )));
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    fn sidecars_for(
        &self,
        kind: PayloadKind,
        predicate: impl Fn(&SchemaId) -> bool,
    ) -> Vec<SidecarSpec> {
        self.registry
            .list()
            .iter()
            .filter(|s| s.kind == kind && s.sidecar_table.is_some() && predicate(&s.schema_id))
            .map(|s| SidecarSpec {
                schema_id: s.schema_id.clone(),
                sidecar_table: s.sidecar_table.clone().unwrap(),
            })
            .collect()
    }

    fn should_skip_self_or_depth(
        &self,
        event: &ChangeEventForWake,
        instance: &PersonalityRef,
        max_depth: u16,
    ) -> bool {
        if event.authoring_personality_type_id.as_deref()
            == Some(instance.personality_type_id.as_str())
            && event.authoring_personality_instance_id == Some(instance.personality_instance_id)
        {
            return true;
        }
        event.wake_chain_depth.into_inner() >= max_depth
    }

    async fn advance_cursor(
        &self,
        config: &WakeConfigRow,
        seq: uuid::Uuid,
    ) -> Result<(), ProtocolError> {
        let instance = PersonalityRef::new(
            config.personality_type_id.clone(),
            config.personality_instance_id,
        );
        self.storage
            .advance_wake_cursor(&config.owner, &instance, seq)
            .await
            .map_err(|e| ProtocolError::internal(format!("advance_wake_cursor: {e}")))
    }
}

fn filter_matches(
    filter: &WakeFilter,
    config: &WakeConfigRow,
    event: &ChangeEventForWake,
    filter_index: usize,
) -> bool {
    let structural = match (filter, &event.event.kind) {
        (
            WakeFilter::OnMemory {
                schema_id,
                authored_by,
                ..
            },
            ChangeEventKind::EntityAppend {
                entity_kind,
                schema_id: event_schema,
                ..
            },
        ) if *entity_kind != crate::EntityKind::Goal => {
            schema_id == event_schema && author_matches(authored_by, event)
        }
        (
            WakeFilter::OnEdge {
                relation_id,
                source,
                target,
                ..
            },
            ChangeEventKind::EdgeAppend {
                relation,
                source: event_source,
                target: event_target,
                ..
            },
        ) => {
            relation_id == relation
                && wake_target_matches(source, event_source, config)
                && wake_target_matches(target, event_target, config)
        }
        (WakeFilter::Custom { .. }, _) => false,
        _ => false,
    };
    structural
        && probability_passes(
            event.event.seq,
            &config.personality_type_id,
            config.personality_instance_id,
            filter_index,
            filter.probability(),
        )
}

fn author_matches(filter: &AuthorFilter, event: &ChangeEventForWake) -> bool {
    match filter {
        AuthorFilter::Any => true,
        AuthorFilter::External => event.authoring_personality_type_id.is_none(),
        AuthorFilter::Personality {
            personality_type_id,
            personality_instance_id,
        } => {
            event.authoring_personality_type_id.as_deref() == Some(personality_type_id.as_str())
                && personality_instance_id
                    .map(|id| event.authoring_personality_instance_id == Some(id))
                    .unwrap_or(true)
        }
    }
}

fn wake_target_matches(target: &WakeTarget, event_ref: &EntityRef, config: &WakeConfigRow) -> bool {
    match target {
        WakeTarget::Any => true,
        WakeTarget::SelfPerspective => {
            *event_ref == EntityRef::Memory(config.current_self_perspective_memory_id)
        }
        WakeTarget::Memory { memory_id } => *event_ref == EntityRef::Memory(*memory_id),
        WakeTarget::Goal { goal_id } => *event_ref == EntityRef::Goal(*goal_id),
    }
}

fn build_wake_context_json(
    config: &WakeConfigRow,
    event: &ChangeEventForWake,
    triggering_memory: Option<MemoryId>,
) -> serde_json::Value {
    let kind = match &event.event.kind {
        ChangeEventKind::EntityAppend {
            entity_kind,
            schema_id,
            schema_version,
            entity,
            supersedes,
        } => serde_json::json!({
            "kind": "entity_append",
            "entity_kind": format!("{entity_kind:?}"),
            "schema_id": schema_id.as_str(),
            "schema_version": schema_version.into_inner(),
            "entity": entity_ref_json(entity),
            "supersedes": supersedes.as_ref().map(entity_ref_json),
        }),
        ChangeEventKind::EdgeAppend {
            relation,
            source,
            target,
            ..
        } => serde_json::json!({
            "kind": "edge_append",
            "relation": relation,
            "source": entity_ref_json(source),
            "target": entity_ref_json(target),
        }),
    };
    serde_json::json!({
        "personality_type_id": config.personality_type_id,
        "personality_instance_id": config.personality_instance_id.into_inner(),
        "current_self_perspective_memory_id":
            config.current_self_perspective_memory_id.into_inner(),
        "triggering_event_seq": event.event.seq,
        "triggering_memory_id": triggering_memory.map(|m| m.into_inner()),
        "triggering_event": kind,
        "wake_chain_depth": event.wake_chain_depth.into_inner(),
        "authoring_personality_type_id": event.authoring_personality_type_id,
        "authoring_personality_instance_id":
            event.authoring_personality_instance_id.map(|id| id.into_inner()),
    })
}

fn entity_ref_json(entity: &EntityRef) -> serde_json::Value {
    match entity {
        EntityRef::Memory(memory_id) => {
            serde_json::json!({"memory_id": memory_id.into_inner()})
        }
        EntityRef::Goal(goal_id) => serde_json::json!({"goal_id": goal_id.into_inner()}),
    }
}

fn triggering_memory_id(event: &crate::ChangeEvent) -> Option<MemoryId> {
    match &event.kind {
        ChangeEventKind::EntityAppend {
            entity: EntityRef::Memory(memory_id),
            ..
        } => Some(*memory_id),
        ChangeEventKind::EdgeAppend {
            source: EntityRef::Memory(memory_id),
            ..
        } => Some(*memory_id),
        _ => None,
    }
}

fn probability_passes(
    seq: uuid::Uuid,
    personality_type_id: &str,
    personality_instance_id: crate::PersonalityInstanceId,
    filter_index: usize,
    probability: f32,
) -> bool {
    if probability <= 0.0 {
        return false;
    }
    if probability >= 1.0 {
        return true;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(seq.as_bytes());
    hasher.update(personality_type_id.as_bytes());
    hasher.update(personality_instance_id.into_inner().as_bytes());
    hasher.update(&filter_index.to_be_bytes());
    let hash = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&hash.as_bytes()[0..8]);
    let sample = u64::from_be_bytes(bytes) as f64 / u64::MAX as f64;
    sample <= f64::from(probability)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probability_is_deterministic() {
        let seq = uuid::Uuid::now_v7();
        let instance = crate::PersonalityInstanceId::new(uuid::Uuid::now_v7());
        let first = probability_passes(seq, "test/personality", instance, 3, 0.5);
        let second = probability_passes(seq, "test/personality", instance, 3, 0.5);
        assert_eq!(first, second);
    }
}
