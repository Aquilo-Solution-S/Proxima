//! Storage DTOs and backend-neutral errors.
//!
//! Port traits live in [`crate::storage_ports`]. See docs/07-storage.md and
//! AGENTS.md invariants 2, 3, 5.

use std::any::{Any, TypeId};
use std::sync::Arc;

use uuid::Uuid;

use crate::edge::EdgeEndpoint;
use crate::{
    AbstractionPayload, CitationMappingPayload, CitedObjectPayload, FactPayload, GoalPayload,
    PayloadReference, PerspectivePayload,
};
use crate::{
    EntityKind, MemoryId, MemoryOperatorKind, Owner, SchemaId, SchemaVersion, SourceBatchId,
};

#[derive(Debug, Clone, thiserror::Error)]
pub enum StorageError {
    #[error("storage backend unavailable: {0}")]
    Unavailable(String),
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("conflict: {0}")]
    Conflict(String),
    /// Transient failure (deadlock / serialization) that is safe to retry
    /// after re-running the whole transaction. Classified from SQLSTATE
    /// `40P01`/`40001`; the `*_atomic` pool wrappers retry a bounded number
    /// of times before surfacing this.
    #[error("retryable storage error: {0}")]
    Retryable(String),
    /// A same `(owner, request_id)` idempotency key already exists with a
    /// different body. Carries the offending request id so the engine can
    /// surface a typed `IdempotencyConflict` without message parsing.
    ///
    /// NOTE: the `Display` form is deliberately `idempotency_conflict:{id}`
    /// so storage-level callers that match on the message keep working.
    #[error("idempotency_conflict:{request_id}")]
    IdempotencyConflict { request_id: String },
    #[error(
        "database schema does not match this release lane; export/reset is required: {details}"
    )]
    SchemaResetRequired { details: String },
    #[error("not found")]
    NotFound,
    #[error("suppressed: {0}")]
    Suppressed(String),
    #[error("internal storage error: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryKindRow {
    pub memory_id: MemoryId,
    pub kind: EntityKind,
}

/// Maximum number of Memory ids accepted by one hydration command.
///
/// Hydration is a bounded repair operation: callers must not be able to turn
/// an owner-scoped request into an unbounded object-store walk.
pub const MAX_MEMORY_HYDRATION_BATCH: usize = 64;

/// Result classification for an owner-authorized hydration request.
///
/// `NotFound` deliberately covers both an absent id and an id outside the
/// permit's owner. The distinction is not observable through this API.
/// `Hydrated` may preserve erased-target witnesses; the count is carried by
/// [`MemoryHydrationOutcome`] without exposing witness metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryHydrationStatus {
    Hydrated,
    AlreadyHot,
    NotFound,
    MissingColdObject,
    /// The cold record predates the database integrity witness or uses a
    /// format this binary does not understand. Operators must migrate or
    /// discard the record; it is not evidence of corrupt bytes.
    UnsupportedColdObject,
    /// The cold record names a sidecar declaration this registry cannot
    /// safely restore (for example, an unsupported sidecar version/stamp).
    UnsupportedColdSidecar,
    InvalidColdObject,
    /// The batch could not commit because another requested id was invalid.
    /// No item with this status was changed.
    NotAttempted,
}

/// One id's typed hydration result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryHydrationOutcome {
    pub memory_id: MemoryId,
    pub status: MemoryHydrationStatus,
    /// Number of correctly kinded erased-target witnesses retained while
    /// hydrating. Zero is the ordinary case and says nothing about whether a
    /// target was ever present.
    pub preserved_witnesses: u32,
}

impl MemoryHydrationOutcome {
    #[must_use]
    pub const fn hydrated(memory_id: MemoryId, preserved_witnesses: u32) -> Self {
        Self {
            memory_id,
            status: MemoryHydrationStatus::Hydrated,
            preserved_witnesses,
        }
    }

