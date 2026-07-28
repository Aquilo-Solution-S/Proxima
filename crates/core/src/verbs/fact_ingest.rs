//! `FactIngest` verb — typed surface only.
//!
//! See docs/14-protocol-surface.md §"Fact write" and
//! docs/01-event-source.md §"Fact membrane". The storage-side body
//! lives in `proxima-storage-pg`.

use uuid::Uuid;

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
}

impl AuthorizedFactWrite {
    pub(crate) fn new(
        permit: MemoryPermit,
        draft: FactWriteCommand,
        fact_sidecar_table: Option<String>,
        fact_natural_key_columns: Vec<String>,
    ) -> Self {
        Self {
            permit,
            draft,
            fact_sidecar_table,
            fact_natural_key_columns,
        }
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
    owner: Owner,
    cited_object: AuthorizedInlineCitedObject,
    mapping: AuthorizedInlineCitationMapping,
}

impl AuthorizedCitationAttachment {
    pub(crate) fn new(
        permit: MemoryPermit,
        memory_id: MemoryId,
        owner: Owner,
        cited_object: AuthorizedInlineCitedObject,
        mapping: AuthorizedInlineCitationMapping,
    ) -> Self {
        Self {
            permit,
            memory_id,
            owner,
            cited_object,
            mapping,
        }
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
}

impl AuthorizedFactWithCitation {
    pub(crate) fn new(
        permit: MemoryPermit,
        draft: FactWriteCommand,
        cited_object: AuthorizedInlineCitedObject,
        mapping: AuthorizedInlineCitationMapping,
        fact_sidecar_table: Option<String>,
        fact_natural_key_columns: Vec<String>,
    ) -> Self {
        Self {
            permit,
            draft,
            cited_object,
            mapping,
            fact_sidecar_table,
            fact_natural_key_columns,
        }
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
        }
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
