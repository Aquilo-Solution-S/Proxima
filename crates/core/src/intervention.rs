//! Wake intervention hook payloads and wake-entry policy.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{FactPayload, MemoryId, Owner, SourceBatchId, SourceId};
use crate::{PersonalityInstanceId, SchemaId, SchemaVersion};

pub const INTERVENTION_REQUESTED_SCHEMA_ID: &str = "core/intervention-requested-v1";
pub const INTERVENTION_DECISION_SCHEMA_ID: &str = "core/intervention-decision-v1";

pub const INTERVENTION_SOURCE_ID: &str = "core/intervention";
pub const INTERVENTION_REQUESTED_OBJECT_SCHEMA: &str = "core/intervention-requested-object-v1";
pub const INTERVENTION_REQUESTED_WHOLE_SCHEMA: &str = "core/intervention-requested-whole-v1";
pub const INTERVENTION_DECISION_OBJECT_SCHEMA: &str = "core/intervention-decision-object-v1";
pub const INTERVENTION_DECISION_WHOLE_SCHEMA: &str = "core/intervention-decision-whole-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type)]
pub struct InterventionPolicy {
    pub intervention_personality_instance_id: Uuid,
    pub intervention_extension_rounds: u16,
    pub intervention_hard_cap_rounds: u16,
    pub intervention_progress_contract: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type, sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.intervention_decision_kind")]
#[serde(rename_all = "snake_case")]
pub enum InterventionDecisionKind {
    #[sqlx(rename = "continue")]
    Continue,
    #[sqlx(rename = "stop")]
    Stop,
    #[sqlx(rename = "redirect")]
    Redirect,
    #[sqlx(rename = "decompose")]
    Decompose,
    #[sqlx(rename = "accept_terminal")]
    AcceptTerminal,
}

impl InterventionDecisionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Stop => "stop",
            Self::Redirect => "redirect",
            Self::Decompose => "decompose",
            Self::AcceptTerminal => "accept_terminal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct InterventionRequestedV1 {
    pub original_invocation_id: Uuid,
    pub original_wake_entry_id: Uuid,
    pub original_personality_instance_id: Uuid,
    pub original_change_event_seq: Uuid,
    pub triggering_memory_id: Uuid,
    pub wake_trace_memory_id: Uuid,
    pub target_intervention_personality_instance_id: Uuid,
    pub max_rounds: u16,
    pub rounds_used: u16,
    pub intervention_extension_rounds: u16,
    pub intervention_hard_cap_rounds: u16,
    pub continued_rounds_used: u16,
    pub active_goal_ids: Vec<Uuid>,
    pub progress_contract: String,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: OffsetDateTime,
}

impl FactPayload for InterventionRequestedV1 {
    const SCHEMA_ID: &'static str = INTERVENTION_REQUESTED_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.intervention_requested_v1"
    }

    fn render(&self) -> String {
        format!(
            "Intervention requested: invocation {} used {}/{} rounds",
            self.original_invocation_id, self.rounds_used, self.max_rounds
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct InterventionDecisionV1 {
    pub intervention_request_memory_id: Uuid,
    pub decision: InterventionDecisionKind,
    #[serde(default)]
    pub grant_rounds: Option<u16>,
    #[serde(default)]
    pub redirect_personality_instance_id: Option<Uuid>,
    pub rationale: String,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub decided_at: OffsetDateTime,
}

impl FactPayload for InterventionDecisionV1 {
    const SCHEMA_ID: &'static str = INTERVENTION_DECISION_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.intervention_decision_v1"
    }

    fn render(&self) -> String {
        format!("Intervention decision: {}", self.decision.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct InterventionRequestPersistInput {
    pub owner: Owner,
    pub root_perspective_memory_id: MemoryId,
    pub request: InterventionRequestedV1,
    pub source_batch_id: SourceBatchId,
    pub source_id: SourceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterventionRequestPersistOutcome {
    pub memory_id: MemoryId,
    pub change_event_seq: Uuid,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone)]
pub struct InterventionWakeRequest {
    pub owner: Owner,
    pub request_memory_id: MemoryId,
    pub change_event_seq: Uuid,
    pub intervention_personality_instance_id: PersonalityInstanceId,
}

pub fn intervention_request_event_draft(
    owner: Owner,
    payload: &[u8],
    source_batch_id: SourceBatchId,
    source_id: SourceId,
    observed_at: OffsetDateTime,
) -> crate::verbs::event_ingest::EventDraft {
    let content_hash = blake3::hash(payload);
    crate::verbs::event_ingest::EventDraft {
        source_id,
        source_batch_id,
        owner,
        schema_id: SchemaId::new(INTERVENTION_REQUESTED_SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(1),
        payload: payload.to_vec(),
        observed_at,
        occurred_at: observed_at,
        cited_object: crate::verbs::event_ingest::CitedObjectHint {
            schema_id: SchemaId::new(INTERVENTION_REQUESTED_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: crate::verbs::event_ingest::CitationMappingHint {
            schema_id: SchemaId::new(INTERVENTION_REQUESTED_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    }
}

pub fn intervention_decision_event_draft(
    owner: Owner,
    payload: &[u8],
    source_batch_id: SourceBatchId,
    source_id: SourceId,
    observed_at: OffsetDateTime,
) -> crate::verbs::event_ingest::EventDraft {
    let content_hash = blake3::hash(payload);
    crate::verbs::event_ingest::EventDraft {
        source_id,
        source_batch_id,
        owner,
        schema_id: SchemaId::new(INTERVENTION_DECISION_SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(1),
        payload: payload.to_vec(),
        observed_at,
        occurred_at: observed_at,
        cited_object: crate::verbs::event_ingest::CitedObjectHint {
            schema_id: SchemaId::new(INTERVENTION_DECISION_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: crate::verbs::event_ingest::CitationMappingHint {
            schema_id: SchemaId::new(INTERVENTION_DECISION_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    }
}
