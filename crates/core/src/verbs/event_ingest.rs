//! `EventIngest` verb — typed surface only.
//!
//! See docs/14-protocol-surface.md §"`EventIngest`" and
//! docs/01-event-source.md §"Properties of an Event". The
//! storage-side body lives in `proxima-storage-pg` (M2.4b).

use uuid::Uuid;

use crate::verbs::schema::SidecarInserter;
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
pub struct InlineCitedObjectDraft {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub payload_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InlineCitationMappingDraft {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub payload_bytes: Vec<u8>,
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
    #[serde(default, skip)]
    pub rendered_text: Option<String>,
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

#[derive(Debug)]
pub struct AuthorizedInlineCitedObject {
    schema_id: SchemaId,
    schema_version: SchemaVersion,
    content_hash: [u8; 32],
    payload_bytes: Vec<u8>,
    sidecar_inserter_fn: SidecarInserter,
}

impl AuthorizedInlineCitedObject {
    pub(crate) fn new(
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        content_hash: [u8; 32],
        payload_bytes: Vec<u8>,
        sidecar_inserter_fn: SidecarInserter,
    ) -> Self {
        Self {
            schema_id,
            schema_version,
            content_hash,
            payload_bytes,
            sidecar_inserter_fn,
        }
    }

    #[must_use]
    pub fn schema_id(&self) -> &SchemaId {
        &self.schema_id
    }

    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    #[must_use]
    pub const fn content_hash(&self) -> &[u8; 32] {
        &self.content_hash
    }

    #[must_use]
    pub fn payload_bytes(&self) -> &[u8] {
        &self.payload_bytes
    }

    #[must_use]
    pub const fn sidecar_inserter_fn(&self) -> SidecarInserter {
        self.sidecar_inserter_fn
    }
}

#[derive(Debug)]
pub struct AuthorizedInlineCitationMapping {
    schema_id: SchemaId,
    schema_version: SchemaVersion,
    payload_bytes: Vec<u8>,
    /// `None` for a pure-link mapping with no sidecar table.
    sidecar_inserter_fn: Option<SidecarInserter>,
}

impl AuthorizedInlineCitationMapping {
    pub(crate) fn new(
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        payload_bytes: Vec<u8>,
        sidecar_inserter_fn: Option<SidecarInserter>,
    ) -> Self {
        Self {
            schema_id,
            schema_version,
            payload_bytes,
            sidecar_inserter_fn,
        }
    }

    #[must_use]
    pub fn schema_id(&self) -> &SchemaId {
        &self.schema_id
    }

    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    #[must_use]
    pub fn payload_bytes(&self) -> &[u8] {
        &self.payload_bytes
    }

    #[must_use]
    pub const fn sidecar_inserter_fn(&self) -> Option<SidecarInserter> {
        self.sidecar_inserter_fn
    }
}

/// Proof that a Fact ingest and its inline citation payloads passed
/// authorization, kind-specific schema validation, and citation
/// mapping target validation.
#[derive(Debug)]
pub struct AuthorizedFactWithCitation {
    draft: EventDraft,
    cited_object: AuthorizedInlineCitedObject,
    mapping: AuthorizedInlineCitationMapping,
    author_personality_instance_id: Option<PersonalityInstanceId>,
}

impl AuthorizedFactWithCitation {
    pub(crate) fn new(
        draft: EventDraft,
        cited_object: AuthorizedInlineCitedObject,
        mapping: AuthorizedInlineCitationMapping,
    ) -> Self {
        let author_personality_instance_id = draft.author_personality_instance_id;
        Self {
            draft,
            cited_object,
            mapping,
            author_personality_instance_id,
        }
    }

    #[must_use]
    pub fn draft(&self) -> &EventDraft {
        &self.draft
    }

    #[must_use]
    pub const fn cited_object(&self) -> &AuthorizedInlineCitedObject {
        &self.cited_object
    }

    #[must_use]
    pub const fn mapping(&self) -> &AuthorizedInlineCitationMapping {
        &self.mapping
    }

    #[must_use]
    pub const fn author_personality_instance_id(&self) -> Option<PersonalityInstanceId> {
        self.author_personality_instance_id
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
        let (kind, id, org_id) = owner.columns();
        hasher.update(kind.as_str().as_bytes());
        hasher.update(b"\x00");
        hasher.update(id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(org_id.as_bytes());
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

#[cfg(test)]
mod tests {
    use super::EventDraft;
    use crate::{
        OrgId, Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
        canonical_json_bytes,
    };
    use serde_json::json;
    use uuid::Uuid;

    fn draft(payload: Vec<u8>) -> EventDraft {
        let now = time::OffsetDateTime::UNIX_EPOCH;
        EventDraft {
            source_id: SourceId::new("test/source"),
            source_batch_id: SourceBatchId::new(Uuid::nil()),
            principal: Principal::User(UserId::new(
                Uuid::parse_str("018f0f4e-6b45-7c00-9bb5-b89b28d9c0a1").expect("uuid literal"),
            )),
            org_id: Some(OrgId::new(
                Uuid::parse_str("018f0f4e-6b45-7c00-9bb5-b89b28d9c0a2").expect("uuid literal"),
            )),
            author_personality_instance_id: None,
            schema_id: SchemaId::new("test/fact".to_string()),
            schema_version: SchemaVersion::new(1),
            payload,
            rendered_text: None,
            observed_at: now,
            occurred_at: now,
            citation: None,
        }
    }

    #[test]
    fn key_permuted_json_payloads_reencode_to_same_event_id() {
        let left = json!({
            "z": {
                "b": 2,
                "a": 1
            },
            "a": "same"
        });
        let right = json!({
            "a": "same",
            "z": {
                "a": 1,
                "b": 2
            }
        });

        let left = draft(canonical_json_bytes(&left));
        let right = draft(canonical_json_bytes(&right));

        assert_eq!(left.payload, right.payload);
        assert_eq!(left.event_id(), right.event_id());
    }
}
