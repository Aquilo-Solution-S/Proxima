//! `persist_wake_trace` verb typed surface.
//!
//! Atomic write of the wake-trace Fact, JSONL `CitedObject`,
//! `CitationMapping`, sidecar payload rows, change event, and canonical
//! authorship/provenance edges. Storage implementation lives behind
//! `Storage::persist_wake_trace_atomic` so core stays backend-neutral.

use uuid::Uuid;

use crate::wake::trace::WakeTracePayload;
use crate::{EventId, GoalId, MemoryId, Owner, Principal, SchemaVersion, SourceBatchId, SourceId};

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
