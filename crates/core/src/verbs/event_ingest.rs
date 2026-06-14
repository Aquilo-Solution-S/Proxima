//! `EventIngest` verb — typed surface only.
//!
//! See docs/14-protocol-surface.md §"`EventIngest`" and
//! docs/01-event-source.md §"Properties of an Event". The
//! storage-side body lives in `proxima-storage-pg` (M2.4b).

use uuid::Uuid;

use crate::{
    EventId, MemoryId, OrgId, Owner, PersonalityInstanceId, Principal, SchemaId, SchemaVersion,
    SourceBatchId, SourceId,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CitedObjectHint {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub content_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CitationMappingHint {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Citation {
    pub object: CitedObjectHint,
    pub mapping: CitationMappingHint,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EventDraft {
    pub source_id: SourceId,
    pub source_batch_id: SourceBatchId,
    pub principal: Principal,
    #[serde(skip)]
    pub org_id: Option<OrgId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_personality_instance_id: Option<PersonalityInstanceId>,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub payload: Vec<u8>,
    pub observed_at: time::OffsetDateTime,
    pub occurred_at: time::OffsetDateTime,
    pub citation: Option<Citation>,
}

/// Proof that an event ingest passed authorization + schema
/// validation and had its owner stamped from the authz context.
/// The only constructor is `Engine::authorize_event_ingest`, so a
/// caller cannot reach the sidecar-ingest primitive below the gate.
#[derive(Debug)]
pub struct AuthorizedEventIngest {
    draft: EventDraft,
}

impl AuthorizedEventIngest {
    pub(crate) fn new(draft: EventDraft) -> Self {
        Self { draft }
    }

    #[must_use]
    pub fn draft(&self) -> &EventDraft {
        &self.draft
    }
}

impl EventDraft {
    /// Reconstructs the storage `Owner` after verb-layer stamping.
    ///
    /// # Panics
    ///
    /// Panics if `stamp_owner` has not populated `org_id` before storage or hash use.
    #[must_use]
    pub fn owner(&self) -> Owner {
        Owner {
            principal: self.principal.clone(),
            org_id: self
                .org_id
                .expect("EventDraft org_id must be stamped before storage/hash use"),
        }
    }

    pub fn stamp_owner(&mut self, stamped: Owner) {
        self.principal = stamped.principal;
        self.org_id = Some(stamped.org_id);
    }

    /// Canonical `event_id` per docs/01: BLAKE3 of
    /// `source_id` || `owner_components` || payload, separated by
    /// 0x00 bytes. Re-receipt of the same observation
    /// produces the same hash by construction.
    #[must_use]
    pub fn event_id(&self) -> EventId {
        let owner = self.owner();
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.source_id.as_str().as_bytes());
        hasher.update(b"\x00");
        let (kind, id) = match &owner.principal {
            Principal::User(u) => ("User", u.into_inner()),
            Principal::Group(g) => ("Group", g.into_inner()),
        };
        hasher.update(kind.as_bytes());
        hasher.update(b"\x00");
        hasher.update(id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(owner.org_id.into_inner().as_bytes());
        hasher.update(b"\x00");
        hasher.update(&self.payload);
        EventId::new(*hasher.finalize().as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EventIngestOutcome {
    pub event_id: EventId,
    pub memory_id: MemoryId,
    pub change_event_seq: Uuid,
    /// True iff the same `event_id` was already ingested.
    /// docs/14 §`EventIngest`: "replay is silently a no-op."
    pub idempotent_replay: bool,
}
