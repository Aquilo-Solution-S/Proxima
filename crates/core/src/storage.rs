//! Storage DTOs and backend-neutral errors.
//!
//! Port traits live in [`crate::storage_ports`]. See docs/concepts.md and
//! AGENTS.md invariants 2, 3, 5.

use std::any::{Any, TypeId};
use std::sync::Arc;

use uuid::Uuid;

use crate::edge::EdgeEndpoint;
use crate::scope::ScopeRef;
use crate::{
    AbstractionPayload, CitedObjectPayload, FactPayload, GoalPayload, PayloadReference,
    PerspectivePayload,
};
use crate::{EntityKind, MemoryId, MemoryOperatorKind, OwnerId, SchemaId, SchemaVersion};

/// Deliberately NOT `#[non_exhaustive]`, like [`crate::ErrorCode`]: the
/// three maps onto a caller's vocabulary (the engine's protocol error, the
/// MCP error kind and REST's status table) match it with no wildcard arm, so
/// a new variant is a compile error until someone chooses its rendering.
#[derive(Debug, Clone, thiserror::Error)]
pub enum StorageError {
    #[error("storage backend unavailable: {0}")]
    Unavailable(String),
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("conflict: {0}")]
    Conflict(String),
    /// A newly minted memory or Goal UUID hit an existing admission primary key.
    /// Storage may mint again once and retry the complete write.
    #[error("memory UUID collision")]
    MemoryIdCollision,
    /// Transient failure (deadlock / serialization) that is safe to retry
    /// after re-running the whole transaction. Classified from SQLSTATE
    /// `40P01`/`40001`; the `*_atomic` pool wrappers retry a bounded number
    /// of times before surfacing this.
    #[error("retryable storage error: {0}")]
    Retryable(String),
    /// Membership writes cannot run from a fixed-snapshot transaction: the
    /// tree lock must be acquired before the snapshot that validates it.
    #[error("membership write requires READ COMMITTED isolation")]
    MembershipIsolation,
    #[error("membership authorization refused: {0}")]
    MembershipForbidden(String),
    /// A same `(owner, request_id)` idempotency key already exists with a
    /// different body. Carries the offending request id so the engine can
    /// surface a typed `IdempotencyConflict` without message parsing, and
    /// the existing handle when known so a client can `supersedes` it.
    ///
    /// NOTE: the `Display` form is deliberately `idempotency_conflict:{id}`
    /// so storage-level callers that match on the message keep working.
    #[error("idempotency_conflict:{request_id}")]
    IdempotencyConflict {
        request_id: String,
        existing: Option<uuid::Uuid>,
    },
    #[error(
        "database schema does not match this release lane; export/reset is required: {details}"
    )]
    SchemaResetRequired { details: String },
    #[error("not found")]
    NotFound,
    /// A write named a flavor-owned lifecycle scope whose registry row is
    /// not there for this owner, asked WHILE HOLDING that scope's fence.
    /// Under the fence the answer is settled for the life of the
    /// transaction — an erase that committed first is visible and one that
    /// has not is waiting — so this is a refusal, never a race.
    #[error("scope not registered: {kind}:{id}")]
    ScopeMissing { kind: String, id: Uuid },
    #[error("suppressed: {0}")]
    Suppressed(String),
    /// A listenable Fact's publication capture was refused (docs/host-integration.md). The
    /// whole write is rolled back; the typed cause travels rather than a
    /// message, so the engine maps it without parsing a string.
    #[error("publication refused: {0}")]
    PublicationRefused(#[from] crate::publication::PublicationError),
    /// `mark_achieved` or abandon on a goal with decompose children that are
    /// still Active or Paused (#514): an open goal never sits under a closed
    /// one. `open` names each open child by its current head (the handle a
    /// caller passes to transition or `mark_achieved`), in child handle order.
    #[error("goal has open children: {}", join_goal_ids(open))]
    GoalChildrenOpen { open: Vec<crate::GoalId> },
    /// `mark_achieved` on a goal whose decompose children are all Abandoned
    /// (#514): split it again, or abandon it.
    #[error("goal children are all abandoned; achieve at least one child first")]
    GoalChildrenAllAbandoned,
    /// A write replayed an admission its request key already names, but
    /// declared something other than what that admission stored. The same
    /// key with a different declaration is an idempotency conflict, and the
    /// engine answers it as one; the cause travels typed, so no caller reads
    /// the message to learn which declaration drifted.
    #[error("{0}")]
    ReplayChanged(ReplayDrift),
    #[error("internal storage error: {0}")]
    Internal(String),
}

/// What a replay declared differently from the admission it replays. The
/// `Display` form is the sentence clients have always read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReplayDrift {
    /// A fact replay named other references.
    #[error("fact replay changed declared refs")]
    FactRefs,
    /// A derivation replay named other origins or references.
    #[error("derived replay changed declared refs")]
    DerivedRefs,
    /// A derivation replay's stored pin rows differ from its declaration.
    #[error("derived replay changed declared index rows")]
    DerivedIndexRows,
    /// A derivation replay named other edges to another store's memories,
    /// kind included.
    #[error("derived replay changed declared external edges")]
    DerivedExternalEdges,
}

