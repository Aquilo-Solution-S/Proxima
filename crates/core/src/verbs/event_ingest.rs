//! `EventIngest` verb — typed surface only.
//!
//! See docs/14-protocol-surface.md §"`EventIngest`" and
//! docs/01-event-source.md §"Properties of an Event". The
//! storage-side body lives in `proxima-storage-pg` (M2.4b).

use uuid::Uuid;

use crate::verbs::schema::SidecarInserter;
use crate::{
    EventId, FactPayload, MemoryId, OrgId, Owner, PersonalityInstanceId, Principal, SchemaId,
    SchemaVersion, SourceBatchId, SourceId, canonical_json_bytes,
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

/// Compact citation input for the common opaque-object / pure-link
/// mapping path used by event-source Fact ingest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationSpec {
    pub cited_object_schema_id: SchemaId,
    pub cited_object_schema_version: SchemaVersion,
    pub content_hash: [u8; 32],
    pub mapping_schema_id: SchemaId,
    pub mapping_schema_version: SchemaVersion,
}

impl CitationSpec {
    /// Build a v1 opaque cited-object / citation-mapping spec.
    #[must_use]
    pub fn v1(
        cited_object_schema_id: impl Into<String>,
        content_hash: [u8; 32],
        mapping_schema_id: impl Into<String>,
    ) -> Self {
        Self {
            cited_object_schema_id: SchemaId::new(cited_object_schema_id.into()),
            cited_object_schema_version: SchemaVersion::new(1),
            content_hash,
            mapping_schema_id: SchemaId::new(mapping_schema_id.into()),
            mapping_schema_version: SchemaVersion::new(1),
        }
    }

    /// Build a v1 citation spec whose cited-object content hash is the
    /// BLAKE3 hash of the typed payload's canonical JSON bytes.
    ///
    /// # Errors
    ///
    /// Returns the serde error if the payload cannot be converted to a JSON
    /// value for canonical encoding.
    pub fn v1_for_payload<P: serde::Serialize>(
        cited_object_schema_id: impl Into<String>,
        payload: &P,
        mapping_schema_id: impl Into<String>,
    ) -> Result<Self, serde_json::Error> {
        let value = serde_json::to_value(payload)?;
        Ok(Self::v1(
            cited_object_schema_id,
            *blake3::hash(&canonical_json_bytes(&value)).as_bytes(),
            mapping_schema_id,
        ))
    }
}

impl From<CitationSpec> for Citation {
    fn from(spec: CitationSpec) -> Self {
        Self {
            object: CitedObjectHint {
                schema_id: spec.cited_object_schema_id,
                schema_version: spec.cited_object_schema_version,
                content_hash: spec.content_hash,
            },
            mapping: CitationMappingHint {
                schema_id: spec.mapping_schema_id,
                schema_version: spec.mapping_schema_version,
            },
        }
    }
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
    fact_sidecar_table: Option<String>,
    fact_natural_key_columns: Vec<String>,
}

impl AuthorizedEventIngest {
    pub(crate) fn new(
        draft: EventDraft,
        fact_sidecar_table: Option<String>,
        fact_natural_key_columns: Vec<String>,
    ) -> Self {
        Self {
            draft,
            fact_sidecar_table,
            fact_natural_key_columns,
        }
    }

    #[must_use]
    pub fn draft(&self) -> &EventDraft {
        &self.draft
    }

    #[must_use]
    pub fn fact_sidecar_table(&self) -> Option<&str> {
        self.fact_sidecar_table.as_deref()
    }

    #[must_use]
    pub fn fact_natural_key_columns(&self) -> &[String] {
        &self.fact_natural_key_columns
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

/// Proof that an existing Fact memory and inline citation payloads
/// passed authorization, kind-specific schema validation, and citation
/// mapping target validation.
#[derive(Debug)]
pub struct AuthorizedCitationAttachment {
    memory_id: MemoryId,
    owner: Owner,
    cited_object: AuthorizedInlineCitedObject,
    mapping: AuthorizedInlineCitationMapping,
}

impl AuthorizedCitationAttachment {
    pub(crate) fn new(
        memory_id: MemoryId,
        owner: Owner,
        cited_object: AuthorizedInlineCitedObject,
        mapping: AuthorizedInlineCitationMapping,
    ) -> Self {
        Self {
            memory_id,
            owner,
            cited_object,
            mapping,
        }
    }

    #[must_use]
    pub const fn memory_id(&self) -> MemoryId {
        self.memory_id
    }

    #[must_use]
    pub const fn owner(&self) -> &Owner {
        &self.owner
    }

    #[must_use]
    pub const fn cited_object(&self) -> &AuthorizedInlineCitedObject {
        &self.cited_object
    }

    #[must_use]
    pub const fn mapping(&self) -> &AuthorizedInlineCitationMapping {
        &self.mapping
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
    fact_sidecar_table: Option<String>,
    fact_natural_key_columns: Vec<String>,
}

impl AuthorizedFactWithCitation {
    pub(crate) fn new(
        draft: EventDraft,
        cited_object: AuthorizedInlineCitedObject,
        mapping: AuthorizedInlineCitationMapping,
        fact_sidecar_table: Option<String>,
        fact_natural_key_columns: Vec<String>,
    ) -> Self {
        let author_personality_instance_id = draft.author_personality_instance_id;
        Self {
            draft,
            cited_object,
            mapping,
            author_personality_instance_id,
            fact_sidecar_table,
            fact_natural_key_columns,
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

    #[must_use]
    pub fn fact_sidecar_table(&self) -> Option<&str> {
        self.fact_sidecar_table.as_deref()
    }

    #[must_use]
    pub fn fact_natural_key_columns(&self) -> &[String] {
        &self.fact_natural_key_columns
    }
}

impl EventDraft {
    /// Build a Fact event draft from a typed payload using the payload's
    /// schema id/version and canonical JSON encoding. The caller supplies
    /// the source and source-batch boundary explicitly; no runtime
    /// registration or source inference happens here.
    ///
    /// # Errors
    ///
    /// Returns the serde error if the typed payload cannot be converted to
    /// a JSON value for canonical event bytes.
    pub fn from_payload<P: FactPayload>(
        owner: &Owner,
        source_id: impl Into<String>,
        source_batch_id: SourceBatchId,
        payload: &P,
        observed_at: time::OffsetDateTime,
    ) -> Result<Self, serde_json::Error> {
        let value = serde_json::to_value(payload)?;
        Ok(Self {
            source_id: SourceId::new(source_id.into()),
            source_batch_id,
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            author_personality_instance_id: None,
            schema_id: P::schema_id(),
            schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
            payload: canonical_json_bytes(&value),
            rendered_text: None,
            observed_at,
            occurred_at: observed_at,
            citation: None,
        })
    }

    /// Attach an opaque citation hint to the draft.
    #[must_use]
    pub fn with_citation(mut self, citation: impl Into<Citation>) -> Self {
        self.citation = Some(citation.into());
        self
    }

    /// Override the occurrence time while preserving the observation time.
    #[must_use]
    pub const fn occurred_at(mut self, occurred_at: time::OffsetDateTime) -> Self {
        self.occurred_at = occurred_at;
        self
    }

    /// Stamp the authoring personality instance for agent-authored Facts.
    #[must_use]
    pub const fn author_personality(mut self, author: PersonalityInstanceId) -> Self {
        self.author_personality_instance_id = Some(author);
        self
    }

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
