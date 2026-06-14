//! `persist_wake_trace` verb typed surface.
//!
//! Atomic write of the wake-trace Fact, JSONL `CitedObject`,
//! `CitationMapping`, sidecar payload rows, change event, and canonical
//! authorship/provenance edges. Storage implementation lives behind
//! `Storage::persist_wake_trace_atomic` so core stays backend-neutral.

use uuid::Uuid;

use crate::personality::WakeTraceOutcomeKind;
use crate::{
    CitationMappingPayload, CitedObjectPayload, EventId, FactPayload, GoalId, MemoryId, Owner,
    Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId, proxima_schema_id,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WakeTracePayload {
    pub invocation_id: Uuid,
    pub wake_entry_id: Uuid,
    pub personality_instance_id: Uuid,
    pub model_target_ref: String,
    pub model_id: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub started_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub finished_at: time::OffsetDateTime,
    pub outcome_kind: WakeTraceOutcomeKind,
    pub failure_reason: Option<String>,
    pub rounds_used: u32,
    pub finish_reason: Option<String>,
    pub total_prompt_tokens: Option<u64>,
    pub total_completion_tokens: Option<u64>,
    pub tool_call_count: u32,
    pub jsonl_truncated: bool,
}

impl FactPayload for WakeTracePayload {
    const SCHEMA_ID: &'static str = proxima_schema_id!("wake-trace-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!(
            "Wake {} {} ({} rounds)",
            self.invocation_id,
            self.outcome_kind.as_str(),
            self.rounds_used
        )
    }

    fn sidecar_table() -> &'static str {
        "proxima_core.wake_trace_v1"
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WakeTraceJsonlPayload {
    pub content_hash: [u8; 32],
    pub byte_len: u64,
    pub line_count: u64,
    pub truncated: bool,
}

impl CitedObjectPayload for WakeTraceJsonlPayload {
    const SCHEMA_ID: &'static str = proxima_schema_id!("wake-trace-jsonl-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.cited_wake_trace_jsonl_v1"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        self.content_hash
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WakeTraceCitationPayload {
    pub byte_range_start: Option<u64>,
    pub byte_range_end: Option<u64>,
}

impl CitationMappingPayload for WakeTraceCitationPayload {
    const SCHEMA_ID: &'static str = proxima_schema_id!("wake-trace-citation-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.citation_wake_trace_v1"
    }

    fn cited_object_schema() -> SchemaId {
        SchemaId::new(proxima_schema_id!("wake-trace-jsonl-v1").to_string())
    }
}

#[derive(Debug, Clone)]
pub struct WakeTracePersistInput {
    pub owner: Owner,
    pub authoring_personality_instance_id: Uuid,
    pub root_perspective_memory_id: MemoryId,
    pub triggering_memory_id: MemoryId,
    pub active_goal_ids: Vec<GoalId>,
    pub jsonl_bytes: Vec<u8>,
    pub jsonl_content_hash: [u8; 32],
    pub jsonl_line_count: u64,
    pub jsonl_truncated: bool,
    pub citation_byte_range: Option<(u64, u64)>,
    pub wake_trace: WakeTracePayload,
    pub source_id: SourceId,
    pub source_batch_id: SourceBatchId,
    pub observed_at: time::OffsetDateTime,
    pub occurred_at: time::OffsetDateTime,
}

impl WakeTracePersistInput {
    /// Whole-verb replay key. Distinct wake invocations with identical
    /// JSONL share a `CitedObject` row, but they do not share this event id.
    #[must_use]
    pub fn event_id(&self) -> EventId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.source_id.as_str().as_bytes());
        hasher.update(b"\0");
        let (kind, id) = match &self.owner.principal {
            Principal::User(u) => ("User", u.into_inner()),
            Principal::Group(g) => ("Group", g.into_inner()),
        };
        hasher.update(kind.as_bytes());
        hasher.update(b"\0");
        hasher.update(id.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.owner.org_id.into_inner().as_bytes());
        hasher.update(b"\0");
        hasher.update(&self.jsonl_content_hash);
        hasher.update(b"\0");
        hasher.update(self.wake_trace.invocation_id.as_bytes());
        EventId::new(*hasher.finalize().as_bytes())
    }

    #[must_use]
    pub fn fact_schema_version(&self) -> SchemaVersion {
        SchemaVersion::new(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeTracePersistOutcome {
    pub event_id: EventId,
    pub fact_memory_id: MemoryId,
    pub cited_object_id: Uuid,
    pub citation_mapping_id: Uuid,
    pub change_event_seq: Uuid,
    pub idempotent_replay: bool,
}