    #[must_use]
    pub const fn simple(memory_id: MemoryId, status: MemoryHydrationStatus) -> Self {
        Self {
            memory_id,
            status,
            preserved_witnesses: 0,
        }
    }
}

/// Results for one bounded hydration command.
///
/// The transaction is atomic over every owner-visible cooled item in the
/// request. If a cold object is missing, unsupported, or invalid, the command
/// commits no hydration and valid cooled items are reported as `NotAttempted`;
/// hot and absent/invisible classifications remain useful and unchanged. An
/// absent or foreign id is returned as `NotFound`, matching owner-scoped reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryHydrationBatchOutcome {
    pub outcomes: Vec<MemoryHydrationOutcome>,
    pub committed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactSourceBatchRow {
    pub memory_id: MemoryId,
    pub source_batch_id: SourceBatchId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryGraphPayloadRow {
    pub memory_id: MemoryId,
    pub tags: Option<Vec<String>>,
    pub body: Option<String>,
}

/// Identity already admitted by a prior owner-scoped read (`t`, kind, schema).
/// Sidecar hydrate takes this instead of re-joining `memory ⋈ memory_head`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryGraphIdentity {
    pub memory_id: MemoryId,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
}

#[derive(Clone)]
pub struct SidecarPayload {
    pub kind: crate::verbs::schema::PayloadKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    type_id: TypeId,
    value: Arc<dyn Any + Send + Sync>,
    protocol_json: fn(&dyn Any) -> Result<serde_json::Value, String>,
    /// Schema-declared reference fields, read back through the erased
    /// value. Ingest needs them without knowing the concrete payload
    /// type, and a per-kind function pointer is how the typed
    /// declaration survives the erasure.
    references: fn(&dyn Any) -> Vec<PayloadReference>,
}

impl PartialEq for SidecarPayload {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.schema_id == other.schema_id
            && self.schema_version == other.schema_version
            && self.type_id == other.type_id
    }
}

impl Eq for SidecarPayload {}

impl std::fmt::Debug for SidecarPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidecarPayload")
            .field("kind", &self.kind)
            .field("schema_id", &self.schema_id)
            .field("schema_version", &self.schema_version)
            .field("type_id", &self.type_id)
            .finish_non_exhaustive()
    }
}

impl SidecarPayload {
    fn new<T>(
        kind: crate::verbs::schema::PayloadKind,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        value: T,
        references: fn(&dyn Any) -> Vec<PayloadReference>,
    ) -> Self
    where
        T: serde::Serialize + Send + Sync + 'static,
    {
        Self {
            kind,
            schema_id,
            schema_version,
            type_id: TypeId::of::<T>(),
            value: Arc::new(value),
            protocol_json: encode_protocol_json::<T>,
            references,
        }
    }

    #[must_use]
    pub fn fact<T>(value: T) -> Self
    where
        T: FactPayload + Send + Sync,
    {
        Self::new(
            crate::verbs::schema::PayloadKind::Fact,
            T::schema_id(),
            SchemaVersion::new(T::SCHEMA_VERSION),
            value,
            fact_references::<T>,
        )
    }

    #[must_use]
    pub fn abstraction<T>(value: T) -> Self
    where
        T: AbstractionPayload + Send + Sync,
    {
        Self::new(
            crate::verbs::schema::PayloadKind::Abstraction,
            T::schema_id(),
            SchemaVersion::new(T::SCHEMA_VERSION),
            value,
            abstraction_references::<T>,
        )
    }

    #[must_use]
    pub fn perspective<T>(value: T) -> Self
    where
        T: PerspectivePayload + Send + Sync,
    {
        Self::new(
            crate::verbs::schema::PayloadKind::Perspective,
            T::schema_id(),
            SchemaVersion::new(T::SCHEMA_VERSION),
            value,
            perspective_references::<T>,
        )
    }

