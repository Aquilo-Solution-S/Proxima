//! Wake intervention hook payloads and wake-entry policy.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use async_trait::async_trait;

use crate::verbs::event_ingest::EventDraft;
use crate::{
    FactPayload, FlavorRegistryFrozen, MemoryId, Owner, SearchProjection,
    SearchProjectionColumnKind, SearchProjectionField, SourceBatchId, SourceId, StorageError,
};
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

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[SearchProjectionField {
                column: "progress_contract",
                kind: SearchProjectionColumnKind::Text,
            }],
        })
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

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[
                SearchProjectionField {
                    column: "decision",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "rationale",
                    kind: SearchProjectionColumnKind::Text,
                },
            ],
        })
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterventionContinueCandidate {
    pub intervention_decision_memory_id: MemoryId,
    pub intervention_request_memory_id: MemoryId,
    pub original_invocation_id: Uuid,
    pub original_wake_entry_id: Uuid,
    pub original_personality_instance_id: PersonalityInstanceId,
    pub original_change_event_seq: Uuid,
    pub original_triggering_memory_id: MemoryId,
    pub wake_trace_memory_id: MemoryId,
    pub grant_rounds: u16,
    pub rationale: String,
}

#[must_use]
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

#[must_use]
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

// ---------------------------------------------------------------------------
// InterventionStore — the storage capability the intervention tool depends on.
//
// A supertrait of `Storage` (see `crate::storage`). All data access for the
// `core/emit_intervention_decision` tool lives behind this trait; the Postgres
// implementation is in `proxima-storage-pg`. The tool never touches a
// `PgPool` — it builds typed payloads and hands them to these verbs. Default
// method bodies keep non-Postgres `Storage` impls (test fakes, `NoopStorage`)
// trivial.
// ---------------------------------------------------------------------------

/// An `InterventionRequested` Fact loaded for decision evaluation.
#[derive(Debug, Clone)]
pub struct LoadedInterventionRequest {
    pub memory_id: MemoryId,
    pub target_intervention_personality_instance_id: Uuid,
    pub intervention_extension_rounds: i32,
    pub intervention_hard_cap_rounds: i32,
}

/// Input to [`InterventionStore::emit_intervention_decision_atomic`].
#[derive(Debug, Clone)]
pub struct EmitInterventionDecisionInput {
    pub owner: Owner,
    pub payload: InterventionDecisionV1,
    /// `Self` Perspective of the Wake Supervisor authoring the decision:
    /// source of the `core/authored` edge and the authorship owner of
    /// both provenance edges.
    pub caller_self: MemoryId,
}

/// Outcome of [`InterventionStore::emit_intervention_decision_atomic`].
#[derive(Debug, Clone)]
pub struct InterventionDecisionEmitOutcome {
    pub memory_id: MemoryId,
    pub idempotent_replay: bool,
}

/// Build the `EventDraft` for an intervention-decision Fact: CBOR-encode
/// the typed payload and wire the content-addressing schema ids. Pure
/// (no I/O) — the storage verb calls this before opening its transaction.
///
/// # Errors
///
/// Returns `StorageError::Internal` if `payload` fails to CBOR-encode.
pub fn intervention_decision_fact_event_draft(
    owner: &Owner,
    payload: &InterventionDecisionV1,
) -> Result<EventDraft, StorageError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes).map_err(|err| {
        StorageError::Internal(format!("serialize intervention decision payload: {err}"))
    })?;
    Ok(intervention_decision_event_draft(
        owner.clone(),
        &payload_bytes,
        SourceBatchId::new(Uuid::now_v7()),
        SourceId::new(INTERVENTION_SOURCE_ID),
        payload.decided_at,
    ))
}

fn intervention_store_unimplemented(verb: &str) -> StorageError {
    StorageError::Internal(format!(
        "storage backend does not implement InterventionStore::{verb}"
    ))
}

#[async_trait]
pub trait InterventionStore: Send + Sync {
    /// Load the `InterventionRequested` Fact backing `memory_id`, or `None`
    /// when it is not an intervention request visible to `owner`.
    async fn load_intervention_request(
        &self,
        _owner: &Owner,
        _memory_id: MemoryId,
    ) -> Result<Option<LoadedInterventionRequest>, StorageError> {
        Ok(None)
    }

    /// Memory id of an intervention decision already emitted for
    /// `(request, idempotency_key)`, enabling idempotent replay.
    async fn existing_intervention_decision(
        &self,
        _owner: &Owner,
        _intervention_request_memory_id: MemoryId,
        _idempotency_key: &str,
    ) -> Result<Option<MemoryId>, StorageError> {
        Ok(None)
    }

    /// Whether `caller_self` is the active Wake Supervisor personality
    /// `target_personality_instance_id` for `owner`.
    async fn is_intervention_supervisor(
        &self,
        _owner: &Owner,
        _caller_self: MemoryId,
        _target_personality_instance_id: Uuid,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }

    /// Sum of `grant_rounds` across prior `continue` decisions for the
    /// request, used to enforce the intervention hard cap.
    async fn prior_continue_grant_rounds(
        &self,
        _owner: &Owner,
        _intervention_request_memory_id: MemoryId,
    ) -> Result<i64, StorageError> {
        Ok(0)
    }

    /// Atomically materialize an intervention-decision Fact, its sidecar
    /// row, and the `core/authored` + `core/derived-from` provenance edges.
    async fn emit_intervention_decision_atomic(
        &self,
        _registry: &FlavorRegistryFrozen,
        _input: &EmitInterventionDecisionInput,
    ) -> Result<InterventionDecisionEmitOutcome, StorageError> {
        Err(intervention_store_unimplemented(
            "emit_intervention_decision_atomic",
        ))
    }
}
