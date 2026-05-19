//! Personality draft types.
//!
//! This module contains draft and input types for personality operations:
//! - `WakeEntryDraft` - Draft of a wake entry configuration
//! - `SidecarSpec` - Sidecar table specification
//! - `PersonalityMemoryDraft` - Draft of a personality memory
//! - `PersonalityWriteRequest` - Request to write personality memories
//! - `PersonalityWriteOutcome` - Outcome of a personality write

use uuid::Uuid;

use crate::error::ProtocolError;
use crate::intervention::InterventionPolicy;
use crate::personality::types::{
    PersonalityMemoryKind, WakeChainDepth, WakeEntryAuthoredBy, WakeEntryGoalScope,
    WakeEntryTriggerKind, WakeExecutionMode,
};
use crate::{MemoryId, ModelTier, Owner, RegisteredRelation, SchemaId, SchemaVersion};

use super::personality::{PersonalityInstanceId, PersonalityRef};

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
    pub goal_scope: WakeEntryGoalScope,
    pub instructions: String,
    pub model_tier: ModelTier,
    pub inference_target_ref: Option<String>,
    pub substrate_tool_palette: Vec<String>,
    pub workspace_tool_palette: Vec<String>,
    pub max_rounds: u16,
    pub intervention_policy: Option<InterventionPolicy>,
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
            goal_scope: WakeEntryGoalScope::None,
            instructions: String::new(),
            model_tier,
            inference_target_ref,
            substrate_tool_palette,
            workspace_tool_palette: Vec::new(),
            max_rounds,
            intervention_policy: None,
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

/// Request to write personality memories.
#[derive(Debug)]
pub struct PersonalityWriteRequest<'a> {
    pub owner: Owner,
    pub instance: PersonalityRef,
    pub model_id: &'a str,
    pub prompt_version: &'a str,
    pub provenance_relation: RegisteredRelation<'a>,
    pub supersedes_relation: RegisteredRelation<'a>,
    /// Substrate-only `core/authored` relation. Storage writes one
    /// `Root Perspective --core/authored--> emitted memory` edge per
    /// memory in the same transaction so graph traversal can answer
    /// "what has this Personality produced?" without falling back to
    /// the `personality_instance_id` row column.
    pub authored_relation: RegisteredRelation<'a>,
    /// Snapshot of the Personality's Root Perspective memory_id taken
    /// at wake-context assembly time. Used as the `source_memory_id`
    /// of the auto-wired `core/authored` edge so the edge attributes
    /// to the perspective that was speaking during this wake, not
    /// whatever the runtime row points to at edge-write time.
    pub current_root_perspective_memory_id: MemoryId,
    pub wake_chain_depth: WakeChainDepth,
    pub memories: &'a [PersonalityMemoryDraft],
    pub sidecar_table: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalityWriteOutcome {
    pub memory_ids: Vec<MemoryId>,
}
