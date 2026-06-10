//! `EventIngest` verb — typed surface only.
//!
//! See docs/14-protocol-surface.md §"`EventIngest`" and
//! docs/01-event-source.md §"Properties of an Event". The
//! storage-side body lives in `proxima-storage-pg` (M2.4b).

use uuid::Uuid;

use crate::{
    EventId, MemoryId, Owner, Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CitedObjectHint {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub content_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CitationMappingHint {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct EventDraft {
    pub source_id: SourceId,
    pub source_batch_id: SourceBatchId,
    pub owner: Owner,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub payload: Vec<u8>,
    pub observed_at: time::OffsetDateTime,
    pub occurred_at: time::OffsetDateTime,
    pub cited_object: CitedObjectHint,
    pub citation_mapping: CitationMappingHint,
}

impl EventDraft {
    /// Canonical `event_id` per docs/01: BLAKE3 of
    /// `source_id` || `owner_components` || payload, separated by
    /// 0x00 bytes. Re-receipt of the same observation
    /// produces the same hash by construction.
    #[must_use]
    pub fn event_id(&self) -> EventId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.source_id.as_str().as_bytes());
        hasher.update(b"\x00");
        let (kind, id) = match &self.owner.principal {
            Principal::User(u) => ("User", u.into_inner()),
            Principal::Group(g) => ("Group", g.into_inner()),
        };
        hasher.update(kind.as_bytes());
        hasher.update(b"\x00");
        hasher.update(id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(self.owner.org_id.into_inner().as_bytes());
        hasher.update(b"\x00");
        hasher.update(&self.payload);
        EventId::new(*hasher.finalize().as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct EventIngestOutcome {
    pub event_id: EventId,
    pub memory_id: MemoryId,
    pub change_event_seq: Uuid,
    /// True iff the same `event_id` was already ingested.
    /// docs/14 §`EventIngest`: "replay is silently a no-op."
    pub idempotent_replay: bool,
}
