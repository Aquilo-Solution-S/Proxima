//! `FactIngest` verb — typed surface only.
//!
//! See docs/14-protocol-surface.md §`FactIngest` and
//! docs/01-event-source.md. The storage-side body lives in
//! `proxima-storage-pg`.

use uuid::Uuid;

#[cfg(any(test, feature = "test-fixtures"))]
use crate::EntityKind;
use crate::edge::EdgeEndpoint;
use crate::engine::MemoryPermit;
use crate::storage_ports::OwnerWritePermit;
use crate::{
    FactPayload, FactReceiptId, MemoryId, Owner, OwnerRefKind, SchemaId, SchemaVersion,
    SidecarPayload, SourceId,
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
    pub observed_at: time::OffsetDateTime,
    pub occurred_at: time::OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FactWriteCommand {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    /// Series id. `None` ⇒ storage mints `uuidv7()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<Uuid>,
    /// Source identity. Set iff `ingest_key` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    /// Source-declared delivery id. Same `(owner, source, ingest_key)` replays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest_key: Option<String>,
    /// Schema-owned receipt replay key material. The typed payload itself
    /// lives in the registered sidecar.
    pub payload: Vec<u8>,
    #[serde(default, skip)]
    pub rendered_text: Option<String>,
    /// Text-search configuration for this admission's lexical index,
    /// resolved by [`crate::lexical_language::resolve_lexical_language`].
    /// [`crate::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT`]
    /// asks for the deployment's configuration; `None` is not that
    /// request but the ABSENCE of one, and a schema whose contract
    /// declares `LanguagePolicy::PerRow` refuses it at the write port
    /// rather than choosing on the writer's behalf.
    ///
    /// It lands on the PROJECTION row — `<flavor>.projection`'s
    /// `lexical_language` column — which is where the vector it governs is
    /// derived. The projection is the first write path that has both the
    /// value and a column in scope, which is why the `language` parameter
    /// three MCP tools advertise lands here and nowhere earlier.
    ///
    /// `skip` like `rendered_text`: the language describes how the text is
    /// indexed, it is not receipt key material, and replaying an import
    /// with detection newly enabled must stay a replay.
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
    /// Observation-neutral pins (visit, write-act, parent).
    #[serde(default, skip)]
    pub refs: Vec<Uuid>,
    /// F/A citation. Perspectives must leave this `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_id: Option<Uuid>,
    /// `fact` | `abstraction` | `perspective`. Default fact.
    #[serde(default)]
    pub kind: String,
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
    /// Exact stable-deduplicated references emitted by the typed sidecars
    /// present at authorization. Raw compatibility refs never enter this
    /// vector, so a later persistence call cannot substitute another typed
    /// declaration while retaining the authorized pins.
    payload_references: Vec<EdgeEndpoint>,
}

impl AuthorizedNodeLinks {
    pub(crate) fn new(
        origins: Vec<EdgeEndpoint>,
        references: Vec<EdgeEndpoint>,
        payload_references: Vec<EdgeEndpoint>,
    ) -> Self {
        Self {
            origins,
            references,
            payload_references,
        }
    }

    /// Construct links for a backend fixture. Production code can only get
    /// this carrier from Engine authorization.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn new_for_tests(origins: Vec<EdgeEndpoint>, references: Vec<EdgeEndpoint>) -> Self {
        Self::new(origins, references, Vec::new())
    }

    /// Targets the write declared it was made from.
    #[must_use]
    pub fn origins(&self) -> &[EdgeEndpoint] {
        &self.origins
    }

    /// Targets the authorized write points at. These may come from typed
    /// payload declarations or the raw compatibility input.
    #[must_use]
    pub fn references(&self) -> &[EdgeEndpoint] {
        &self.references
    }

    /// Whether authorization observed a typed payload reference declaration.
    #[must_use]
    pub fn has_payload_references(&self) -> bool {
        !self.payload_references.is_empty()
    }

    /// Check that persistence received the same typed reference declaration
    /// authorization admitted. Values outside reference fields are not part
    /// of this witness.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed declarations or a changed ordered,
    /// stable-deduplicated endpoint vector.
    pub fn validate_sidecar_references(&self, sidecars: &[SidecarPayload]) -> Result<(), String> {
        let mut actual = Vec::new();
        for reference in sidecars.iter().flat_map(SidecarPayload::references) {
            reference.validate()?;
            reference.target.validate_shape()?;
            if !actual.contains(&reference.target) {
                actual.push(reference.target);
            }
        }
        if actual != self.payload_references {
            return Err("typed Fact sidecar references changed after authorization".to_owned());
        }
        Ok(())
    }
}