    #[must_use]
    pub fn goal<T>(value: T) -> Self
    where
        T: GoalPayload + Send + Sync,
    {
        Self::new(
            crate::verbs::schema::PayloadKind::Goal,
            T::schema_id(),
            SchemaVersion::new(T::SCHEMA_VERSION),
            value,
            goal_references::<T>,
        )
    }

    #[must_use]
    pub fn cited_object<T>(value: T) -> Self
    where
        T: CitedObjectPayload,
    {
        Self::new(
            crate::verbs::schema::PayloadKind::CitedObject,
            T::schema_id(),
            SchemaVersion::new(T::SCHEMA_VERSION),
            value,
            no_references,
        )
    }

    #[must_use]
    pub fn citation_mapping<T>(value: T) -> Self
    where
        T: CitationMappingPayload,
    {
        Self::new(
            crate::verbs::schema::PayloadKind::CitationMapping,
            T::schema_id(),
            SchemaVersion::new(T::SCHEMA_VERSION),
            value,
            no_references,
        )
    }

    #[must_use]
    pub fn downcast_ref<T>(&self) -> Option<&T>
    where
        T: Send + Sync + 'static,
    {
        self.value.downcast_ref::<T>()
    }

    /// Node references this payload declares (docs/16 §The Model).
    /// Empty for payload kinds that cannot reference nodes.
    #[must_use]
    pub fn references(&self) -> Vec<PayloadReference> {
        (self.references)(self.value.as_ref())
    }

    /// Render this typed payload as JSON for protocol output.
    ///
    /// # Errors
    ///
    /// Returns an error when the erased value does not match its encoder or
    /// the typed serializer fails.
    pub fn to_protocol_json(&self) -> Result<serde_json::Value, String> {
        (self.protocol_json)(self.value.as_ref())
    }

