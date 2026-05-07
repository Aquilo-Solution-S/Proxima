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
    ChangeEvent, Engine, FlavorDescriptor, FlavorProvenance, Owner, PersonalityInstanceId,
    PersonalityInstanceRow,
};
use tauri::State;
use tauri::ipc::Channel;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InstantiatePersonalityTs {
    pub owner: Owner,
    pub personality_type_id: String,
    pub payload_overrides: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InstantiatePersonalityOutcomeTs {
    pub instance_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SetWakeConfigTs {
    pub owner: Owner,
    pub personality_type_id: String,
    pub personality_instance_id: String,
    pub wake_filters: Vec<WakeFilterTs>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SetWakeConfigOutcomeTs {
    pub status: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ListPersonalityInstancesTs {
    pub owner: Owner,
    pub personality_type_id: Option<String>,
    #[serde(default)]
    pub include_tombstoned: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TombstonePersonalityTs {
    pub owner: Owner,
    pub personality_type_id: String,
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
    pub personality_type_id: String,
    pub personality_instance_id: String,
    pub current_self_perspective_memory_id: String,
    pub display_name: String,
    pub status: String,
    pub wake_filters: Vec<WakeFilterTs>,
    pub flavor: FlavorDescriptorTs,
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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthorFilterTs {
    Any,
    External,
    Personality {
        personality_type_id: String,
        personality_instance_id: Option<String>,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeTargetTs {
    Any,
    SelfPerspective,
    Memory { memory_id: String },
    Goal { goal_id: String },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeFilterTs {
    OnMemory {
        version: u16,
        schema_id: String,
        authored_by: AuthorFilterTs,
        probability: f32,
    },
    OnEdge {
        version: u16,
        relation_id: String,
        source: WakeTargetTs,
        target: WakeTargetTs,
        probability: f32,
    },
    Custom {
        version: u16,
        kind_id: String,
        params_json: String,
        probability: f32,
    },
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
            .list_personality_instances(
                &req.owner,
                req.personality_type_id.as_deref(),
                req.include_tombstoned,
            )
            .await?;
        let registry = engine.registry();
        rows.into_iter()
            .map(|row| {
                let flavor = registry
                    .flavor_for_personality_type(&row.personality_type_id)
                    .ok_or_else(|| {
                        ProtocolError::internal(format!(
                            "no FlavorDescriptor for personality_type_id {}",
                            row.personality_type_id,
                        ))
                    })?;
                PersonalityInstanceTs::from_row(row, flavor)
            })
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
        let payload_overrides = req
            .payload_overrides
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| ProtocolError::internal(format!("payload_overrides JSON: {e}")))?;
        let out = engine
            .instantiate_personality(proxima_core::InstantiatePersonalityRequest {
                owner: req.owner,
                personality_type_id: req.personality_type_id,
                payload_overrides,
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
pub async fn set_wake_config(
    _engine: State<'_, Arc<Engine>>,
    req: SetWakeConfigTs,
) -> Result<SetWakeConfigOutcomeTs, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("set_wake_config", req_bytes, async move {
        let _ = req;
        Err(ProtocolError::internal(
            "SetWakeConfig was removed by the Phase 1a WakeEntry migration",
        ))
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
                personality_type_id: req.personality_type_id,
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
    fn from_row(
        row: PersonalityInstanceRow,
        flavor: &FlavorDescriptor,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            owner: row.owner,
            personality_type_id: row.personality_type_id,
            personality_instance_id: row.personality_instance_id.into_inner().to_string(),
            current_self_perspective_memory_id: row
                .current_root_perspective_memory_id
                .into_inner()
                .to_string(),
            display_name: row.display_name,
            status: row.status,
            wake_filters: Vec::new(),
            flavor: FlavorDescriptorTs::from(flavor),
        })
    }
}

impl From<&FlavorDescriptor> for FlavorDescriptorTs {
    fn from(d: &FlavorDescriptor) -> Self {
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
