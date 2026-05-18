//! Budget-review hook payloads and wake-entry policy.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{FactPayload, MemoryId, Owner, SourceBatchId, SourceId};
use crate::{PersonalityInstanceId, SchemaId, SchemaVersion};

pub const BUDGET_REVIEW_REQUESTED_SCHEMA_ID: &str = "core/budget-review-requested-v1";
pub const BUDGET_DECISION_SCHEMA_ID: &str = "core/budget-decision-v1";

pub const BUDGET_SOURCE_ID: &str = "core/budget-review";
pub const BUDGET_REVIEW_OBJECT_SCHEMA: &str = "core/budget-review-requested-object-v1";
pub const BUDGET_REVIEW_WHOLE_SCHEMA: &str = "core/budget-review-requested-whole-v1";
pub const BUDGET_DECISION_OBJECT_SCHEMA: &str = "core/budget-decision-object-v1";
pub const BUDGET_DECISION_WHOLE_SCHEMA: &str = "core/budget-decision-whole-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type)]
pub struct BudgetExhaustionPolicy {
    pub budgeter_personality_instance_id: Uuid,
    pub budget_extension_rounds: u16,
    pub budget_hard_cap_rounds: u16,
    pub budget_progress_contract: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type, sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.budget_decision_kind")]
#[serde(rename_all = "snake_case")]
pub enum BudgetDecisionKind {
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

impl BudgetDecisionKind {
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
pub struct BudgetReviewRequestedV1 {
    pub original_invocation_id: Uuid,
    pub original_wake_entry_id: Uuid,
    pub original_personality_instance_id: Uuid,
    pub original_change_event_seq: Uuid,
    pub triggering_memory_id: Uuid,
    pub wake_trace_memory_id: Uuid,
    pub target_budgeter_personality_instance_id: Uuid,
    pub max_rounds: u16,
    pub rounds_used: u16,
    pub budget_extension_rounds: u16,
    pub budget_hard_cap_rounds: u16,
    pub continued_rounds_used: u16,
    pub active_goal_ids: Vec<Uuid>,
    pub progress_contract: String,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: OffsetDateTime,
}

impl FactPayload for BudgetReviewRequestedV1 {
    const SCHEMA_ID: &'static str = BUDGET_REVIEW_REQUESTED_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.budget_review_requested_v1"
    }

    fn render(&self) -> String {
        format!(
            "Budget review requested: invocation {} used {}/{} rounds",
            self.original_invocation_id, self.rounds_used, self.max_rounds
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct BudgetDecisionV1 {
    pub budget_request_memory_id: Uuid,
    pub decision: BudgetDecisionKind,
    #[serde(default)]
    pub grant_rounds: Option<u16>,
    #[serde(default)]
    pub redirect_personality_instance_id: Option<Uuid>,
    pub rationale: String,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub decided_at: OffsetDateTime,
}

impl FactPayload for BudgetDecisionV1 {
    const SCHEMA_ID: &'static str = BUDGET_DECISION_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.budget_decision_v1"
    }

    fn render(&self) -> String {
        format!("Budget decision: {}", self.decision.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct BudgetReviewPersistInput {
    pub owner: Owner,
    pub root_perspective_memory_id: MemoryId,
    pub request: BudgetReviewRequestedV1,
    pub source_batch_id: SourceBatchId,
    pub source_id: SourceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetReviewPersistOutcome {
    pub memory_id: MemoryId,
    pub change_event_seq: Uuid,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone)]
pub struct BudgetReviewWakeRequest {
    pub owner: Owner,
    pub request_memory_id: MemoryId,
    pub change_event_seq: Uuid,
    pub budgeter_personality_instance_id: PersonalityInstanceId,
}

pub fn budget_review_event_draft(
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
        schema_id: SchemaId::new(BUDGET_REVIEW_REQUESTED_SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(1),
        payload: payload.to_vec(),
        observed_at,
        occurred_at: observed_at,
        cited_object: crate::verbs::event_ingest::CitedObjectHint {
            schema_id: SchemaId::new(BUDGET_REVIEW_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: crate::verbs::event_ingest::CitationMappingHint {
            schema_id: SchemaId::new(BUDGET_REVIEW_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    }
}

pub fn budget_decision_event_draft(
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
        schema_id: SchemaId::new(BUDGET_DECISION_SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(1),
        payload: payload.to_vec(),
        observed_at,
        occurred_at: observed_at,
        cited_object: crate::verbs::event_ingest::CitedObjectHint {
            schema_id: SchemaId::new(BUDGET_DECISION_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: crate::verbs::event_ingest::CitationMappingHint {
            schema_id: SchemaId::new(BUDGET_DECISION_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    }
}
