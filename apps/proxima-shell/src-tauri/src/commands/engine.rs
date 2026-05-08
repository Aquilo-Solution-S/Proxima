use std::sync::Arc;

use futures_util::StreamExt;
use proxima_core::auth::Credentials;
use proxima_core::error::ProtocolError;
use proxima_core::verbs::event_history::{EventHistoryRequest, EventHistoryResponse};
use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use proxima_core::verbs::query::{QueryRequest, QueryResponse};
use proxima_core::verbs::schema::{SchemaRequest, SchemaResponse};
use proxima_core::verbs::subscribe::SubscribeRequest;
use proxima_core::{
    ChangeEvent, Engine, FlavorProvenance, Owner, PersonalityInstanceId, PersonalityInstanceRow,
};
use tauri::State;
use tauri::ipc::Channel;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InstantiatePersonalityTs {
    pub owner: Owner,
    pub display_name: String,
    pub purpose: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InstantiatePersonalityOutcomeTs {
    pub instance_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ListPersonalityInstancesTs {
    pub owner: Owner,
    #[serde(default)]
    pub include_tombstoned: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TombstonePersonalityTs {
    pub owner: Owner,
    pub personality_instance_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TombstonePersonalityOutcomeTs {
    pub status: String,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct PersonalityInstanceTs {
    pub owner: Owner,
    pub personality_instance_id: String,
    pub current_root_perspective_memory_id: String,
    pub display_name: String,
    pub status: String,
    pub wake_entries: Vec<WakeEntryTs>,
    pub flavor: Option<FlavorDescriptorTs>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct FlavorDescriptorTs {
    pub flavor_id: String,
    pub display_name: String,
    pub package_version: String,
    pub author: Option<String>,
    pub provenance: FlavorProvenanceTs,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlavorProvenanceTs {
    Builtin,
    Marketplace { source_url: String },
    Local { workspace_path: String },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKindTs {
    OnMemory,
    OnEdge,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum AuthoredByTs {
    Any,
    SelfAuthor,
    Other,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionModeTs {
    SubstrateOnly,
    Workspace,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum ModelTierTs {
    Fast,
    Standard,
    Deep,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct WakeEntryTs {
    pub wake_entry_id: String,
    pub trigger_kind: TriggerKindTs,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub execution_mode: ExecutionModeTs,
    pub authored_by: AuthoredByTs,
    pub probability_promille: u16,
    pub recipe_ref: String,
    pub model_tier: ModelTierTs,
    pub inference_target_ref: Option<String>,
    pub substrate_tool_palette: Vec<String>,
    pub workspace_tool_palette: Vec<String>,
    pub max_rounds: u16,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct WakeEntryDraftTs {
    pub trigger_kind: TriggerKindTs,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub execution_mode: ExecutionModeTs,
    pub authored_by: AuthoredByTs,
    pub probability_promille: u16,
    pub recipe_ref: String,
    pub model_tier: ModelTierTs,
    pub inference_target_ref: Option<String>,
    pub substrate_tool_palette: Vec<String>,
    pub workspace_tool_palette: Vec<String>,
    pub max_rounds: u16,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SetWakeEntriesTs {
    pub owner: Owner,
    pub personality_instance_id: String,
    pub entries: Vec<WakeEntryDraftTs>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SetWakeEntriesOutcomeTs {
    pub active_entries: u32,
}

#[tauri::command]
#[specta::specta]
pub async fn schema(engine: State<'_, Arc<Engine>>) -> Result<SchemaResponse, ProtocolError> {
    crate::perf::ipc::record(
        "schema",
        0,
        async move { Ok(engine.schema(&SchemaRequest)) },
    )
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn query(
    engine: State<'_, Arc<Engine>>,
    req: QueryRequest,
) -> Result<QueryResponse, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("query", req_bytes, async move {
        engine.query(&Credentials::None, &req).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn event_history(
    engine: State<'_, Arc<Engine>>,
    req: EventHistoryRequest,
) -> Result<EventHistoryResponse, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("event_history", req_bytes, async move {
        engine.event_history(&Credentials::None, &req).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn event_ingest(
    engine: State<'_, Arc<Engine>>,
    draft: EventDraft,
) -> Result<EventIngestOutcome, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&draft);
    crate::perf::ipc::record("event_ingest", req_bytes, async move {
        engine.event_ingest(&Credentials::None, draft).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn goal_write(
    engine: State<'_, Arc<Engine>>,
    draft: GoalDraft,
) -> Result<GoalWriteOutcome, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&draft);
    crate::perf::ipc::record("goal_write", req_bytes, async move {
        engine.write_goal(&Credentials::None, draft).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn provision_owner(
    engine: State<'_, Arc<Engine>>,
    owner: Owner,
) -> Result<(), ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&owner);
    crate::perf::ipc::record("provision_owner", req_bytes, async move {
        engine.provision_owner(&owner).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_personality_instances(
    engine: State<'_, Arc<Engine>>,
    req: ListPersonalityInstancesTs,
) -> Result<Vec<PersonalityInstanceTs>, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("list_personality_instances", req_bytes, async move {
        let rows = engine
            .list_personality_instances(&req.owner, req.include_tombstoned)
            .await?;
        rows.into_iter()
            .map(|row| Ok::<_, ProtocolError>(PersonalityInstanceTs::from_row(row)))
            .collect()
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn instantiate_personality(
    engine: State<'_, Arc<Engine>>,
    req: InstantiatePersonalityTs,
) -> Result<InstantiatePersonalityOutcomeTs, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("instantiate_personality", req_bytes, async move {
        let out = engine
            .instantiate_personality(proxima_core::InstantiatePersonalityRequest {
                owner: req.owner,
                display_name: req.display_name,
                purpose: req.purpose,
            })
            .await?;
        Ok(InstantiatePersonalityOutcomeTs {
            instance_id: out.instance_id.into_inner().to_string(),
        })
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_wake_entries(
    engine: State<'_, Arc<Engine>>,
    req: SetWakeEntriesTs,
) -> Result<SetWakeEntriesOutcomeTs, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("set_wake_entries", req_bytes, async move {
        let instance_id = uuid::Uuid::parse_str(&req.personality_instance_id)
            .map_err(|e| ProtocolError::internal(format!("personality_instance_id: {e}")))?;
        let personality_instance_id = PersonalityInstanceId::new(instance_id);
        let core_req = proxima_core::SetWakeEntriesRequest {
            owner: req.owner,
            personality_instance_id,
            entries: req
                .entries
                .into_iter()
                .map(|draft| draft_to_core(draft, personality_instance_id))
                .collect(),
        };
        let out = engine
            .set_wake_entries(&Credentials::None, &core_req)
            .await?;
        Ok(SetWakeEntriesOutcomeTs {
            active_entries: out.active_entries,
        })
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn tombstone_personality(
    engine: State<'_, Arc<Engine>>,
    req: TombstonePersonalityTs,
) -> Result<TombstonePersonalityOutcomeTs, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("tombstone_personality", req_bytes, async move {
        let instance_id = uuid::Uuid::parse_str(&req.personality_instance_id)
            .map_err(|e| ProtocolError::internal(format!("personality_instance_id: {e}")))?;
        let out = engine
            .tombstone_personality(proxima_core::TombstonePersonalityRequest {
                owner: req.owner,
                personality_instance_id: PersonalityInstanceId::new(instance_id),
            })
            .await?;
        Ok(TombstonePersonalityOutcomeTs {
            status: out.status,
            idempotent_replay: out.idempotent_replay,
        })
    })
    .await
}

/// Subscribe — engine returns a `Stream<Item = ChangeEvent>`; we
/// spawn a forwarder onto the caller-supplied `Channel<ChangeEvent>`
/// so events flow back through Tauri IPC. The handler returns when
/// the subscription is established; the stream lifetime is bound to
/// the spawned task and ends when storage closes its end (or the JS
/// side drops the channel, surfaced as a send error).
#[tauri::command]
#[specta::specta]
pub async fn subscribe(
    engine: State<'_, Arc<Engine>>,
    req: SubscribeRequest,
    on_event: Channel<ChangeEvent>,
) -> Result<(), ProtocolError> {
    let stream = engine.subscribe(&Credentials::None, req).await?;
    tokio::spawn(async move {
        let mut inbound = stream;
        while let Some(event) = inbound.next().await {
            if on_event.send(event).is_err() {
                break;
            }
        }
    });
    Ok(())
}

impl PersonalityInstanceTs {
    fn from_row(row: PersonalityInstanceRow) -> Self {
        let wake_entries = row.wake_entries.iter().map(WakeEntryTs::from_row).collect();
        Self {
            owner: row.owner,
            personality_instance_id: row.personality_instance_id.into_inner().to_string(),
            current_root_perspective_memory_id: row
                .current_root_perspective_memory_id
                .into_inner()
                .to_string(),
            display_name: row.display_name,
            status: row.status,
            wake_entries,
            flavor: None,
        }
    }
}

impl WakeEntryTs {
    fn from_row(row: &proxima_core::WakeEntryRow) -> Self {
        Self {
            wake_entry_id: row.wake_entry_id.to_string(),
            trigger_kind: match row.trigger_kind {
                proxima_core::WakeEntryTriggerKind::OnMemory => TriggerKindTs::OnMemory,
                proxima_core::WakeEntryTriggerKind::OnEdge => TriggerKindTs::OnEdge,
            },
            trigger_id: row.trigger_id.clone(),
            label: row.label.clone(),
            enabled: row.enabled,
            execution_mode: match row.execution_mode {
                proxima_core::WakeEntryExecutionMode::SubstrateOnly => {
                    ExecutionModeTs::SubstrateOnly
                }
                proxima_core::WakeEntryExecutionMode::Workspace => ExecutionModeTs::Workspace,
            },
            authored_by: match row.authored_by {
                proxima_core::WakeEntryAuthoredBy::Any => AuthoredByTs::Any,
                proxima_core::WakeEntryAuthoredBy::SelfAuthor => AuthoredByTs::SelfAuthor,
                proxima_core::WakeEntryAuthoredBy::Other => AuthoredByTs::Other,
            },
            probability_promille: row.probability_promille,
            recipe_ref: row.recipe_ref.clone(),
            model_tier: tier_to_ts(row.model_tier),
            inference_target_ref: row.inference_target_ref.clone(),
            substrate_tool_palette: row.substrate_tool_palette.clone(),
            workspace_tool_palette: row.workspace_tool_palette.clone(),
            max_rounds: row.max_rounds,
            disabled_reason: row.disabled_reason.clone(),
        }
    }
}

pub(crate) fn tier_from_ts(tier: ModelTierTs) -> proxima_core::ModelTier {
    match tier {
        ModelTierTs::Fast => proxima_core::ModelTier::Fast,
        ModelTierTs::Standard => proxima_core::ModelTier::Standard,
        ModelTierTs::Deep => proxima_core::ModelTier::Deep,
    }
}

pub(crate) fn tier_to_ts(tier: proxima_core::ModelTier) -> ModelTierTs {
    match tier {
        proxima_core::ModelTier::Fast => ModelTierTs::Fast,
        proxima_core::ModelTier::Standard => ModelTierTs::Standard,
        proxima_core::ModelTier::Deep => ModelTierTs::Deep,
    }
}

fn draft_to_core(
    draft: WakeEntryDraftTs,
    personality_instance_id: PersonalityInstanceId,
) -> proxima_core::WakeEntryDraft {
    proxima_core::WakeEntryDraft {
        wake_entry_id: uuid::Uuid::now_v7(),
        personality_instance_id,
        trigger_kind: match draft.trigger_kind {
            TriggerKindTs::OnMemory => proxima_core::WakeEntryTriggerKind::OnMemory,
            TriggerKindTs::OnEdge => proxima_core::WakeEntryTriggerKind::OnEdge,
        },
        trigger_id: draft.trigger_id,
        label: draft.label,
        enabled: draft.enabled,
        execution_mode: match draft.execution_mode {
            ExecutionModeTs::SubstrateOnly => proxima_core::WakeExecutionMode::SubstrateOnly,
            ExecutionModeTs::Workspace => proxima_core::WakeExecutionMode::Workspace,
        },
        authored_by: match draft.authored_by {
            AuthoredByTs::Any => proxima_core::WakeEntryAuthoredBy::Any,
            AuthoredByTs::SelfAuthor => proxima_core::WakeEntryAuthoredBy::SelfAuthor,
            AuthoredByTs::Other => proxima_core::WakeEntryAuthoredBy::Other,
        },
        probability_promille: draft.probability_promille,
        recipe_ref: draft.recipe_ref,
        model_tier: tier_from_ts(draft.model_tier),
        inference_target_ref: draft.inference_target_ref,
        substrate_tool_palette: draft.substrate_tool_palette,
        workspace_tool_palette: draft.workspace_tool_palette,
        max_rounds: draft.max_rounds,
    }
}

impl From<&proxima_core::FlavorDescriptor> for FlavorDescriptorTs {
    fn from(d: &proxima_core::FlavorDescriptor) -> Self {
        Self {
            flavor_id: d.flavor_id.clone(),
            display_name: d.display_name.clone(),
            package_version: d.package_version.clone(),
            author: d.author.clone(),
            provenance: FlavorProvenanceTs::from(&d.provenance),
        }
    }
}

impl From<&FlavorProvenance> for FlavorProvenanceTs {
    fn from(p: &FlavorProvenance) -> Self {
        match p {
            FlavorProvenance::Builtin => Self::Builtin,
            FlavorProvenance::Marketplace { source_url } => Self::Marketplace {
                source_url: source_url.clone(),
            },
            FlavorProvenance::Local { workspace_path } => Self::Local {
                workspace_path: workspace_path.clone(),
            },
        }
    }
}