    /// Tags declared on the typed payload, if it has a `tags` array.
    #[must_use]
    pub fn graph_tags(&self) -> Vec<String> {
        self.to_protocol_json()
            .ok()
            .and_then(|value| {
                value.get("tags")?.as_array().map(|tags| {
                    tags.iter()
                        .filter_map(|tag| tag.as_str().map(ToOwned::to_owned))
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    /// Body/text declared on the typed payload (`body`, else `text`).
    #[must_use]
    pub fn graph_body(&self) -> Option<String> {
        self.to_protocol_json().ok().and_then(|value| {
            value
                .get("body")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.get("text").and_then(serde_json::Value::as_str))
                .map(ToOwned::to_owned)
        })
    }
}

fn encode_protocol_json<T>(value: &dyn Any) -> Result<serde_json::Value, String>
where
    T: serde::Serialize + Send + Sync + 'static,
{
    let typed = value
        .downcast_ref::<T>()
        .ok_or_else(|| "sidecar payload type mismatch".to_string())?;
    serde_json::to_value(typed).map_err(|err| err.to_string())
}

fn fact_references<T: FactPayload>(value: &dyn Any) -> Vec<PayloadReference> {
    value
        .downcast_ref::<T>()
        .map(T::references)
        .unwrap_or_default()
}

fn abstraction_references<T: AbstractionPayload>(value: &dyn Any) -> Vec<PayloadReference> {
    value
        .downcast_ref::<T>()
        .map(T::references)
        .unwrap_or_default()
}

fn perspective_references<T: PerspectivePayload>(value: &dyn Any) -> Vec<PayloadReference> {
    value
        .downcast_ref::<T>()
        .map(T::references)
        .unwrap_or_default()
}

fn goal_references<T: GoalPayload>(value: &dyn Any) -> Vec<PayloadReference> {
    value
        .downcast_ref::<T>()
        .map(T::references)
        .unwrap_or_default()
}

/// Cited objects and citation mappings are not nodes in the F/A/P graph,
/// so they have no reference fields to declare.
fn no_references(_value: &dyn Any) -> Vec<PayloadReference> {
    Vec::new()
}

/// What storage must do about a derived memory's vector, decided by the
/// engine before the write opens its transaction.
///
/// The three cases are mutually exclusive and each one names its own
/// consequence, so "no vector was written" can never again be confused with
/// "no vector was wanted". A pair of `Option`s could spell the same states,
/// but it could also spell the two that mean nothing (a vector with no model
/// id, a model id with neither a vector nor a deferral) — and it was the
/// silent version of the third case that made an unembeddable derived text a
/// permanently failing write.
#[derive(Debug, Clone, PartialEq)]
pub enum DerivedEmbedding<'a> {
    /// No embedding client is configured. Storage writes no vector and
    /// enqueues nothing; a later `reconcile_embeddings` is what covers
    /// these rows.
    None,
    /// The client embedded the text. Storage writes the vector inline, in
    /// the same transaction as the row.
    Ready { model_id: &'a str, vector: Vec<f32> },
    /// The client could not embed this text but the provider is up, so the
    /// input — not the provider — is what failed. Storage writes no vector
    /// and enqueues a durable embedding job for `model_id` **in the same
    /// transaction as the row**, so the drain (which owns the bisecting
    /// over-limit rescue) picks the memory up. Losing the whole write, and
    /// every model call upstream of it, is not the right answer to an input
    /// one provider call refused.
    Deferred { model_id: &'a str },
}

impl DerivedEmbedding<'_> {
    /// Whether storage is expected to enqueue an embedding job instead of
    /// writing a vector.
    #[must_use]
    pub const fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred { .. })
    }
}

#[derive(Debug)]
pub struct AuthorDerivedRequest<'a> {
    pub memory_id: MemoryId,
    pub owner: Owner,
    pub kind: EntityKind,
    pub text: String,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub operator_kind: MemoryOperatorKind,
    pub model_id: &'a str,
    pub sidecar_payload: SidecarPayload,
    /// Prior `t` of the series this write revises. Supersession is a
    /// later `t` on the same `handle`, not a column: storage resolves the
    /// prior row's `handle` and appends this write to that series, so
    /// there is no lineage pointer and no edge. `None` mints a new
    /// series.
    pub supersedes: Option<MemoryId>,
    /// Resolved text-search configuration name to stamp on the row;
    /// `None` applies the database default. Storage verifies the name
    /// against the catalog inside the write transaction.
    pub lexical_language: Option<&'a str>,
    pub embedding: DerivedEmbedding<'a>,
    /// What this memory was made from. Storage stores the entries in the
    /// row's own `origins` pin column, in the same transaction as the
    /// row — the [`crate::EdgeKind::Origin`] reading is a consequence of
    /// which column they land in, never a parameter.
    pub origins: &'a [EdgeEndpoint],
    /// Nodes this memory's payload points at, read off its
    /// schema-declared reference fields by the engine. Storage stores
    /// them in the row's own `refs` pin column, in the same transaction
    /// as the row, which is what makes them
    /// [`crate::EdgeKind::Reference`] pins.
    pub references: &'a [EdgeEndpoint],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorDerivedOutcome {
    pub memory_id: MemoryId,
    pub idempotent_replay: bool,
    /// Pins this write declared (`origins` + `refs`). A count, not a
    /// list: pins live in the row's own columns, so replaying the write
    /// re-asserts the same values and there is no pin id to hand back.
    pub edge_count: usize,
    /// The memory landed without a vector and carries a pending embedding
    /// job instead ([`DerivedEmbedding::Deferred`]). Until a drain runs, the
    /// memory is lexically findable and semantically invisible — a caller
    /// that needs it searchable now must say so, which it cannot do if the
    /// only record is a log line.
    pub embedding_deferred: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingJobClaim {
    pub job_id: Uuid,
    pub owner: Owner,
    pub entity_kind: EntityKind,
    pub entity_id: MemoryId,
    pub model_id: String,
    pub claim_token: Uuid,
}
