//! Personality wake/decide/write substrate.
//!
//! Personalities are build-time flavor declarations. Runtime instances
//! are addressed by `(personality_type_id, personality_instance_id)` and
//! carry a self-Perspective plus wake filters in storage.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use uuid::Uuid;

use crate::error::ProtocolError;
use crate::outbox::{ChangeEvent, EntityKind};
use crate::{
    Engine, GoalId, LlmCaps, MemoryId, ModelTier, Owner, RegisteredRelation, SchemaId,
    SchemaVersion,
};

pub mod authorization;
pub mod tools;

pub use tools::substrate_pack;

pub const MAX_WAKE_CHAIN_DEPTH: u16 = 10;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct PersonalityInstanceId(Uuid);

impl PersonalityInstanceId {
    #[must_use]
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
)]
pub struct WakeChainDepth(u16);

impl WakeChainDepth {
    #[must_use]
    pub const fn new(inner: u16) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn zero() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn into_inner(self) -> u16 {
        self.0
    }

    #[must_use]
    pub fn next_after(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct PersonalityRef {
    pub personality_type_id: String,
    pub personality_instance_id: PersonalityInstanceId,
}

impl PersonalityRef {
    #[must_use]
    pub fn new(
        personality_type_id: impl Into<String>,
        personality_instance_id: PersonalityInstanceId,
    ) -> Self {
        Self {
            personality_type_id: personality_type_id.into(),
            personality_instance_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthorFilter {
    Any,
    External,
    Personality {
        personality_type_id: String,
        personality_instance_id: Option<PersonalityInstanceId>,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeTarget {
    Any,
    SelfPerspective,
    Memory { memory_id: MemoryId },
    Goal { goal_id: GoalId },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeFilter {
    OnMemory {
        version: u16,
        schema_id: SchemaId,
        authored_by: AuthorFilter,
        probability: f32,
    },
    OnEdge {
        version: u16,
        relation_id: String,
        source: WakeTarget,
        target: WakeTarget,
        probability: f32,
    },
    Custom {
        version: u16,
        kind_id: String,
        params: serde_json::Value,
        probability: f32,
    },
}

impl WakeFilter {
    #[must_use]
    pub fn on_memory(schema_id: SchemaId) -> Self {
        Self::OnMemory {
            version: 1,
            schema_id,
            authored_by: AuthorFilter::Any,
            probability: 1.0,
        }
    }

    #[must_use]
    pub fn on_self_inspires() -> Self {
        Self::OnEdge {
            version: 1,
            relation_id: crate::CORE_INSPIRES_RELATION.to_string(),
            source: WakeTarget::Any,
            target: WakeTarget::SelfPerspective,
            probability: 1.0,
        }
    }

    #[must_use]
    pub fn probability(&self) -> f32 {
        match self {
            Self::OnMemory { probability, .. }
            | Self::OnEdge { probability, .. }
            | Self::Custom { probability, .. } => *probability,
        }
    }

    #[must_use]
    pub fn version(&self) -> u16 {
        match self {
            Self::OnMemory { version, .. }
            | Self::OnEdge { version, .. }
            | Self::Custom { version, .. } => *version,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactRow {
    pub memory_id: MemoryId,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub payload_json: serde_json::Value,
    pub wake_chain_depth: WakeChainDepth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub memory_id: MemoryId,
    pub kind: String,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub text: Option<String>,
    pub wake_chain_depth: WakeChainDepth,
    pub payload_json: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AbstractionRow {
    pub memory_id: MemoryId,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub text: String,
    pub payload_json: serde_json::Value,
    pub wake_chain_depth: WakeChainDepth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarSpec {
    pub schema_id: SchemaId,
    pub sidecar_table: String,
}

#[derive(Debug, Clone)]
pub struct PersonalitySelfDraft {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub text: String,
    pub typed_payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonalityMemoryKind {
    Abstraction,
    Perspective,
}

impl PersonalityMemoryKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Abstraction => "Abstraction",
            Self::Perspective => "Perspective",
        }
    }

    #[must_use]
    pub const fn entity_kind(self) -> EntityKind {
        match self {
            Self::Abstraction => EntityKind::Abstraction,
            Self::Perspective => EntityKind::Perspective,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PersonalityMemoryDraft {
    pub kind: PersonalityMemoryKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub text: String,
    pub typed_payload: serde_json::Value,
    pub provenance: Vec<MemoryId>,
    pub embedding: Vec<f32>,
    pub embedding_model_id: String,
}

#[derive(Debug)]
pub struct PersonalityToolContext<'a> {
    pub engine: &'a Engine,
    pub owner: &'a Owner,
    pub type_id: &'a str,
    pub instance_id: PersonalityInstanceId,
    pub current_self_perspective_memory_id: MemoryId,
    pub triggering_event_memory_id: MemoryId,
    pub triggering_event_depth: WakeChainDepth,
    pub writeable_schemas: &'a [&'static str],
    pub writeable_relations: &'a [&'static str],
    pub palette: &'a [Arc<dyn PersonalityTool>],
    read_log: tokio::sync::Mutex<Vec<(MemoryId, WakeChainDepth)>>,
}

impl<'a> PersonalityToolContext<'a> {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        engine: &'a Engine,
        owner: &'a Owner,
        type_id: &'a str,
        instance_id: PersonalityInstanceId,
        current_self_perspective_memory_id: MemoryId,
        triggering_event_memory_id: MemoryId,
        triggering_event_depth: WakeChainDepth,
        writeable_schemas: &'a [&'static str],
        writeable_relations: &'a [&'static str],
        palette: &'a [Arc<dyn PersonalityTool>],
    ) -> Self {
        Self {
            engine,
            owner,
            type_id,
            instance_id,
            current_self_perspective_memory_id,
            triggering_event_memory_id,
            triggering_event_depth,
            writeable_schemas,
            writeable_relations,
            palette,
            read_log: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    pub(crate) async fn record_read(
        &self,
        ids: impl IntoIterator<Item = (MemoryId, WakeChainDepth)>,
    ) {
        let mut log = self.read_log.lock().await;
        log.extend(ids);
    }

    pub(crate) async fn snapshot_provenance(&self) -> (Vec<MemoryId>, WakeChainDepth) {
        let log = self.read_log.lock().await;
        let mut provenance = Vec::with_capacity(log.len() + 1);
        provenance.push(self.triggering_event_memory_id);
        let mut depth = self.triggering_event_depth;
        for (memory_id, memory_depth) in log.iter().copied() {
            if !provenance.contains(&memory_id) {
                provenance.push(memory_id);
            }
            depth = depth.max(memory_depth);
        }
        (provenance, depth.next_after())
    }
}

#[derive(Debug, Clone)]
pub struct PersonalityInstanceRow {
    pub owner: Owner,
    pub personality_type_id: String,
    pub personality_instance_id: PersonalityInstanceId,
    pub current_self_perspective_memory_id: MemoryId,
    pub display_name: String,
    pub status: String,
    pub wake_filters: Vec<WakeFilter>,
}

#[derive(Debug, Clone)]
pub struct WakeConfigRow {
    pub owner: Owner,
    pub personality_type_id: String,
    pub personality_instance_id: PersonalityInstanceId,
    pub current_self_perspective_memory_id: MemoryId,
    pub wake_filters_json: serde_json::Value,
    pub status: String,
    pub last_considered_seq: Uuid,
}

#[derive(Debug, Clone)]
pub struct ChangeEventForWake {
    pub event: ChangeEvent,
    pub authoring_personality_type_id: Option<String>,
    pub authoring_personality_instance_id: Option<PersonalityInstanceId>,
    pub wake_chain_depth: WakeChainDepth,
}

#[derive(Debug, Clone)]
pub struct InstantiatePersonalityRequest {
    pub owner: Owner,
    pub personality_type_id: String,
    pub payload_overrides: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InstantiatePersonalityResponse {
    pub instance_id: PersonalityInstanceId,
}

#[derive(Debug, Clone)]
pub struct SetWakeConfigRequest {
    pub owner: Owner,
    pub personality_type_id: String,
    pub personality_instance_id: PersonalityInstanceId,
    pub wake_filters: Vec<WakeFilter>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SetWakeConfigResponse {
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstonePersonalityRequest {
    pub owner: Owner,
    pub personality_type_id: String,
    pub personality_instance_id: PersonalityInstanceId,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TombstonePersonalityResponse {
    pub status: String,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeInvocationStatus {
    Running,
    Succeeded,
    Truncated,
    Failed,
}

impl WakeInvocationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Truncated => "truncated",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug)]
pub struct PersonalityWriteRequest<'a> {
    pub owner: Owner,
    pub instance: PersonalityRef,
    pub model_id: &'a str,
    pub prompt_version: &'a str,
    pub provenance_relation: RegisteredRelation<'a>,
    pub supersedes_relation: RegisteredRelation<'a>,
    pub wake_chain_depth: WakeChainDepth,
    pub memories: &'a [PersonalityMemoryDraft],
    pub sidecar_table: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalityWriteOutcome {
    pub memory_ids: Vec<MemoryId>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PersonalityToolResult {
    pub content: serde_json::Value,
    pub is_error: bool,
}

impl PersonalityToolResult {
    #[must_use]
    pub fn ok(content: serde_json::Value) -> Self {
        Self {
            content,
            is_error: false,
        }
    }

    #[must_use]
    pub fn error(content: serde_json::Value) -> Self {
        Self {
            content,
            is_error: true,
        }
    }
}

#[async_trait]
pub trait PersonalityTool: Send + Sync + std::fmt::Debug {
    fn tool_id(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn args_schema(&self) -> serde_json::Value;

    async fn invoke(
        &self,
        ctx: &PersonalityToolContext<'_>,
        args: serde_json::Value,
    ) -> Result<PersonalityToolResult, ProtocolError>;
}

#[async_trait]
pub trait WakeFilterCtx: Send {
    async fn fetch_memory(
        &mut self,
        memory_id: MemoryId,
    ) -> Result<Option<serde_json::Value>, ProtocolError>;
    async fn current_self_perspective(&mut self) -> Result<MemoryId, ProtocolError>;
}

#[async_trait]
pub trait WakeFilterKind: Send + Sync + std::fmt::Debug {
    fn kind_id(&self) -> &'static str;
    fn version(&self) -> u16;
    fn params_schema(&self) -> serde_json::Value;

    async fn matches(
        &self,
        ctx: &mut dyn WakeFilterCtx,
        params: &serde_json::Value,
        event: &ChangeEvent,
    ) -> Result<bool, ProtocolError>;
}

#[derive(Debug)]
pub struct OnMemoryWakeFilterKind;

#[async_trait]
impl WakeFilterKind for OnMemoryWakeFilterKind {
    fn kind_id(&self) -> &'static str {
        "core/on-memory"
    }

    fn version(&self) -> u16 {
        1
    }

    fn params_schema(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(OnMemoryWakeFilterParams))
            .expect("schema serializes")
    }

    async fn matches(
        &self,
        _ctx: &mut dyn WakeFilterCtx,
        _params: &serde_json::Value,
        _event: &ChangeEvent,
    ) -> Result<bool, ProtocolError> {
        Ok(false)
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, JsonSchema)]
struct OnMemoryWakeFilterParams {
    schema_id: String,
}

#[derive(Debug)]
pub struct OnEdgeWakeFilterKind;

#[async_trait]
impl WakeFilterKind for OnEdgeWakeFilterKind {
    fn kind_id(&self) -> &'static str {
        "core/on-edge"
    }

    fn version(&self) -> u16 {
        1
    }

    fn params_schema(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(OnEdgeWakeFilterParams))
            .expect("schema serializes")
    }

    async fn matches(
        &self,
        _ctx: &mut dyn WakeFilterCtx,
        _params: &serde_json::Value,
        _event: &ChangeEvent,
    ) -> Result<bool, ProtocolError> {
        Ok(false)
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, JsonSchema)]
struct OnEdgeWakeFilterParams {
    relation_id: String,
}

/// Build-time personality declaration contributed by a flavor.
#[async_trait]
pub trait PersonalityFlavor: Send + Sync + std::fmt::Debug {
    fn personality_type_id(&self) -> &'static str;
    fn self_schema(&self) -> SchemaId;
    fn default_self_payload(
        &self,
        owner: &Owner,
        payload_overrides: Option<&serde_json::Value>,
    ) -> Result<PersonalitySelfDraft, ProtocolError>;
    fn system_prompt(&self) -> &'static str;
    fn tools(&self) -> Vec<Arc<dyn PersonalityTool>> {
        Vec::new()
    }
    fn writeable_schemas(&self) -> &'static [&'static str];
    fn writeable_relations(&self) -> &'static [&'static str];
    fn default_wake_filters(&self) -> Vec<WakeFilter>;
    fn tier(&self) -> ModelTier {
        ModelTier::Standard
    }
    fn max_wake_chain_depth(&self) -> u16 {
        MAX_WAKE_CHAIN_DEPTH
    }
    fn requires(&self) -> LlmCaps {
        LlmCaps::none()
    }
}

#[cfg(test)]
mod wake_filter_envelope_tests {
    use super::*;

    #[test]
    fn on_memory_round_trips_through_serde() {
        let original = WakeFilter::OnMemory {
            version: 1,
            schema_id: SchemaId::new("proxima-code/commit-v1".into()),
            authored_by: AuthorFilter::External,
            probability: 0.5,
        };
        let json = serde_json::to_value(&original).expect("serializes");
        assert_eq!(json["kind"], "on_memory");
        assert_eq!(json["version"], 1);
        let decoded: WakeFilter = serde_json::from_value(json).expect("deserializes");
        assert_eq!(decoded, original);
    }

    #[test]
    fn on_edge_round_trips_through_serde() {
        let original = WakeFilter::on_self_inspires();
        let json = serde_json::to_value(&original).expect("serializes");
        assert_eq!(json["kind"], "on_edge");
        let decoded: WakeFilter = serde_json::from_value(json).expect("deserializes");
        assert_eq!(decoded, original);
    }

    #[test]
    fn rejects_missing_version() {
        let no_version = serde_json::json!({
            "kind": "on_memory",
            "schema_id": "proxima-code/commit-v1",
            "authored_by": { "kind": "any" },
            "probability": 1.0
        });
        let err = serde_json::from_value::<WakeFilter>(no_version).unwrap_err();
        assert!(
            err.to_string().contains("missing field `version`"),
            "got: {err}",
        );
    }

    #[test]
    fn rejects_unknown_kind() {
        let unknown = serde_json::json!({
            "kind": "on_galaxy",
            "version": 1,
            "probability": 1.0
        });
        let err = serde_json::from_value::<WakeFilter>(unknown).unwrap_err();
        assert!(
            err.to_string().contains("unknown variant"),
            "got: {err}",
        );
    }

    #[test]
    fn rejects_schema_invalid_params() {
        let bad_probability_type = serde_json::json!({
            "kind": "on_memory",
            "version": 1,
            "schema_id": "proxima-code/commit-v1",
            "authored_by": { "kind": "any" },
            "probability": "high"
        });
        assert!(serde_json::from_value::<WakeFilter>(bad_probability_type).is_err());

        let missing_relation = serde_json::json!({
            "kind": "on_edge",
            "version": 1,
            "source": { "kind": "any" },
            "target": { "kind": "any" },
            "probability": 1.0
        });
        assert!(serde_json::from_value::<WakeFilter>(missing_relation).is_err());
    }

    #[test]
    fn custom_envelope_round_trips() {
        let original = WakeFilter::Custom {
            version: 2,
            kind_id: "proxima-code/recent-pr-touch".into(),
            params: serde_json::json!({"max_age_days": 14}),
            probability: 0.75,
        };
        let json = serde_json::to_value(&original).expect("serializes");
        let decoded: WakeFilter = serde_json::from_value(json).expect("deserializes");
        assert_eq!(decoded, original);
    }
}
