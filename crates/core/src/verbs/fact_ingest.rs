//! `FactIngest` verb — typed surface only.
//!
//! See docs/14-protocol-surface.md §"Fact write" and
//! docs/01-event-source.md §"Fact membrane". The storage-side body
//! lives in `proxima-storage-pg`.

use uuid::Uuid;

use crate::edge::EdgeEndpoint;
use crate::engine::MemoryPermit;
use crate::storage_ports::OwnerWritePermit;
use crate::{
    FactPayload, FactReceiptId, MemoryId, Owner, OwnerRefKind, SchemaId, SchemaVersion,
    SidecarPayload, SourceBatchId, SourceId,
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
/// mapping path used by Fact membrane ingest.
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
    /// BLAKE3 hash of the Fact payload's schema-owned receipt key.
    #[must_use]
    pub fn v1_for_payload<P: FactPayload>(
        cited_object_schema_id: impl Into<String>,
        payload: &P,
        mapping_schema_id: impl Into<String>,
    ) -> Self {
        let payload = payload.receipt_key();
        Self::v1(
            cited_object_schema_id,
            *blake3::hash(&payload).as_bytes(),
            mapping_schema_id,
        )
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
pub struct FactReceiptDraft {
    pub source_id: SourceId,
    pub source_batch_id: SourceBatchId,
    pub observed_at: time::OffsetDateTime,
    pub occurred_at: time::OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FactWriteCommand {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    /// Schema-owned receipt replay key material. The typed payload itself
    /// lives in the registered sidecar.
    pub payload: Vec<u8>,
    #[serde(default, skip)]
    pub rendered_text: Option<String>,
    /// Text-search configuration to stamp on the memory row, resolved by
    /// [`crate::lexical_language::resolve_lexical_language`]; `None`
    /// applies the database default. `skip` like `rendered_text`: the
    /// language describes how the text is indexed, it is not receipt
    /// key material, and replaying an import with detection newly
    /// enabled must stay a replay.
    #[serde(default, skip)]
    pub lexical_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<FactReceiptDraft>,
    pub citation: Option<Citation>,
    /// What this Fact was made from — an OCR reading declaring the
    /// upload it read, say. Each entry becomes an
    /// [`crate::EdgeKind::Origin`] index row inside the Fact's own write
    /// transaction, which is what makes the provenance idempotent
    /// without an id scheme: replaying the ingest re-asserts the same
    /// primary key.
    ///
    /// Not receipt key material and `skip`ped like `rendered_text`: the
    /// same observation is the same Fact whether or not a caller repeats
    /// the declaration, and a receipt replay must stay a replay.
    #[serde(default, skip)]
    pub derived_from: Vec<EdgeEndpoint>,
}

/// Proof that a Fact write passed authorization + schema validation
/// and had its owner stamped from the authz context.
/// The only constructor is `Engine::authorize_fact_ingest`, so a
/// caller cannot reach the sidecar-ingest primitive below the gate.
#[derive(Debug)]
pub struct AuthorizedFactWrite {
    permit: MemoryPermit,
    draft: FactWriteCommand,
    fact_sidecar_table: Option<String>,
    fact_natural_key_columns: Vec<String>,
    links: AuthorizedNodeLinks,
}

/// The index rows a node write is admitted to assert, resolved and
/// read-checked by the engine before storage sees them.
///
/// Both lists are endpoints only. Their kinds are not carried because
/// they are not chosen: `origins` are [`crate::EdgeKind::Origin`] rows
/// and `references` are [`crate::EdgeKind::Reference`] rows, by virtue
/// of where they came from.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AuthorizedNodeLinks {
    origins: Vec<EdgeEndpoint>,
    references: Vec<EdgeEndpoint>,
}

impl AuthorizedNodeLinks {
    pub(crate) fn new(origins: Vec<EdgeEndpoint>, references: Vec<EdgeEndpoint>) -> Self {
        Self {
            origins,
            references,
        }
    }

    /// Targets the write declared it was made from.
    #[must_use]
    pub fn origins(&self) -> &[EdgeEndpoint] {
        &self.origins
    }

    /// Targets the write's typed payload points at.
    #[must_use]
    pub fn references(&self) -> &[EdgeEndpoint] {
        &self.references
    }
}

impl AuthorizedFactWrite {
    pub(crate) fn new(
        permit: MemoryPermit,
        draft: FactWriteCommand,
        fact_sidecar_table: Option<String>,
        fact_natural_key_columns: Vec<String>,
        links: AuthorizedNodeLinks,
    ) -> Self {
        Self {
            permit,
            draft,
            fact_sidecar_table,
            fact_natural_key_columns,
            links,
        }
    }

    /// Index rows storage must assert alongside the Fact row, in the
    /// same transaction.
    #[must_use]
    pub fn links(&self) -> &AuthorizedNodeLinks {
        &self.links
    }

    #[must_use]
    pub fn permit(&self) -> &MemoryPermit {
        &self.permit
    }

    /// # Panics
    ///
    /// Panics only if this authorized wrapper was not constructed through the
    /// engine fact-ingest authorization path.
    #[must_use]
    pub fn owner_write_permit(&self) -> &OwnerWritePermit {
        self.permit
            .owner_write_permit()
            .expect("AuthorizedFactWrite is constructed from a write permit")
    }

    #[must_use]
    pub fn draft(&self) -> &FactWriteCommand {
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

    #[cfg(test)]
    pub(crate) fn expire_delegated_write_for_test(&mut self) {
        self.permit.expire_delegated_write_for_test();
    }
}

#[derive(Debug)]
pub struct AuthorizedInlineCitedObject {
    schema_id: SchemaId,
    schema_version: SchemaVersion,
    content_hash: [u8; 32],
    sidecar_payload: SidecarPayload,
}

impl AuthorizedInlineCitedObject {
    pub(crate) fn new(
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        content_hash: [u8; 32],
        sidecar_payload: SidecarPayload,
    ) -> Self {
        Self {
            schema_id,
            schema_version,
            content_hash,
            sidecar_payload,
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
    pub const fn sidecar_payload(&self) -> &SidecarPayload {
        &self.sidecar_payload
    }
}

#[derive(Debug)]
pub struct AuthorizedInlineCitationMapping {
    schema_id: SchemaId,
    schema_version: SchemaVersion,
    sidecar_payload: Option<SidecarPayload>,
}

impl AuthorizedInlineCitationMapping {
    pub(crate) fn new(
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        sidecar_payload: Option<SidecarPayload>,
    ) -> Self {
        Self {
            schema_id,
            schema_version,
            sidecar_payload,
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
    pub const fn sidecar_payload(&self) -> Option<&SidecarPayload> {
        self.sidecar_payload.as_ref()
    }
}

/// Proof that an existing Fact memory and inline citation payloads
/// passed authorization, kind-specific schema validation, and citation
/// mapping target validation.
#[derive(Debug)]
pub struct AuthorizedCitationAttachment {
    permit: MemoryPermit,
    memory_id: MemoryId,
    memory_kind: crate::EntityKind,
    owner: Owner,
    cited_object: AuthorizedInlineCitedObject,
    mapping: AuthorizedInlineCitationMapping,
}

impl AuthorizedCitationAttachment {
    pub(crate) fn new(
        permit: MemoryPermit,
        memory_id: MemoryId,
        memory_kind: crate::EntityKind,
        owner: Owner,
        cited_object: AuthorizedInlineCitedObject,
        mapping: AuthorizedInlineCitationMapping,
    ) -> Self {
        Self {
            permit,
            memory_id,
            memory_kind,
            owner,
            cited_object,
            mapping,
        }
    }

    /// Kind the caller declared for the target memory, already checked
    /// against [`crate::citations::kind_may_cite_directly`]. Storage must
    /// reject the write when the stored row disagrees.
    #[must_use]
    pub const fn memory_kind(&self) -> crate::EntityKind {
        self.memory_kind
    }

    #[must_use]
    pub fn permit(&self) -> &MemoryPermit {
        &self.permit
    }

    /// # Panics
    ///
    /// Panics only if this authorized wrapper was not constructed through the
    /// engine citation-attachment authorization path.
    #[must_use]
    pub fn owner_write_permit(&self) -> &OwnerWritePermit {
        self.permit
            .owner_write_permit()
            .expect("AuthorizedCitationAttachment is constructed from a write permit")
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
    permit: MemoryPermit,
    draft: FactWriteCommand,
    cited_object: AuthorizedInlineCitedObject,
    mapping: AuthorizedInlineCitationMapping,
    fact_sidecar_table: Option<String>,
    fact_natural_key_columns: Vec<String>,
    links: AuthorizedNodeLinks,
}

impl AuthorizedFactWithCitation {
    pub(crate) fn new(
        permit: MemoryPermit,
        draft: FactWriteCommand,
        cited_object: AuthorizedInlineCitedObject,
        mapping: AuthorizedInlineCitationMapping,
        fact_sidecar_table: Option<String>,
        fact_natural_key_columns: Vec<String>,
        links: AuthorizedNodeLinks,
    ) -> Self {
        Self {
            permit,
            draft,
            cited_object,
            mapping,
            fact_sidecar_table,
            fact_natural_key_columns,
            links,
        }
    }

    /// Index rows storage must assert alongside the Fact row.
    #[must_use]
    pub fn links(&self) -> &AuthorizedNodeLinks {
        &self.links
    }

    #[must_use]
    pub fn permit(&self) -> &MemoryPermit {
        &self.permit
    }

    /// # Panics
    ///
    /// Panics only if this authorized wrapper was not constructed through the
    /// engine fact-with-citation authorization path.
    #[must_use]
    pub fn owner_write_permit(&self) -> &OwnerWritePermit {
        self.permit
            .owner_write_permit()
            .expect("AuthorizedFactWithCitation is constructed from a write permit")
    }

    #[must_use]
    pub fn draft(&self) -> &FactWriteCommand {
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
    pub fn fact_sidecar_table(&self) -> Option<&str> {
        self.fact_sidecar_table.as_deref()
    }

    #[must_use]
    pub fn fact_natural_key_columns(&self) -> &[String] {
        &self.fact_natural_key_columns
    }
}

/// Proof that a Fact ingest citing an EXISTING cited object by id passed
/// authorization, kind-specific schema validation, and mapping-payload
/// validation.
///
/// The by-ref twin of [`AuthorizedFactWithCitation`]: it carries no
/// inline cited-object payload — only the referenced id plus the object
/// schema the mapping targets, which storage checks against the stored
/// row (existence, owner, schema) before writing the mapping. Receipt
/// and idempotency semantics are identical to the inline path: the
/// citation is not part of the receipt key on either path, and a receipt
/// replay short-circuits before any citation row is written.
#[derive(Debug)]
pub struct AuthorizedFactWithCitationRef {
    permit: MemoryPermit,
    draft: FactWriteCommand,
    cited_object_id: Uuid,
    expected_object_schema: SchemaId,
    mapping: AuthorizedInlineCitationMapping,
    fact_sidecar_table: Option<String>,
    fact_natural_key_columns: Vec<String>,
    links: AuthorizedNodeLinks,
}

impl AuthorizedFactWithCitationRef {
    #[allow(clippy::too_many_arguments)] // one parameter per authorized fact
    pub(crate) fn new(
        permit: MemoryPermit,
        draft: FactWriteCommand,
        cited_object_id: Uuid,
        expected_object_schema: SchemaId,
        mapping: AuthorizedInlineCitationMapping,
        fact_sidecar_table: Option<String>,
        fact_natural_key_columns: Vec<String>,
        links: AuthorizedNodeLinks,
    ) -> Self {
        Self {
            permit,
            draft,
            cited_object_id,
            expected_object_schema,
            mapping,
            fact_sidecar_table,
            fact_natural_key_columns,
            links,
        }
    }

    /// Index rows storage must assert alongside the Fact row.
    #[must_use]
    pub fn links(&self) -> &AuthorizedNodeLinks {
        &self.links
    }

    #[must_use]
    pub fn permit(&self) -> &MemoryPermit {
        &self.permit
    }

    /// # Panics
    ///
    /// Panics only if this authorized wrapper was not constructed through the
    /// engine by-ref fact-with-citation authorization path.
    #[must_use]
    pub fn owner_write_permit(&self) -> &OwnerWritePermit {
        self.permit
            .owner_write_permit()
            .expect("AuthorizedFactWithCitationRef is constructed from a write permit")
    }

    #[must_use]
    pub fn draft(&self) -> &FactWriteCommand {
        &self.draft
    }

    #[must_use]
    pub const fn cited_object_id(&self) -> Uuid {
        self.cited_object_id
    }

    /// The cited-object schema the mapping targets; storage rejects the
    /// write when the referenced object's stored `schema_id` differs.
    #[must_use]
    pub fn expected_object_schema(&self) -> &SchemaId {
        &self.expected_object_schema
    }

    #[must_use]
    pub const fn mapping(&self) -> &AuthorizedInlineCitationMapping {
        &self.mapping
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

impl FactWriteCommand {
    /// Build a receipt-backed Fact write command from a typed payload using
    /// the payload's schema id/version and schema-owned receipt key. The
    /// engine stamps the owner from authorization before computing the
    /// receipt id; no caller-supplied owner is carried in the command.
    pub fn from_payload<P: FactPayload>(
        source_id: impl Into<String>,
        source_batch_id: SourceBatchId,
        payload: &P,
        observed_at: time::OffsetDateTime,
    ) -> Self {
        Self {
            schema_id: P::schema_id(),
            schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
            payload: payload.receipt_key(),
            rendered_text: Some(payload.render()),
            lexical_language: None,
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new(source_id.into()),
                source_batch_id,
                observed_at,
                occurred_at: observed_at,
            }),
            citation: None,
            derived_from: Vec::new(),
        }
    }

    /// Declare what this Fact was made from. The index rows that follow
    /// are `origin` rows because *that is what a derivation declaration
    /// means* — the caller names targets, never a kind.
    #[must_use]
    pub fn with_derived_from(mut self, derived_from: Vec<EdgeEndpoint>) -> Self {
        self.derived_from = derived_from;
        self
    }

    /// Attach an opaque citation hint to the draft.
    #[must_use]
    pub fn with_citation(mut self, citation: impl Into<Citation>) -> Self {
        self.citation = Some(citation.into());
        self
    }

    /// Stamp an explicit lexical language (a resolved text-search
    /// configuration name); `None` keeps the database default.
    #[must_use]
    pub fn with_lexical_language(mut self, lexical_language: Option<String>) -> Self {
        self.lexical_language = lexical_language;
        self
    }

    /// Override the occurrence time while preserving the observation time.
    #[must_use]
    pub fn occurred_at(mut self, occurred_at: time::OffsetDateTime) -> Self {
        if let Some(receipt) = &mut self.receipt {
            receipt.occurred_at = occurred_at;
        }
        self
    }

    /// Canonical receipt id per docs/01: BLAKE3 of `source_id` ||
    /// `owner_components` || schema-owned receipt key, separated by 0x00
    /// bytes. Re-receipt of the same observation produces the same hash by
    /// construction. Receiptless commands return `None` and are not replayed.
    #[must_use]
    pub fn receipt_id_for_owner(&self, owner: Owner) -> Option<FactReceiptId> {
        let receipt = self.receipt.as_ref()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(receipt.source_id.as_str().as_bytes());
        hasher.update(b"\x00");
        let kind = OwnerRefKind::of(&owner);
        let id = owner.stable_key_uuid();
        hasher.update(kind.as_str().as_bytes());
        hasher.update(b"\x00");
        hasher.update(id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(&self.payload);
        Some(FactReceiptId::new(*hasher.finalize().as_bytes()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FactIngestOutcome {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<FactReceiptId>,
    pub memory_id: MemoryId,
    pub change_event_seq: Uuid,
    /// True iff the same receipt id was already ingested.
    /// Receiptless Facts are never receipt-replayed.
    pub idempotent_replay: bool,
    /// The cited object this Fact reaches, when it carries a citation.
    ///
    /// Server-generated inside the write transaction, so a caller that
    /// supplied the artefact rather than its id has no other way to learn
    /// it. Populated on replay too — reading it back from the existing
    /// Fact's mapping — because the whole point of a content-addressed
    /// upload is that the second caller gets the first caller's object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_object_id: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::{FactReceiptDraft, FactWriteCommand};
    use crate::{
        OwnerRef, PayloadKeyBuilder, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
    };
    use uuid::Uuid;

    fn owner() -> OwnerRef {
        OwnerRef::Personal(UserId::new(
            Uuid::parse_str("018f0f4e-6b45-7c00-9bb5-b89b28d9c0a1").expect("uuid literal"),
        ))
    }

    fn draft(payload: Vec<u8>) -> FactWriteCommand {
        let now = time::OffsetDateTime::UNIX_EPOCH;
        FactWriteCommand {
            schema_id: SchemaId::new("test/fact".to_string()),
            schema_version: SchemaVersion::new(1),
            payload,
            rendered_text: None,
            lexical_language: None,
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new("test/source"),
                source_batch_id: SourceBatchId::new(Uuid::nil()),
                observed_at: now,
                occurred_at: now,
            }),
            citation: None,
            derived_from: Vec::new(),
        }
    }

    #[test]
    fn identical_schema_owned_keys_produce_same_receipt_id() {
        let mut key = PayloadKeyBuilder::new("test/fact", 1);
        key.field_str("stable_id", "same");
        let left = draft(key.finish());

        let mut key = PayloadKeyBuilder::new("test/fact", 1);
        key.field_str("stable_id", "same");
        let right = draft(key.finish());

        assert_eq!(left.payload, right.payload);
        assert_eq!(
            left.receipt_id_for_owner(owner()),
            right.receipt_id_for_owner(owner())
        );
    }

    /// Pins the org-free `receipt_id` BLAKE3 against drift. The hash folds
    /// source ‖ principal kind/id ‖ payload — no org. A
    /// fixed input must reproduce exactly this hex forever.
    #[test]
    fn receipt_id_golden_is_org_free() {
        let principal = OwnerRef::Personal(UserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid literal"),
        ));
        let draft = FactWriteCommand {
            schema_id: SchemaId::new("golden/fact".to_string()),
            schema_version: SchemaVersion::new(1),
            payload: b"golden-payload".to_vec(),
            rendered_text: None,
            lexical_language: None,
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new("golden/source"),
                source_batch_id: SourceBatchId::new(Uuid::nil()),
                observed_at: time::OffsetDateTime::UNIX_EPOCH,
                occurred_at: time::OffsetDateTime::UNIX_EPOCH,
            }),
            citation: None,
            derived_from: Vec::new(),
        };
        assert_eq!(
            hex::encode(
                draft
                    .receipt_id_for_owner(principal)
                    .expect("receipt")
                    .into_inner()
            ),
            "2469dc45f6d65917f6b3b13606ee8165330351f773bfec45c144ecabc5992da3"
        );
    }
}
