//! `FactIngest` verb — typed surface only.
//!
//! See docs/protocol.md §`FactIngest` and
//! docs/concepts.md. The storage-side body lives in
//! `proxima-storage-pg`.

use crate::verbs::query::SidecarAtom;
use uuid::Uuid;

use crate::edge::EdgeEndpoint;
use crate::engine::WritePermit;
use crate::error::ProtocolError;
use crate::storage_ports::OwnerWritePermit;
use crate::{
    FactPayload, FactReceiptId, MemoryId, OwnerId, SchemaId, SchemaVersion, SidecarPayload,
    SourceId,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CitedObjectHint {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub content_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Citation {
    pub object: CitedObjectHint,
}

/// Compact citation input for the common opaque-object path used by Fact
/// membrane ingest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationSpec {
    pub cited_object_schema_id: SchemaId,
    pub cited_object_schema_version: SchemaVersion,
    pub content_hash: [u8; 32],
}

impl CitationSpec {
    /// Build a v1 opaque cited-object spec.
    #[must_use]
    pub fn v1(cited_object_schema_id: impl Into<String>, content_hash: [u8; 32]) -> Self {
        Self {
            cited_object_schema_id: SchemaId::new(cited_object_schema_id.into()),
            cited_object_schema_version: SchemaVersion::new(1),
            content_hash,
        }
    }

    /// Build a v1 citation spec whose cited-object content hash is the
    /// BLAKE3 hash of the Fact payload's schema-owned receipt key.
    #[must_use]
    pub fn v1_for_payload<P: FactPayload>(
        cited_object_schema_id: impl Into<String>,
        payload: &P,
    ) -> Self {
        let payload = payload.receipt_key();
        Self::v1(cited_object_schema_id, *blake3::hash(&payload).as_bytes())
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InlineCitedObjectDraft {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub payload_bytes: Vec<u8>,
}

/// The receipt a sourced Fact write asks for: the source whose delivery it
/// is. [`FactWriteCommand::receipt_id_for_owner`] folds it into the receipt
/// id. When the observation occurred is the command's `occurred_at`, not a
/// receipt field.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FactReceiptDraft {
    pub source_id: SourceId,
}

/// How far past the write's clock a Fact's `FactWriteCommand.occurred_at`
/// may lie: an observation cannot postdate its own recording, and two
/// hosts' clocks disagreeing by seconds is not a writer error.
pub const OCCURRED_AT_FUTURE_BOUND: time::Duration = time::Duration::minutes(5);

/// Admit a writer's occurrence time against the clock reading `now`.
///
/// Returns the value the Fact stores: in UTC and truncated to whole
/// microseconds, the precision of a Postgres `timestamptz`, so the time
/// stored, published in the outbox envelope and read back is one value.
///
/// # Errors
///
/// [`ProtocolError::invalid_argument`] on `occurred_at` when it lies more
/// than [`OCCURRED_AT_FUTURE_BOUND`] after `now`.
pub(crate) fn check_occurred_at(
    occurred_at: time::OffsetDateTime,
    now: time::OffsetDateTime,
) -> Result<time::OffsetDateTime, ProtocolError> {
    if occurred_at > now + OCCURRED_AT_FUTURE_BOUND {
        return Err(ProtocolError::invalid_argument(
            "occurred_at",
            format!(
                "must not lie more than {} minutes in the future",
                OCCURRED_AT_FUTURE_BOUND.whole_minutes()
            ),
        ));
    }
    occurred_at
        .checked_to_offset(time::UtcOffset::UTC)
        .and_then(|utc| utc.replace_microsecond(utc.microsecond()).ok())
        .ok_or_else(|| {
            ProtocolError::invalid_argument("occurred_at", "is not representable in UTC")
        })
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
    /// When the observation occurred. `None` ⇒ the write time, read out of
    /// the `t` storage mints. Every Fact stores the resolved value and every
    /// read returns it; an Abstraction or Perspective carries none.
    /// Supersession, recency and versioning still order by `t`. The engine
    /// refuses a value more than [`OCCURRED_AT_FUTURE_BOUND`] in the future.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<time::OffsetDateTime>,
    pub citation: Option<Citation>,
    /// Additional reference pins, separate from payload-declared references.
    /// Excluded from receipt key material; replay still requires matching pins.
    #[serde(default, skip)]
    pub additional_references: Vec<EdgeEndpoint>,
    /// Observation-neutral pins (visit, write-act, parent).
    #[serde(default, skip)]
    pub refs: Vec<Uuid>,
    /// F/A citation. Perspectives must leave this `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_id: Option<Uuid>,
    /// What the row is. A draft that names none is a Fact.
    #[serde(default)]
    pub kind: crate::MemoryKind,
    /// Tool `idempotency_key` ledger entry. When set, storage judges this
    /// request before receipt replay and claims the ledger rows on admit.
    /// Not receipt material and never serialized.
    #[serde(skip)]
    pub request: Option<crate::verbs::idempotency::RequestKey>,
}

/// The values every authorized Fact witness carries: the mint gate, the
/// owner-stamped command, the Fact's sidecar contract and payload set, and
/// the index rows storage must assert alongside the Fact row.
///
/// `pub(crate)`, and no `pub` accessor hands the core out. A witness that could
/// yield its core would let a citing write be routed through the plain-Fact
/// persistence path, which drops the citation; a witness reveals its core's
/// values one getter at a time instead.
#[derive(Debug)]
pub(crate) struct AuthorizedFactCore {
    permit: WritePermit,
    draft: FactWriteCommand,
    fact_sidecar_table: Option<String>,
    fact_natural_key_columns: Vec<String>,
    fact_natural_key_values: Option<Vec<(String, SidecarAtom)>>,
    /// [`crate::engine::FactWrite::reobserve_if_displaced`]. Only the typed Fact
    /// write sets it; both citing routes leave it `false`.
    reobserve_displaced: bool,
    sidecar_payloads: Vec<SidecarPayload>,
    links: AuthorizedNodeLinks,
    /// The publication record this admission captures, resolved by the
    /// engine at authorization time from the frozen `SchemaInfo.listenable`
    /// and the typed payload. `None` for every non-listenable schema, which
    /// is every schema unless one declared otherwise.
    publication: Option<crate::publication::PublicationPlan>,
}

impl AuthorizedFactCore {
    pub(crate) fn new(
        permit: WritePermit,
        draft: FactWriteCommand,
        fact_sidecar_table: Option<String>,
        fact_natural_key_columns: Vec<String>,
        sidecar_payloads: Vec<SidecarPayload>,
        links: AuthorizedNodeLinks,
    ) -> Self {
        Self {
            permit,
            draft,
            fact_sidecar_table,
            fact_natural_key_columns,
            fact_natural_key_values: None,
            reobserve_displaced: false,
            sidecar_payloads,
            links,
            publication: None,
        }
    }

    /// Bind the resolved publication plan (draft + the engine's configured
    /// limits). Called only by the engine's authorization path, which is
    /// the one place that has both the frozen schema declaration, the
    /// trusted model identity AND the deployment's [`PublicationConfig`].
    ///
    /// [`PublicationConfig`]: crate::publication::PublicationConfig
    pub(crate) fn with_publication(
        mut self,
        publication: Option<crate::publication::PublicationPlan>,
    ) -> Self {
        self.publication = publication;
        self
    }

    pub(crate) const fn publication(&self) -> Option<&crate::publication::PublicationPlan> {
        self.publication.as_ref()
    }

    pub(crate) fn with_natural_key_values(
        mut self,
        values: Option<Vec<(String, SidecarAtom)>>,
    ) -> Self {
        self.fact_natural_key_values = values;
        self
    }

    pub(crate) fn sidecar_payloads(&self) -> &[SidecarPayload] {
        &self.sidecar_payloads
    }

    pub(crate) const fn links(&self) -> &AuthorizedNodeLinks {
        &self.links
    }

    pub(crate) const fn permit(&self) -> &WritePermit {
        &self.permit
    }

    pub(crate) const fn draft(&self) -> &FactWriteCommand {
        &self.draft
    }

    pub(crate) fn fact_sidecar_table(&self) -> Option<&str> {
        self.fact_sidecar_table.as_deref()
    }

    pub(crate) fn fact_natural_key_columns(&self) -> &[String] {
        &self.fact_natural_key_columns
    }

    #[cfg(test)]
    pub(crate) fn expire_delegated_write_for_test(&mut self) {
        self.permit.expire_delegated_write_for_test();
    }
}

/// Proof that a Fact write passed authorization + schema validation
/// and had its owner stamped from the authz context.
/// The only constructor is `Engine::authorize_fact_ingest`, so a
/// caller cannot reach the sidecar-ingest primitive below the gate.
#[derive(Debug)]
pub struct AuthorizedFactWrite {
    core: AuthorizedFactCore,
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

    /// Construct links for a backend fixture. Production code can only get
    /// this carrier from Engine authorization.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn new_for_tests(origins: Vec<EdgeEndpoint>, references: Vec<EdgeEndpoint>) -> Self {
        Self::new(origins, references)
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
        // Legacy backend fixtures also use this mint for derived rows. Their
        // declared pins must keep their original kind; Engine Fact admission
        // constructs AuthorizedNodeLinks independently and never emits origins.
        let origins = draft.additional_references.clone();
        let references = draft
            .refs
            .iter()
            .copied()
            .map(|id| EdgeEndpoint::memory(crate::EntityKind::Fact, MemoryId::new(id)))
            .collect();
        Self::new_with_links_for_tests(
            owner_write,
            draft,
            fact_sidecar_table,
            fact_natural_key_columns,
            AuthorizedNodeLinks::new_for_tests(origins, references),
        )
    }

    /// Test-only: bind the typed sidecars that a production authorization
    /// call would carry into the opaque prepared write.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn with_sidecar_payloads_for_tests(mut self, sidecars: Vec<SidecarPayload>) -> Self {
        self.core.sidecar_payloads = sidecars;
        self
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
        Self::new(AuthorizedFactCore::new(
            WritePermit::for_tests(owner_write),
            draft,
            fact_sidecar_table,
            fact_natural_key_columns,
            Vec::new(),
            links,
        ))
    }

    /// Test-only: attach the publication plan core would have resolved.
    ///
    /// Storage backend tests exercise the WRITE PORT, and the port reads
    /// the plan off the witness. Without this they could only reach the
    /// capture by calling a backend verb directly, which is the layering
    /// the port exists to prevent. It takes the whole plan — draft AND
    /// limits — because that is what production hands storage; a fixture
    /// that could supply limits separately would be testing a shape the
    /// engine cannot produce.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn with_publication_for_tests(
        mut self,
        publication: crate::publication::PublicationPlan,
    ) -> Self {
        self.core = self.core.with_publication(Some(publication));
        self
    }

    pub(crate) const fn new(core: AuthorizedFactCore) -> Self {
        Self { core }
    }

    /// Carry [`crate::engine::FactWrite::reobserve_if_displaced`] to storage.
    pub(crate) const fn reobserving_displaced(mut self, reobserve: bool) -> Self {
        self.core.reobserve_displaced = reobserve;
        self
    }

    /// Index rows storage must assert alongside the Fact row, in the
    /// same transaction.
    #[must_use]
    pub const fn links(&self) -> &AuthorizedNodeLinks {
        self.core.links()
    }

    #[must_use]
    pub const fn permit(&self) -> &WritePermit {
        self.core.permit()
    }

    #[must_use]
    pub fn owner_write_permit(&self) -> &OwnerWritePermit {
        self.core.permit().owner_write_permit()
    }

    #[must_use]
    pub const fn draft(&self) -> &FactWriteCommand {
        self.core.draft()
    }

    /// The publication record this admission must capture, or `None` when
    /// the Fact's schema is not listenable.
    ///
    /// Read by the storage backend inside the write transaction. It is
    /// DATA, not a callback, so the bounded retry around
    /// begin→body→commit re-runs cleanly.
    #[must_use]
    pub const fn publication(&self) -> Option<&crate::publication::PublicationPlan> {
        self.core.publication()
    }

    #[must_use]
    pub fn fact_sidecar_table(&self) -> Option<&str> {
        self.core.fact_sidecar_table()
    }

    #[must_use]
    pub fn fact_natural_key_columns(&self) -> &[String] {
        self.core.fact_natural_key_columns()
    }

    /// Exact natural-key values captured from the typed payload during authorization.
    /// Storage compares them before automatic series selection.
    #[must_use]
    pub fn fact_natural_key_values(&self) -> Option<&[(String, SidecarAtom)]> {
        self.core.fact_natural_key_values.as_deref()
    }

    /// Whether a replay whose Fact no longer heads its natural-key series is
    /// admitted again rather than replayed
    /// ([`crate::engine::FactWrite::reobserve_if_displaced`]).
    #[must_use]
    pub const fn reobserves_displaced(&self) -> bool {
        self.core.reobserve_displaced
    }

    #[must_use]
    pub fn sidecar_payloads(&self) -> &[SidecarPayload] {
        self.core.sidecar_payloads()
    }

    #[cfg(test)]
    pub(crate) fn expire_delegated_write_for_test(&mut self) {
        self.core.expire_delegated_write_for_test();
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

/// Proof that a Fact ingest and its inline cited object passed
/// authorization and kind-specific schema validation.
#[derive(Debug)]
pub struct AuthorizedFactWithCitation {
    core: AuthorizedFactCore,
    cited_object: AuthorizedInlineCitedObject,
    upload_completion: Option<Uuid>,
}

impl AuthorizedFactWithCitation {
    pub(crate) const fn new(
        core: AuthorizedFactCore,
        cited_object: AuthorizedInlineCitedObject,
    ) -> Self {
        Self {
            core,
            cited_object,
            upload_completion: None,
        }
    }

    /// Index rows storage must assert alongside the Fact row.
    #[must_use]
    pub const fn links(&self) -> &AuthorizedNodeLinks {
        self.core.links()
    }

    #[must_use]
    pub const fn permit(&self) -> &WritePermit {
        self.core.permit()
    }

    #[must_use]
    pub fn owner_write_permit(&self) -> &OwnerWritePermit {
        self.core.permit().owner_write_permit()
    }

    #[must_use]
    pub const fn draft(&self) -> &FactWriteCommand {
        self.core.draft()
    }

    /// The publication record this admission must capture, or `None` when
    /// the Fact's schema is not listenable.
    ///
    /// Read by the storage backend inside the write transaction. It is
    /// DATA, not a callback, so the bounded retry around
    /// begin→body→commit re-runs cleanly.
    #[must_use]
    pub const fn publication(&self) -> Option<&crate::publication::PublicationPlan> {
        self.core.publication()
    }

    pub(crate) fn with_upload_completion(mut self, upload_id: Uuid) -> Self {
        self.upload_completion = Some(upload_id);
        self
    }

    /// Upload row to complete atomically with this cited Fact, minted only by
    /// the Engine's upload verb.
    ///
    /// ```compile_fail
    /// fn forge(write: proxima_core::verbs::fact_ingest::AuthorizedFactWithCitation) {
    ///     write.with_upload_completion(uuid::Uuid::nil());
    /// }
    /// ```
    #[must_use]
    pub const fn upload_completion(&self) -> Option<Uuid> {
        self.upload_completion
    }

    #[must_use]
    pub const fn cited_object(&self) -> &AuthorizedInlineCitedObject {
        &self.cited_object
    }

    #[must_use]
    pub fn fact_sidecar_table(&self) -> Option<&str> {
        self.core.fact_sidecar_table()
    }

    #[must_use]
    pub fn fact_natural_key_columns(&self) -> &[String] {
        self.core.fact_natural_key_columns()
    }

    /// Exact natural-key values captured from the typed payload during authorization.
    /// Storage compares them before automatic series selection.
    #[must_use]
    pub fn fact_natural_key_values(&self) -> Option<&[(String, SidecarAtom)]> {
        self.core.fact_natural_key_values.as_deref()
    }

    /// Whether a replay whose Fact no longer heads its natural-key series is
    /// admitted again rather than replayed
    /// ([`crate::engine::FactWrite::reobserve_if_displaced`]).
    #[must_use]
    pub const fn reobserves_displaced(&self) -> bool {
        self.core.reobserve_displaced
    }

    #[must_use]
    pub fn sidecar_payloads(&self) -> &[SidecarPayload] {
        self.core.sidecar_payloads()
    }
}

/// Proof that a Fact ingest citing an EXISTING cited object by id passed
/// authorization and kind-specific schema validation.
///
/// The by-ref twin of [`AuthorizedFactWithCitation`]: it carries no
/// inline cited-object payload — only the referenced id, which storage
/// checks against the stored row (existence, owner) before the Fact
/// cites it. Receipt and idempotency semantics are identical to the
/// inline path: the citation is not part of the receipt key on either
/// path, and a receipt replay short-circuits before any citation is
/// written.
#[derive(Debug)]
pub struct AuthorizedFactWithCitationRef {
    core: AuthorizedFactCore,
    cited_object_id: Uuid,
}

impl AuthorizedFactWithCitationRef {
    pub(crate) const fn new(core: AuthorizedFactCore, cited_object_id: Uuid) -> Self {
        Self {
            core,
            cited_object_id,
        }
    }

    /// Index rows storage must assert alongside the Fact row.
    #[must_use]
    pub const fn links(&self) -> &AuthorizedNodeLinks {
        self.core.links()
    }

    #[must_use]
    pub const fn permit(&self) -> &WritePermit {
        self.core.permit()
    }

    #[must_use]
    pub fn owner_write_permit(&self) -> &OwnerWritePermit {
        self.core.permit().owner_write_permit()
    }

    #[must_use]
    pub const fn draft(&self) -> &FactWriteCommand {
        self.core.draft()
    }

    /// The publication record this admission must capture, or `None` when
    /// the Fact's schema is not listenable.
    ///
    /// Read by the storage backend inside the write transaction. It is
    /// DATA, not a callback, so the bounded retry around
    /// begin→body→commit re-runs cleanly.
    #[must_use]
    pub const fn publication(&self) -> Option<&crate::publication::PublicationPlan> {
        self.core.publication()
    }

    #[must_use]
    pub const fn cited_object_id(&self) -> Uuid {
        self.cited_object_id
    }

    #[must_use]
    pub fn fact_sidecar_table(&self) -> Option<&str> {
        self.core.fact_sidecar_table()
    }

    #[must_use]
    pub fn fact_natural_key_columns(&self) -> &[String] {
        self.core.fact_natural_key_columns()
    }

    /// Exact natural-key values captured from the typed payload during authorization.
    /// Storage compares them before automatic series selection.
    #[must_use]
    pub fn fact_natural_key_values(&self) -> Option<&[(String, SidecarAtom)]> {
        self.core.fact_natural_key_values.as_deref()
    }

    /// Whether a replay whose Fact no longer heads its natural-key series is
    /// admitted again rather than replayed
    /// ([`crate::engine::FactWrite::reobserve_if_displaced`]).
    #[must_use]
    pub const fn reobserves_displaced(&self) -> bool {
        self.core.reobserve_displaced
    }

    #[must_use]
    pub fn sidecar_payloads(&self) -> &[SidecarPayload] {
        self.core.sidecar_payloads()
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
    ///
    /// The command carries no occurrence time: the Fact occurred when it is
    /// written unless the `occurred_at` builder says otherwise.
    pub fn from_payload<P: FactPayload>(source_id: impl Into<String>, payload: &P) -> Self {
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
            }),
            occurred_at: None,
            citation: None,
            additional_references: Vec::new(),
            refs: Vec::new(),
            blob_id: None,
            kind: crate::MemoryKind::Fact,
            request: None,
        }
    }

    /// Add reference targets. Facts never acquire derivation origins.
    #[must_use]
    pub fn with_additional_references(mut self, additional_references: Vec<EdgeEndpoint>) -> Self {
        self.additional_references = additional_references;
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

    /// Attach a tool `idempotency_key` request so storage judges and claims
    /// the shared request ledger.
    #[must_use]
    pub fn request_key(mut self, request: crate::verbs::idempotency::RequestKey) -> Self {
        self.request = Some(request);
        self
    }

    /// When the observation occurred, for a Fact recorded after the fact
    /// (an import, a backfill); sets the `occurred_at` field.
    #[must_use]
    pub const fn occurred_at(mut self, occurred_at: time::OffsetDateTime) -> Self {
        self.occurred_at = Some(occurred_at);
        self
    }

    /// Canonical receipt id per docs/concepts.md: BLAKE3 of `source_id` ||
    /// the owner id's 16 bytes || schema-owned receipt key, separated by 0x00
    /// bytes. Re-receipt of the same observation produces the same hash by
    /// construction. Receiptless commands return `None` and are not replayed.
    #[must_use]
    pub fn receipt_id_for_owner(&self, owner: OwnerId) -> Option<FactReceiptId> {
        let receipt = self.receipt.as_ref()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(receipt.source_id.as_str().as_bytes());
        hasher.update(b"\x00");
        let id = owner.into_inner();
        hasher.update(id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(&self.payload);
        Some(FactReceiptId::new(*hasher.finalize().as_bytes()))
    }
}

/// A Fact receipt read by ingest identity.
///
/// This is the row `(owner, source_id, ingest_key)` already names. It is
/// not a [`FactIngestOutcome`]: nothing is admitted, and a key with no
/// visible admission is absence rather than a replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReceipt {
    /// Admission `t`.
    pub memory_id: MemoryId,
    /// Series handle.
    pub handle: Uuid,
    /// Schema of that admission.
    pub schema_id: SchemaId,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FactIngestOutcome {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<FactReceiptId>,
    /// Version `t`. Alias of the row id.
    pub memory_id: MemoryId,
    /// Series handle.
    pub handle: Uuid,
    /// True iff the same receipt id was already ingested.
    /// Receiptless Facts are never receipt-replayed.
    pub idempotent_replay: bool,
    /// The cited object this Fact reaches, when it carries a citation.
    ///
    /// Server-generated inside the write transaction, so a caller that
    /// supplied the artefact rather than its id has no other way to learn
    /// it. Populated on replay too — reading it back from the existing
    /// Fact's citation — because the whole point of a content-addressed
    /// upload is that the second caller gets the first caller's object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_object_id: Option<Uuid>,
    /// True when this is a replay of a memory that has since been forgotten
    /// (#488): the handle answers, the memory stays forgotten, and nothing
    /// was written. Always false for a fresh admission.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forgotten: bool,
}

#[cfg(test)]
mod tests {
    use super::{OCCURRED_AT_FUTURE_BOUND, check_occurred_at};
    use crate::error::ErrorCode;

    fn at(rfc3339: &str) -> time::OffsetDateTime {
        time::OffsetDateTime::parse(rfc3339, &time::format_description::well_known::Rfc3339)
            .expect("RFC 3339")
    }

    #[test]
    fn an_occurrence_beyond_the_future_bound_is_refused() {
        let now = at("2026-10-15T12:00:00Z");
        let err = check_occurred_at(
            now + OCCURRED_AT_FUTURE_BOUND + time::Duration::microseconds(1),
            now,
        )
        .expect_err("past the bound");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("occurred_at"), "{}", err.message);
    }

    #[test]
    fn an_occurrence_within_the_clock_skew_or_in_the_past_is_admitted() {
        let now = at("2026-10-15T12:00:00Z");
        let at_bound = now + OCCURRED_AT_FUTURE_BOUND;
        assert_eq!(
            check_occurred_at(at_bound, now).expect("at the bound"),
            at_bound
        );
        let backdate = at("2023-03-22T08:15:00Z");
        assert_eq!(check_occurred_at(backdate, now).expect("past"), backdate);
    }

    #[test]
    fn an_admitted_occurrence_is_utc_and_whole_microseconds() {
        let now = at("2026-10-15T12:00:00Z");
        let admitted =
            check_occurred_at(at("2023-03-22T10:15:00.123456789+02:00"), now).expect("past");
        assert_eq!(admitted, at("2023-03-22T08:15:00.123456Z"));
        assert_eq!(admitted.offset(), time::UtcOffset::UTC);
    }
}
