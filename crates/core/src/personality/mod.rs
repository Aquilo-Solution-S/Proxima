//! Personality wake/decide/write substrate.
//!
//! Personalities are build-time flavor declarations. Runtime instances
//! are addressed by `personality_instance_id` and point at a Root
//! Perspective plus WakeEntry rows in storage.

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::ProtocolError;
use crate::outbox::{ChangeEvent, EntityKind};
use crate::{Engine, MemoryId, ModelTier, Owner, RegisteredRelation, SchemaId, SchemaVersion};

pub mod authorization;
pub mod tools;

pub use tools::{ActiveGoalSummary, substrate_pack};
#[doc(hidden)]
pub use tools::__test_only_model_id_from_wake_invocation;

pub const MAX_WAKE_CHAIN_DEPTH: u16 = 10;

/// Canonical schema id for the Root-Perspective sidecar that backs every
/// personality after Phase 2 Step 1. Stamped on the memory + change_event
/// rows minted by `instantiate_personality`.
pub const ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID: &str =
    "proxima-core/root-personality-perspective-v1";

/// Sidecar table backing [`ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID`].
pub const ROOT_PERSONALITY_PERSPECTIVE_SIDECAR_TABLE: &str =
    "proxima_core.root_personality_perspective_v1";

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
    pub personality_instance_id: PersonalityInstanceId,
}