fn join_goal_ids(ids: &[crate::GoalId]) -> String {
    ids.iter()
        .map(|id| id.into_inner().hyphenated().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryKindRow {
    pub memory_id: MemoryId,
    pub kind: EntityKind,
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
    protocol_json: Reader<Result<serde_json::Value, String>>,
    /// Schema-declared reference fields. Ingest needs them without knowing
    /// the concrete payload type, so the constructor captures the typed
    /// value in a reader and the declaration survives the erasure without
    /// a downcast.
    references: Reader<Vec<PayloadReference>>,
    /// The flavor-owned lifecycle scope this payload declares. Same
    /// mechanism as `references`, and for the same reason: the admission
    /// fences the scope without knowing which concrete payload type it is
    /// writing. `None` for the payload kinds that are not memories (cited
    /// objects) and for every schema that declares no `SCOPE_KIND`.
    scope: Reader<Option<ScopeRef>>,
}

/// A read of one typed payload, closed over the value it reads.
type Reader<R> = Arc<dyn Fn() -> R + Send + Sync>;

fn reader<T, R>(value: &Arc<T>, read: fn(&T) -> R) -> Reader<R>
where
    T: Send + Sync + 'static,
    R: 'static,
{
    let value = Arc::clone(value);
    Arc::new(move || read(&value))
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
        references: fn(&T) -> Vec<PayloadReference>,
        scope: fn(&T) -> Option<ScopeRef>,
    ) -> Self
    where
        T: serde::Serialize + Send + Sync + 'static,
    {
        let value = Arc::new(value);
        Self {
            kind,
            schema_id,
            schema_version,
            type_id: TypeId::of::<T>(),
            protocol_json: reader(&value, |typed| {
                serde_json::to_value(typed).map_err(|err| err.to_string())
            }),
            references: reader(&value, references),
            scope: reader(&value, scope),
            value,
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
            T::references,
            T::scope,
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
            T::references,
            T::scope,
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
            T::references,
            T::scope,
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
            T::references,
            |_| None,
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
            |_| Vec::new(),
            |_| None,
        )
    }

    #[must_use]
    pub fn downcast_ref<T>(&self) -> Option<&T>
    where
        T: Send + Sync + 'static,
    {
        self.value.downcast_ref::<T>()
    }

    /// Node references this payload declares (docs/concepts.md).
    /// Empty for payload kinds that cannot reference nodes.
    #[must_use]
    pub fn references(&self) -> Vec<PayloadReference> {
        (self.references)()
    }

    /// The flavor-owned lifecycle scope this payload declares
    /// (docs/build-a-flavor.md), or `None` when it declares none.
    ///
    /// The admission path reads THIS: it is the only thing between a typed
    /// payload and the fence its scope's erase holds, and it answers for
    /// every caller — protocol ingress, a flavor's own write, a host
    /// writing the payload straight through `Engine` — because the
    /// declaration travels with the value rather than with the caller.
    #[must_use]
    pub fn scope(&self) -> Option<ScopeRef> {
        (self.scope)()
    }

    /// Render this typed payload as JSON for protocol output.
    ///
    /// # Errors
    ///
    /// Returns an error when the typed serializer fails.
    pub fn to_protocol_json(&self) -> Result<serde_json::Value, String> {
        (self.protocol_json)()
    }
}

#[derive(Debug)]
pub struct AuthorDerivedRequest<'a> {
    /// `PostgreSQL` uses this UUID as the series handle when `supersedes` is
    /// absent; with `supersedes`, it retains the prior version's handle.
    /// It allocates the actual admission `t` separately. Use the returned
    /// [`AuthorDerivedOutcome::memory_id`] to address the memory.
    pub memory_id: MemoryId,
    pub owner: OwnerId,
    pub kind: EntityKind,
    pub text: String,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub operator_kind: MemoryOperatorKind,
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
    /// Every space selected by the schema and owner write route (current
    /// and next). Jobs are queued in the same transaction as the memory;
    /// an empty slice requests no embedding work.
    pub queued_spaces: &'a [crate::EmbeddingSpace],
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
    /// Memories of other stores this memory was also made from. Storage
    /// records them beside the row, in the same transaction, without any
    /// constraint on the remote side, as `origin` rows; a replay records
    /// nothing and must declare the same rows.
    pub external_origins: &'a [uuid::Uuid],
    /// Memories of other stores this memory refers to (an interpretation's
    /// remote subjects), recorded like `external_origins` but as
    /// `reference` rows (#480). A memory id is never in both lists.
    pub external_references: &'a [uuid::Uuid],
    /// Tool `idempotency_key` ledger entry when the caller supplied one.
    pub request: Option<crate::verbs::idempotency::RequestKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorDerivedOutcome {
    pub memory_id: MemoryId,
    pub idempotent_replay: bool,
    /// The replay matched a memory that has been forgotten: the write was
    /// not repeated and the memory stays forgotten. Always false for a new
    /// admission.
    pub forgotten: bool,
    /// Pins this write declared (`origins` + `refs`). A count, not a
    /// list: pins live in the row's own columns, so replaying the write
    /// re-asserts the same values and there is no pin id to hand back.
    pub edge_count: usize,
    /// This newly admitted row queued embedding work in its memory
    /// transaction. Nonembeddable schemas, routes with no clients, and
    /// idempotent replays report false; vectors become available after a drain.
    pub embedding_deferred: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingJobClaim {
    pub job_id: Uuid,
    pub owner: OwnerId,
    pub entity_kind: EntityKind,
    pub entity_id: MemoryId,
    pub space: crate::EmbeddingSpace,
    pub claim_token: Uuid,
}