impl AuthorizedFactWrite {
    /// Test-only constructor. Engine fact-ingest authorization remains the
    /// production mint; this exists so a storage-backend test can exercise
    /// the WRITE PORT — the only public write path — instead of reaching
    /// past it into a backend verb.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn new_for_tests(
        owner_write: OwnerWritePermit,
        draft: FactWriteCommand,
        fact_sidecar_table: Option<String>,
        fact_natural_key_columns: Vec<String>,
    ) -> Self {
        let origins = draft.derived_from.clone();
        let references = draft
            .refs
            .iter()
            .copied()
            .map(|id| EdgeEndpoint::memory(EntityKind::Fact, MemoryId::new(id)))
            .collect();
        Self::new_with_links_for_tests(
            owner_write,
            draft,
            fact_sidecar_table,
            fact_natural_key_columns,
            AuthorizedNodeLinks::new_for_tests(origins, references),
        )
    }

    /// Test-only mint that intentionally accepts links independent of the
    /// draft. Storage regression tests use it to prove that the backend
    /// persists the authorized carrier rather than compatibility fields.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn new_with_links_for_tests(
        owner_write: OwnerWritePermit,
        draft: FactWriteCommand,
        fact_sidecar_table: Option<String>,
        fact_natural_key_columns: Vec<String>,
        links: AuthorizedNodeLinks,
    ) -> Self {
        Self::new(
            crate::engine::MemoryPermit::owner_scoped_with_write_for_tests(
                owner_write,
                crate::access::Relation::Editor,
            ),
            draft,
            fact_sidecar_table,
            fact_natural_key_columns,
            links,
        )
    }

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
    ///
    /// The `ingest_key` is the hex BLAKE3 digest of the receipt key, not
    /// the receipt key's own bytes. The key lands in the
    /// `ingest_keys (owner, source, ingest_key)` primary-key btree, and a
    /// receipt key embeds every declared field value verbatim — so a long
    /// Fact body (the remember surface admits 20,000 chars; btree index
    /// rows cap at a few KB) made the write fail with an index-row-size
    /// error, far from the payload that caused it. The digest is
    /// fixed-width (64 hex chars) and a pure function of the receipt key,
    /// so replay semantics are unchanged: the same declared values digest
    /// to the same key and replay, different values digest apart and
    /// mint. The raw bytes stay in `payload`, where
    /// [`Self::receipt_id_for_owner`] folds them.
    pub fn from_payload<P: FactPayload>(
        source_id: impl Into<String>,
        payload: &P,
        observed_at: time::OffsetDateTime,
    ) -> Self {
        let source_id = source_id.into();
        let receipt_key = payload.receipt_key();
        Self {
            schema_id: P::schema_id(),
            schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
            handle: None,
            source_id: Some(source_id.clone()),
            ingest_key: Some(hex::encode(blake3::hash(&receipt_key).as_bytes())),
            payload: receipt_key,
            rendered_text: Some(payload.render()),
            lexical_language: None,
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new(source_id),
                observed_at,
                occurred_at: observed_at,
            }),
            citation: None,
            derived_from: Vec::new(),
            refs: Vec::new(),
            blob_id: None,
            kind: "fact".into(),
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

    /// Observation-neutral pins (visit, write-act). Not origins.
    #[must_use]
    pub fn with_refs(mut self, refs: Vec<Uuid>) -> Self {
        self.refs = refs;
        self
    }

    /// Attach an opaque citation hint to the draft.
    #[must_use]
    pub fn with_citation(mut self, citation: impl Into<Citation>) -> Self {
        self.citation = Some(citation.into());
        self
    }

    /// Reuse an existing series handle, or leave `None` for storage to mint.
    #[must_use]
    pub const fn with_handle(mut self, handle: Option<Uuid>) -> Self {
        self.handle = handle;
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
    /// Version `t`. Alias of the row id.
    pub memory_id: MemoryId,
    /// Series handle.
    pub handle: Uuid,
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
    use crate::{OwnerRef, PayloadKeyBuilder, SchemaId, SchemaVersion, SourceId, UserId};
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
            handle: None,
            source_id: None,
            ingest_key: None,
            payload,
            rendered_text: None,
            lexical_language: None,
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new("test/source"),
                observed_at: now,
                occurred_at: now,
            }),
            citation: None,
            derived_from: Vec::new(),
            refs: Vec::new(),
            blob_id: None,
            kind: "fact".into(),
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
            handle: None,
            source_id: None,
            ingest_key: None,
            payload: b"golden-payload".to_vec(),
            rendered_text: None,
            lexical_language: None,
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new("golden/source"),
                observed_at: time::OffsetDateTime::UNIX_EPOCH,
                occurred_at: time::OffsetDateTime::UNIX_EPOCH,
            }),
            citation: None,
            derived_from: Vec::new(),
            refs: Vec::new(),
            blob_id: None,
            kind: "fact".into(),
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

    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    struct NotePayload {
        title: String,
        body: String,
    }

    impl crate::FactPayload for NotePayload {
        const SCHEMA_ID: &'static str = "test/note";
        const SCHEMA_VERSION: u32 = 1;

        fn receipt_key(&self) -> Vec<u8> {
            let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
            key.field_str("title", &self.title);
            key.field_str("body", &self.body);
            key.finish()
        }

        fn render(&self) -> String {
            format!("{}\n\n{}", self.title, self.body)
        }
    }

    /// The ingest key is the fixed-width digest of the receipt key, never
    /// the key's own bytes: a receipt key embeds every declared field value
    /// verbatim, and the `ingest_keys` primary-key btree caps index rows at
    /// ~2.7KB, so a Fact body within the surface's 20,000-char bound must
    /// not reach that index verbatim. Identity survives the digest — equal
    /// receipt keys hash together, a difference at the far end of a long
    /// body hashes apart.
    #[test]
    fn from_payload_ingest_key_is_a_fixed_width_digest_of_the_receipt_key() {
        let long_body = "x".repeat(20_000);
        let note = |body: String| NotePayload {
            title: "same title".into(),
            body,
        };
        let command = |payload: &NotePayload| {
            FactWriteCommand::from_payload("test/source", payload, time::OffsetDateTime::UNIX_EPOCH)
        };

        let first = command(&note(long_body.clone()));
        let key = first
            .ingest_key
            .as_deref()
            .expect("sourced command carries an ingest key");
        assert_eq!(key.len(), 64, "hex-encoded BLAKE3-256 is 64 chars");
        assert_eq!(key, hex::encode(blake3::hash(&first.payload).as_bytes()));

        let replay = command(&note(long_body.clone()));
        assert_eq!(replay.ingest_key, first.ingest_key);

        let deep_change = command(&note(format!("{}y", &long_body[..19_999])));
        assert_ne!(deep_change.ingest_key, first.ingest_key);
    }
}