impl PersonalityRef {
    #[must_use]
    pub const fn new(personality_instance_id: PersonalityInstanceId) -> Self {
        Self {
            personality_instance_id,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
#[serde(rename_all = "snake_case")]
pub enum WakeEntryTriggerKind {
    OnMemory,
    OnEdge,
}

impl WakeEntryTriggerKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OnMemory => "on_memory",
            Self::OnEdge => "on_edge",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeExecutionMode {
    SubstrateOnly,
    Workspace,
}

impl WakeExecutionMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SubstrateOnly => "substrate_only",
            Self::Workspace => "workspace",
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WakeEntryDraft {
    pub wake_entry_id: Uuid,
    pub personality_instance_id: PersonalityInstanceId,
    pub trigger_kind: WakeEntryTriggerKind,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub execution_mode: WakeExecutionMode,
    pub authored_by: WakeEntryAuthoredBy,
    pub probability_promille: u16,
    pub recipe_ref: String,
    pub model_tier: ModelTier,
    pub inference_target_ref: Option<String>,
    pub substrate_tool_palette: Vec<String>,
    pub workspace_tool_palette: Vec<String>,
    pub max_rounds: u16,
}

impl WakeEntryDraft {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        wake_entry_id: Uuid,
        personality_instance_id: PersonalityInstanceId,
        trigger_kind: WakeEntryTriggerKind,
        trigger_id: impl Into<String>,
        label: impl Into<String>,
        authored_by: WakeEntryAuthoredBy,
        probability_promille: u16,
        recipe_ref: impl Into<String>,
        model_tier: ModelTier,
        inference_target_ref: Option<String>,
        substrate_tool_palette: Vec<String>,
        max_rounds: u16,
    ) -> Result<Self, ProtocolError> {
        if probability_promille > 1000 {
            return Err(ProtocolError::internal(
                "wake entry probability_promille must be between 0 and 1000",
            ));
        }
        if max_rounds == 0 {
            return Err(ProtocolError::internal(
                "wake entry max_rounds must be greater than 0",
            ));
        }
        Ok(Self {
            wake_entry_id,
            personality_instance_id,
            trigger_kind,
            trigger_id: trigger_id.into(),
            label: label.into(),
            enabled: true,
            execution_mode: WakeExecutionMode::SubstrateOnly,
            authored_by,
            probability_promille,
            recipe_ref: recipe_ref.into(),
            model_tier,
            inference_target_ref,
            substrate_tool_palette,
            workspace_tool_palette: Vec::new(),
            max_rounds,
        })
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
    pub current_root_perspective_memory_id: MemoryId,
    pub triggering_event_memory_id: MemoryId,
    pub triggering_event_depth: WakeChainDepth,
    pub writeable_schemas: &'a [&'static str],
    pub writeable_relations: &'a [&'static str],
    pub palette: &'a [Arc<dyn PersonalityTool>],
    /// Active wake invocation, when this tool call is dispatched as part
    /// of a goose-driven wake. `None` for the legacy admin-tool path
    /// (no wake context bound to the request). Substrate tools that
    /// stamp memory provenance read `model_id` from here so the row
    /// reflects the actual InferenceTarget that drove the wake instead
    /// of a static `Standard`-tier guess.
    pub wake_invocation: Option<&'a crate::wake::token_store::WakeTokenContext>,
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
        current_root_perspective_memory_id: MemoryId,
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
            current_root_perspective_memory_id,
            triggering_event_memory_id,
            triggering_event_depth,
            writeable_schemas,
            writeable_relations,
            palette,
            wake_invocation: None,
            read_log: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    /// Bind the active `WakeTokenContext` for the duration of this tool
    /// dispatch. The MCP handler calls this after extracting the wake
    /// token from request extensions.
    #[must_use]
    pub fn with_wake_invocation(
        mut self,
        wake_invocation: &'a crate::wake::token_store::WakeTokenContext,
    ) -> Self {
        self.wake_invocation = Some(wake_invocation);
        self
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
    pub personality_instance_id: PersonalityInstanceId,
    pub current_root_perspective_memory_id: MemoryId,
    pub display_name: String,
    pub status: String,
    pub wake_entries: Vec<WakeEntryRow>,
}

/// Owner-scoped projection of one row from `proxima_core.personality`.
/// Returned by `Storage::fetch_personality_runtime`. Carries just the
/// columns the wake-context assembler needs to read fresh per wake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalityRuntimeRow {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub current_root_perspective_memory_id: MemoryId,
    pub display_name: String,
    pub status: String,
}

/// Sidecar row backing a Root-Perspective memory. Returned by
/// `Storage::fetch_root_personality_perspective`. Lives next to
/// `PersonalityRuntimeRow` because both feed the wake-context
/// `root_perspective` envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootPersonalityPerspectiveRow {
    pub memory_id: MemoryId,
    pub display_name: String,
    pub purpose: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct WakeEntryRow {
    pub wake_entry_id: Uuid,
    pub trigger_kind: WakeEntryTriggerKind,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub execution_mode: WakeEntryExecutionMode,
    pub authored_by: WakeEntryAuthoredBy,
    pub probability_promille: u16,
    pub recipe_ref: String,
    pub model_tier: ModelTier,
    pub inference_target_ref: Option<String>,
    pub substrate_tool_palette: Vec<String>,
    pub workspace_tool_palette: Vec<String>,
    pub max_rounds: u16,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum WakeEntryExecutionMode {
    SubstrateOnly,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum WakeEntryAuthoredBy {
    Any,
    SelfAuthor,
    Other,
}

impl WakeEntryAuthoredBy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::SelfAuthor => "self",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WakeDispatchEntryRow {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub current_root_perspective_memory_id: MemoryId,
    pub max_wake_chain_depth: u16,
    pub last_considered_seq: Uuid,
    pub wake_entry: WakeEntryDraft,
}

#[derive(Debug, Clone)]
pub struct ChangeEventForWake {
    pub event: ChangeEvent,
    pub authoring_personality_instance_id: Option<PersonalityInstanceId>,
    pub wake_chain_depth: WakeChainDepth,
}

#[derive(Debug, Clone)]
pub struct InstantiatePersonalityRequest {
    pub owner: Owner,
    pub display_name: String,
    pub purpose: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InstantiatePersonalityResponse {
    pub instance_id: PersonalityInstanceId,
}

#[derive(Debug, Clone)]
pub struct SetWakeEntriesRequest {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub entries: Vec<WakeEntryDraft>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SetWakeEntriesResponse {
    pub active_entries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstonePersonalityRequest {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TombstonePersonalityResponse {
    pub status: String,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WakeInvocationStart {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub wake_entry_id: Uuid,
    pub change_event_seq: Uuid,
    pub wake_token: Uuid,
    pub recipe_sha256: String,
    pub resolved_inference_target_ref: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WakeInvocationFinalize {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub wake_entry_id: Uuid,
    pub change_event_seq: Uuid,
    pub status: WakeInvocationStatus,
    pub turn_count: Option<u16>,
    pub cost_usd: Option<f64>,
    pub failure_reason: Option<String>,
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

/// Build-time personality declaration contributed by a flavor.
pub trait PersonalityFlavor: Send + Sync + std::fmt::Debug {
    fn personality_type_id(&self) -> &'static str;
    /// Default name shown when `provision_owner` seeds an instance.
    fn default_display_name(&self) -> &'static str;
    /// Default purpose shown when `provision_owner` seeds an instance.
    fn default_purpose(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_entry_accepts_promille_probability() {
        let entry = WakeEntryDraft::new(
            Uuid::from_u128(10),
            PersonalityInstanceId::new(Uuid::from_u128(1)),
            WakeEntryTriggerKind::OnMemory,
            "proxima-test/fact-v1",
            "on_test_fact",
            WakeEntryAuthoredBy::Any,
            250,
            "recipe:proxima-test/personality-v1",
            ModelTier::Fast,
            Some("local-cli:codex-spark".to_string()),
            vec!["core/query".to_string()],
            4,
        )
        .unwrap();
        assert_eq!(entry.trigger_kind, WakeEntryTriggerKind::OnMemory);
        assert_eq!(entry.trigger_id, "proxima-test/fact-v1");
        assert_eq!(entry.probability_promille, 250);
        assert_eq!(entry.model_tier, ModelTier::Fast);
        assert_eq!(
            entry.inference_target_ref.as_deref(),
            Some("local-cli:codex-spark")
        );
    }

    #[test]
    fn wake_entry_rejects_probability_above_promille_ceiling() {
        let err = WakeEntryDraft::new(
            Uuid::from_u128(11),
            PersonalityInstanceId::new(Uuid::from_u128(2)),
            WakeEntryTriggerKind::OnMemory,
            "proxima-test/fact-v1",
            "on_test_fact",
            WakeEntryAuthoredBy::Any,
            1001,
            "recipe:proxima-test/personality-v1",
            ModelTier::Standard,
            None,
            Vec::new(),
            4,
        )
        .unwrap_err();
        assert!(err.to_string().contains("probability_promille"));
    }
}
