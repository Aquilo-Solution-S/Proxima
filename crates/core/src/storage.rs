//! Abstract storage trait — backend-neutral surface for the
//! engine.
//!
//! See docs/07-storage.md and AGENTS.md invariants 2, 3, 5.

use std::any::{Any, TypeId};
use std::sync::Arc;

use crate::SourceBatchId;
use crate::dependency::MemoryDependency;
use crate::personality::WakeEntryDraft;
use crate::personality::{
    AbstractionRow, ActiveGoalSummary, ChangeEventForWake, InstantiatePersonalityRequest,
    InstantiatePersonalityResponse, ListReadScopeRequest, ListReadScopeResponse, MemorySnapshot,
    PersonalityInstanceId, PersonalityInstanceRow, PersonalityRef, PersonalityWriteOutcome,
    PersonalityWriteRequest, SetReadScopeRequest, SetReadScopeResponse, SetWakeEntriesRequest,
    SetWakeEntriesResponse, SidecarSpec, TombstonePersonalityRequest, TombstonePersonalityResponse,
};
use crate::verbs::close_batch::CloseBatchOutcome;
use crate::verbs::event_history::{EventHistoryRequest, EventHistoryResponse};
use crate::verbs::event_ingest::{
    AuthorizedEventIngest, AuthorizedFactWithCitation, EventDraft, EventIngestOutcome,
};
use crate::verbs::fact_cleanup::{CleanupDueFactsOutcome, TombstoneFactOutcome};
use crate::verbs::goal_write::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalWriteOutcome, ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use crate::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
use crate::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use crate::{
    AbstractionPayload, CitationMappingPayload, CitedObjectPayload, EdgePayload, FactPayload,
    GoalPayload, PerspectivePayload,
};
use crate::{
    EdgeAuthorshipKind, EdgeId, EntityId, EntityKind, EntityOwnerRow, FactEntityId, GroupId,
    MembershipRow, MemoryId, MemoryOperatorKind, Owner, Principal, RegisteredRelation, Relation,
    RemoveOwnerOutcome, SchemaId, SchemaVersion, UserId,
};

#[derive(Debug, Clone, thiserror::Error)]
pub enum StorageError {
    #[error("storage backend unavailable: {0}")]
    Unavailable(String),
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not found")]
    NotFound,
    #[error("internal storage error: {0}")]
    Internal(String),
}

/// Boxed closure for read-modify-write on `WakeEntry` rows.
pub type WakeEntriesMutator =
    Box<dyn FnOnce(&[WakeEntryDraft]) -> Result<Vec<WakeEntryDraft>, String> + Send + 'static>;

/// Identity row for a per-master-token shell-author personality.
///
/// Returned by [`Storage::ensure_master_token_personality`].
/// Carries both the personality instance id and the
/// `current_root_perspective_memory_id` so callers can populate
/// `McpToolCtx.caller_self_perspective` without a second round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasterTokenPersonality {
    pub instance_id: crate::PersonalityInstanceId,
    pub self_perspective_memory_id: crate::MemoryId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryKindRow {
    pub memory_id: MemoryId,
    /// `None` means Fact; Abstraction/Perspective are stored explicitly.
    pub kind: Option<EntityKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryGraphPayloadRow {
    pub memory_id: MemoryId,
    pub tags: Option<Vec<String>>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeighborEdgeRow {
    pub edge_id: EdgeId,
    pub relation: String,
    pub source_kind: EntityKind,
    pub source_memory_id: Option<MemoryId>,
    pub target_kind: EntityKind,
    pub target_memory_id: Option<MemoryId>,
    pub target_readable: bool,
    pub source_world_readable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeEndpointKindRow {
    pub edge_id: EdgeId,
    pub source_kind: EntityKind,
    pub target_kind: EntityKind,
}

#[derive(Clone)]
pub struct SidecarPayload {
    pub kind: crate::verbs::schema::PayloadKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    type_id: TypeId,
    value: Arc<dyn Any + Send + Sync>,
    protocol_json: fn(&dyn Any) -> Result<serde_json::Value, String>,
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
        )
    }

    #[must_use]
    pub fn edge<T>(value: T) -> Self
    where
        T: EdgePayload + Send + Sync,
    {
        Self::new(
            crate::verbs::schema::PayloadKind::Edge,
            T::schema_id(),
            SchemaVersion::new(T::SCHEMA_VERSION),
            value,
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
        )
    }

    #[must_use]
    pub fn downcast_ref<T>(&self) -> Option<&T>
    where
        T: Send + Sync + 'static,
    {
        self.value.downcast_ref::<T>()
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

#[derive(Debug, Clone)]
pub struct DerivedEdgeSpec<'a> {
    pub owner: &'a Owner,
    pub relation: RegisteredRelation<'a>,
    pub source_kind: EntityKind,
    pub source_memory_id: MemoryId,
    pub target_kind: EntityKind,
    pub target_memory_id: MemoryId,
    pub authorship_kind: EdgeAuthorshipKind,
    pub authorship_owner_memory_id: Option<MemoryId>,
    pub sidecar_payload: Option<&'a SidecarPayload>,
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
    pub prompt_version: &'a str,
    pub author_personality_instance_id: Option<PersonalityInstanceId>,
    pub sidecar_payload: SidecarPayload,
    /// Prior A/P memory superseded by this derived memory. Storage must
    /// persist this on `memories.supersedes` in the same transaction as
    /// the row, sidecar, and edge writes.
    pub supersedes: Option<MemoryId>,
    pub embedding: Option<Vec<f32>>,
    pub embedding_model_id: Option<&'a str>,
    pub edges: &'a [DerivedEdgeSpec<'a>],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorDerivedOutcome {
    pub memory_id: MemoryId,
    pub idempotent_replay: bool,
    pub edge_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingJobClaim {
    pub owner: Owner,
    pub entity_kind: EntityKind,
    pub entity_id: MemoryId,
    pub model_id: String,
    pub embedding_version: i32,
    pub attempts: i32,
}

#[async_trait::async_trait]
pub trait Storage: Send + Sync {
    /// Atomic Fact materialization per docs/14 §`EventIngest`.
    /// Single transaction inserting `cited_object`, event,
    /// memory(Fact), `citation_mapping`, `change_event`. Replay
    /// (`event_id` collision) returns the original outcome with
    /// `idempotent_replay = true`.
    ///
    /// # Errors
    ///
    /// Constraint violations map to `ConstraintViolation`; sqlx
    /// failures map to Internal.
    async fn ingest_event_atomic(
        &self,
        draft: &EventDraft,
        embedding_model_id: Option<&str>,
    ) -> Result<EventIngestOutcome, StorageError>;

    /// Owner-scoped read of stored Fact render text. Returns `None` when
    /// the row is absent, non-Fact, belongs to another owner, or has no
    /// rendered text yet.
    async fn load_fact_text(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError>;

    /// Owner-scoped read of stored text for any embeddable memory kind.
    async fn load_embedding_text(
        &self,
        owner: &Owner,
        entity_kind: EntityKind,
        memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError>;

    /// Owner-scoped idempotent upsert of one Fact embedding row for the
    /// `(Fact, memory_id, 1, model_id)` natural key.
    async fn upsert_fact_embedding(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
    ) -> Result<(), StorageError>;

    /// Owner-scoped idempotent upsert of one embedding row for any
    /// embeddable memory kind.
    async fn upsert_memory_embedding(
        &self,
        owner: &Owner,
        entity_kind: EntityKind,
        memory_id: crate::MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
    ) -> Result<(), StorageError>;

    /// Owner-scoped list of Facts with stored text and no embedding row
    /// for `model_id`.
    async fn list_facts_missing_embedding(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<crate::MemoryId>, StorageError>;

    /// Atomically claim up to `limit` pending memory embedding jobs for
    /// `model_id`, oldest-first. Implementations must use row locks
    /// with `SKIP LOCKED` so concurrent host drainers do not claim the
    /// same job.
    async fn claim_pending_embedding_jobs(
        &self,
        model_id: &str,
        limit: i64,
    ) -> Result<Vec<EmbeddingJobClaim>, StorageError>;

    /// Mark a claimed embedding job complete. The embedding row is the
    /// durable success record, so completed jobs are deleted.
    async fn complete_embedding_job(&self, claim: &EmbeddingJobClaim) -> Result<(), StorageError>;

    /// Mark a claimed embedding job failed for this attempt, resetting
    /// it to `pending` until the retry cap is reached.
    async fn fail_embedding_job(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError>;

    /// Owner-scoped bounded enqueue of pending jobs for Facts with
    /// stored text and no current-model embedding row.
    async fn enqueue_missing_embedding_jobs(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: i64,
    ) -> Result<u64, StorageError>;

    /// Owner-scoped count of embedding jobs not yet embedded (status
    /// `pending` or `processing`). Surfaces drain backlog so a caller can
    /// distinguish "semantic recall still warming up" from "genuinely empty".
    async fn count_pending_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError>;

    /// Atomic MCP-call activity materialization. One transaction writes
    /// the call Fact, inline I/O `CitedObject`, `CitationMapping`, typed
    /// sidecars, and entity change event. Whole-verb replay returns the
    /// original ids with `idempotent_replay = true`.
    async fn persist_mcp_call_atomic(
        &self,
        input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError>;

    /// Atomic Fact materialization for already-authorized `EventIngest`
    /// plus one typed sidecar payload. The storage backend dispatches
    /// the payload through its build-time sidecar registry.
    async fn ingest_event_with_typed_sidecar(
        &self,
        authorized: &AuthorizedEventIngest,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<EventIngestOutcome, StorageError>;

    /// Atomic Fact + Citation materialization for an already-authorized
    /// inline-citation write plus one typed Fact sidecar payload.
    async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<EventIngestOutcome, StorageError>;

    /// Atomic derived Memory authoring: Memory row, typed sidecar,
    /// optional embedding row, `memories.supersedes`, and
    /// already-resolved edge specs. Callers that set `supersedes`
    /// must include the matching `core/supersedes` edge unless they
    /// enter through [`crate::Engine::author_derived`], which
    /// synthesizes it.
    async fn author_derived(
        &self,
        req: &AuthorDerivedRequest<'_>,
    ) -> Result<AuthorDerivedOutcome, StorageError>;

    /// Atomic single memory-edge authoring for already-resolved relation
    /// specs.
    async fn append_memory_edge(&self, edge: &DerivedEdgeSpec<'_>) -> Result<EdgeId, StorageError>;

    /// Owner-scoped lookup of memory endpoint kinds. Missing ids are
    /// omitted; callers that need strict existence must compare ids.
    async fn load_memory_kinds(
        &self,
        _owner: &Owner,
        _memory_ids: &[MemoryId],
    ) -> Result<Vec<MemoryKindRow>, StorageError> {
        Ok(Vec::new())
    }

    /// Owner-scoped payload fragments used by graph MCP projections.
    async fn load_memory_graph_payloads(
        &self,
        _owner: &Owner,
        _memory_ids: &[MemoryId],
        _include_body: bool,
    ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
        Ok(Vec::new())
    }

    /// Read-set-scoped memory-neighbor edges for graph MCP projections.
    async fn load_neighbor_memory_edges(
        &self,
        _read_owners: &[Principal],
        _memory_ids: &[MemoryId],
        _limit: usize,
    ) -> Result<Vec<NeighborEdgeRow>, StorageError> {
        Ok(Vec::new())
    }

    /// Owner-scoped memory-edge id lookup for one source, relation, and
    /// target set.
    async fn load_memory_edge_ids(
        &self,
        _owner: &Owner,
        _relation: &str,
        _source_memory_id: MemoryId,
        _target_memory_ids: &[MemoryId],
    ) -> Result<Vec<EdgeId>, StorageError> {
        Ok(Vec::new())
    }

    /// Lookup edge endpoint kinds for display of `change_event` rows.
    async fn load_edge_endpoint_kinds(
        &self,
        _edge_ids: &[EdgeId],
    ) -> Result<Vec<EdgeEndpointKindRow>, StorageError> {
        Ok(Vec::new())
    }

    /// Owner-scoped lookup of a non-tombstoned personality root.
    async fn active_personality_root(
        &self,
        _owner: &Owner,
        _instance_id: PersonalityInstanceId,
    ) -> Result<Option<MemoryId>, StorageError> {
        Ok(None)
    }

    /// Atomic direct Active Goal create plus goal payload sidecar,
    /// activation Fact, `core/inspires`, and `core/motivated-by`
    /// evidence edges.
    async fn create_goal_atomic(
        &self,
        req: &CreateGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError>;

    /// Atomic Goal head transition. Storage rejects stale priors by
    /// relying on the unique successor constraint, not a TOCTOU pre-read.
    async fn transition_goal_atomic(
        &self,
        req: &TransitionGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError>;

    /// Atomic Goal achievement. Requires nonempty evidence.
    async fn achieve_goal_atomic(
        &self,
        req: &AchieveGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError>;

    /// Atomic Active Goal content replacement.
    async fn modify_goal_atomic(
        &self,
        req: &ModifyGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError>;

    /// Atomic child Goal creation plus `goal_parents` rows.
    async fn decompose_goal_atomic(
        &self,
        req: &DecomposeGoalAtomicRequest<'_>,
    ) -> Result<DecomposeGoalOutcome, StorageError>;

    /// Owner-scoped bounded read of `change_event` rows, newest-first.
    /// Server clamps `limit` to `MAX_EVENT_HISTORY_LIMIT`. When
    /// `before` is `Some(seq)`, returns rows with `seq < before`.
    /// `seq_high_water` is the latest seq in the owner's `change_event`
    /// log at read time (cursor for a follow-up pull).
    async fn event_history(
        &self,
        req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, StorageError>;

    /// Owner-scoped, optionally actor-filtered MCP-call activity log,
    /// newest-first. Server clamps `limit` to
    /// `MAX_MCP_CALL_HISTORY_LIMIT`.
    async fn read_mcp_call_history(
        &self,
        req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, StorageError>;

    /// Owner-scoped snapshot read of memories per docs/14 §"Query".
    /// Returns `MemoryRow` substrate shape with typed payload projections
    /// from sidecar tables. `schemas` is the list of registered schemas
    /// with sidecar tables for dynamic JOIN construction.
    async fn query_memories(
        &self,
        req: &crate::verbs::query::QueryRequest,
        schemas: &[crate::verbs::schema::SchemaInfo],
    ) -> Result<crate::verbs::query::QueryResponse, StorageError>;

    /// Read-set-scoped edge read by edge id and/or endpoint filter.
    async fn read_edges(
        &self,
        read_owners: &[Principal],
        req: &crate::verbs::query::EdgeReadRequest,
    ) -> Result<crate::verbs::query::EdgeReadResponse, StorageError>;

    /// Read-set-scoped existence probe for an edge id and/or endpoint filter.
    async fn edge_exists(
        &self,
        read_owners: &[Principal],
        req: &crate::verbs::query::EdgeExistsRequest,
    ) -> Result<crate::verbs::query::EdgeExistsResponse, StorageError>;

    /// Owner-scoped lexical/semantic memory search. Similarity is
    /// query-time only; this method never writes edges.
    async fn search_memories(
        &self,
        req: &crate::verbs::query::MemorySearchRequest,
        projections: &[crate::verbs::schema::MemorySearchProjection],
    ) -> Result<Vec<crate::verbs::query::MemorySearchResult>, StorageError>;

    /// Owner-scoped lookup of the stable aggregate identity for a
    /// stateful Fact natural key. One unique-index probe; no head read.
    async fn fact_entity_id_for(
        &self,
        owner: &Owner,
        schema_id: &SchemaId,
        schema_version: SchemaVersion,
        natural_key: &[String],
    ) -> Result<Option<FactEntityId>, StorageError>;

    /// Read-set-scoped discovery of Fact rows whose citation mapping points at
    /// one cited object.
    async fn facts_citing_object(
        &self,
        read_owners: &[Principal],
        cited_object_id: uuid::Uuid,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<crate::personality::MemorySnapshot>, StorageError>;

    /// Plain inverse read-back from one already-authorized Fact to its citation
    /// mapping and cited object, if present.
    async fn citation_of_fact(
        &self,
        fact_memory_id: crate::MemoryId,
    ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError>;

    /// Read-set-scoped inverse read-back from a stateful Fact entity's current
    /// readable head to that head version's citation mapping and cited object,
    /// if present.
    async fn citation_of_entity_head(
        &self,
        read_owners: &[Principal],
        fact_entity_id: FactEntityId,
    ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError>;

    /// Read-set-scoped bounded walk over memory-only Provenance and
    /// Supersession edges. Does not traverse Goals or write edges.
    async fn walk_memory_lineage(
        &self,
        read_owners: &[Principal],
        req: &crate::verbs::query::MemoryLineageRequest,
    ) -> Result<crate::verbs::query::MemoryLineageResponse, StorageError>;

    /// Read-set-scoped active Goal query for one personality Self-Perspective.
    /// Traverses `core/inspires` edges authored at proposal/attachment time,
    /// follows Goal supersession forward, and returns only current Active
    /// heads. No `GoalConnection` sidecar is modeled.
    async fn list_active_goals(
        &self,
        read_owners: &[Principal],
        self_perspective_memory_id: crate::MemoryId,
        limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError>;

    /// Storage-backed group memberships for one principal. User
    /// principals resolve explicit group rows; group principals have no
    /// recursive memberships in v0.0.1.
    async fn resolve_membership(
        &self,
        member: &Principal,
    ) -> Result<Vec<MembershipRow>, StorageError>;

    /// Entity reachability predicate over the caller's resolved read-owner
    /// set. Dead/tombstoned entities have no `entity_owner` rows.
    async fn entity_is_readable(
        &self,
        entity: EntityId,
        read_owners: &[Principal],
    ) -> Result<bool, StorageError>;

    /// Home owner lookup for a Memory or Goal entity. Dead/tombstoned
    /// entities have no `entity_owner` rows and return `None`.
    async fn entity_home_owner(&self, entity: EntityId) -> Result<Option<Principal>, StorageError>;

    /// Add a read-only `entity_owner` share row. Idempotent.
    async fn add_entity_owner_share(
        &self,
        entity: EntityId,
        owner: &Principal,
        granted_by: Option<uuid::Uuid>,
    ) -> Result<(), StorageError>;

    /// Remove a read-only `entity_owner` share row. Home rows are never
    /// removed through this path.
    async fn remove_entity_owner_share(
        &self,
        entity: EntityId,
        owner: &Principal,
    ) -> Result<RemoveOwnerOutcome, StorageError>;

    /// List all reachability rows for one entity.
    async fn list_entity_owners(
        &self,
        entity: EntityId,
    ) -> Result<Vec<EntityOwnerRow>, StorageError>;

    /// Public marketplace view: memories carrying a World `entity_owner`
    /// row, newest first.
    async fn list_world_entities(
        &self,
        limit: usize,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError>;

    /// Add one group membership relation. Idempotent.
    async fn add_group_member(
        &self,
        group_id: GroupId,
        member_user_id: UserId,
        relation: Relation,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError>;

    /// Remove all membership relations for one user in one group.
    async fn remove_group_member(
        &self,
        group_id: GroupId,
        member_user_id: UserId,
    ) -> Result<(), StorageError>;

    /// List group members and their relations.
    async fn list_group_members(
        &self,
        group_id: GroupId,
    ) -> Result<Vec<(UserId, Relation)>, StorageError>;

    /// Owner-scoped, idempotent batch close. See docs/01 §"The contract"
    /// and docs/04 §"Source-batch lifecycle". Flips
    /// `source_batches.closed_at` from NULL to `now()`. Re-close is a
    /// no-op returning the existing `closed_at` with `already_closed = true`.
    /// A batch belonging to a different owner returns `NotFound`.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when the batch doesn't exist or belongs to a
    /// different owner. sqlx failures map to `Internal`.
    async fn close_batch(
        &self,
        principal: &Principal,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError>;

    /// List configured personality instances for an owner. When
    /// `include_tombstoned` is `false` (the default for UI listings),
    /// rows whose status is `tombstoned` are filtered out.
    /// Implementations populate each row's active `wake_entries`.
    async fn list_personality_instances(
        &self,
        owner: &Owner,
        include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, StorageError>;

    /// Mark a personality instance tombstoned. Subsequent dispatcher
    /// ticks must skip it. Idempotent on the natural key: repeats
    /// return `idempotent_replay = true` without rewriting
    /// `tombstoned_at`.
    async fn tombstone_personality(
        &self,
        req: &TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, StorageError>;

    /// Instantiate one inert personality instance with its Root
    /// Perspective (a `Perspective` memory stamped with
    /// `ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID`) and cursor rows.
    async fn instantiate_personality(
        &self,
        req: &InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, StorageError>;

    /// Ensure a per-master-token shell-author personality exists for
    /// the `(owner, master_token_id)` pair. Idempotent: returns the
    /// existing identity on replay, or mints a fresh personality with
    /// `display_name = "shell-author"`, an empty `WakeConfig`, and a row
    /// in `proxima_core.master_token_personality`.
    async fn ensure_master_token_personality(
        &self,
        owner: &Owner,
        master_token_id: uuid::Uuid,
    ) -> Result<MasterTokenPersonality, StorageError>;

    /// Ensure a per-subject personality exists for the
    /// `(owner, subject principal)` pair. Idempotent: returns the
    /// existing identity on replay, or mints a fresh personality with a
    /// Root Perspective and a row in
    /// `proxima_core.subject_personality`.
    async fn ensure_subject_personality(
        &self,
        owner: &Owner,
        subject: &Principal,
    ) -> Result<MasterTokenPersonality, StorageError>;

    /// Replace active `WakeEntry` rows for one personality instance.
    async fn set_wake_entries(
        &self,
        req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, StorageError>;

    /// Transactional read-modify-write over a personality's `WakeConfig`.
    /// Locks the personality row (SELECT FOR UPDATE), reads current active
    /// wake entries, applies the `mutate` closure, then replaces all entries
    /// atomically. Used by granular add/update/remove ops to serialise
    /// concurrent mutations on the same personality.
    async fn set_wake_entries_within(
        &self,
        owner: &Owner,
        personality_instance_id: PersonalityInstanceId,
        mutate: WakeEntriesMutator,
    ) -> Result<SetWakeEntriesResponse, StorageError>;

    /// List explicit read-scope grants for one reader personality. Identity
    /// reads are implicit and are not returned.
    async fn list_read_scope(
        &self,
        req: &ListReadScopeRequest,
    ) -> Result<ListReadScopeResponse, StorageError>;

    /// Replace explicit read-scope grants for one reader personality. Identity
    /// reads remain implicit even when omitted.
    async fn set_read_scope(
        &self,
        req: &SetReadScopeRequest,
    ) -> Result<SetReadScopeResponse, StorageError>;

    /// Upsert the owner-scoped Fact-retention duration, in seconds.
    async fn upsert_fact_retention(&self, owner: &Owner, seconds: i64) -> Result<(), StorageError>;

    /// Read the owner-scoped Fact-retention duration, in seconds.
    async fn get_fact_retention(&self, owner: &Owner) -> Result<Option<i64>, StorageError>;

    /// Clear the owner-scoped Fact-retention duration.
    async fn clear_fact_retention(&self, owner: &Owner) -> Result<bool, StorageError>;

    /// Hard-erase due Facts for `owner`, tombstone transitive derived
    /// memory dependents, and erase orphaned citation backing rows.
    async fn cleanup_due_facts(
        &self,
        owner: &Owner,
        fact_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<CleanupDueFactsOutcome, StorageError>;

    /// Hard-erase one owner-scoped Fact by id, tombstone transitive
    /// derived memory dependents, and erase orphaned citation backing rows.
    async fn tombstone_fact(
        &self,
        owner: &Owner,
        fact_id: uuid::Uuid,
        fact_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<TombstoneFactOutcome, StorageError>;

    async fn list_change_events_after(
        &self,
        read_owners: &[Principal],
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError>;

    async fn list_change_events_for_replay(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        until: Option<uuid::Uuid>,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        let rows = self
            .list_change_events_after(std::slice::from_ref(owner), after, limit)
            .await?;
        Ok(rows
            .into_iter()
            .filter(|row| until.is_none_or(|until| row.event.seq <= until))
            .collect())
    }

    async fn load_memory_batch_facts(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<crate::FactRow>, StorageError>;

    async fn load_abstraction_heads(
        &self,
        owner: &Owner,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError>;

    async fn load_perspective_heads(
        &self,
        owner: &Owner,
        instance: PersonalityInstanceId,
        root_perspective_memory_id: crate::MemoryId,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<MemorySnapshot>, StorageError>;

    async fn lookup_prior_personality_head(
        &self,
        owner: &Owner,
        instance: &PersonalityRef,
        schema_id: &crate::SchemaId,
    ) -> Result<Option<crate::MemoryId>, StorageError>;

    async fn append_personality_memories(
        &self,
        req: &PersonalityWriteRequest<'_>,
    ) -> Result<PersonalityWriteOutcome, StorageError>;

    /// Unconditional load of an id already authorized via `authorize_entry_read`;
    /// carries no ownership filter by design.
    async fn load_memory_by_id(
        &self,
        memory_id: crate::MemoryId,
        reader_personality_instance_id: Option<PersonalityInstanceId>,
        sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError>;

    async fn list_memory_dependencies(
        &self,
        _owner: &Owner,
        _source_memory_id: crate::MemoryId,
    ) -> Result<Vec<MemoryDependency>, StorageError> {
        Ok(Vec::new())
    }
}

pub type StorageHandle = Arc<dyn Storage>;

/// Storage that rejects all writes — used by the in-memory
/// demo path and by tests that don't want PG.
#[derive(Debug, Default, Clone)]
pub struct NoopStorage;

#[async_trait::async_trait]
impl Storage for NoopStorage {
    async fn ingest_event_atomic(
        &self,
        _draft: &EventDraft,
        _embedding_model_id: Option<&str>,
    ) -> Result<EventIngestOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn persist_mcp_call_atomic(
        &self,
        _input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn ingest_event_with_typed_sidecar(
        &self,
        _authorized: &AuthorizedEventIngest,
        _sidecar_payload: &SidecarPayload,
        _embedding_model_id: Option<&str>,
    ) -> Result<EventIngestOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        _authorized: &AuthorizedFactWithCitation,
        _sidecar_payload: &SidecarPayload,
        _embedding_model_id: Option<&str>,
    ) -> Result<EventIngestOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn fact_entity_id_for(
        &self,
        _owner: &Owner,
        _schema_id: &SchemaId,
        _schema_version: SchemaVersion,
        _natural_key: &[String],
    ) -> Result<Option<FactEntityId>, StorageError> {
        Ok(None)
    }

    async fn author_derived(
        &self,
        _req: &AuthorDerivedRequest<'_>,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn append_memory_edge(
        &self,
        _edge: &DerivedEdgeSpec<'_>,
    ) -> Result<EdgeId, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn load_fact_text(
        &self,
        _owner: &Owner,
        _memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    async fn load_embedding_text(
        &self,
        _owner: &Owner,
        _entity_kind: EntityKind,
        _memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    async fn upsert_fact_embedding(
        &self,
        _owner: &Owner,
        _memory_id: crate::MemoryId,
        _model_id: &str,
        _dim: usize,
        _vec: &[f32],
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn upsert_memory_embedding(
        &self,
        _owner: &Owner,
        _entity_kind: EntityKind,
        _memory_id: crate::MemoryId,
        _model_id: &str,
        _dim: usize,
        _vec: &[f32],
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn list_facts_missing_embedding(
        &self,
        _owner: &Owner,
        _model_id: &str,
        _limit: usize,
    ) -> Result<Vec<crate::MemoryId>, StorageError> {
        Ok(Vec::new())
    }

    async fn claim_pending_embedding_jobs(
        &self,
        _model_id: &str,
        _limit: i64,
    ) -> Result<Vec<EmbeddingJobClaim>, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn complete_embedding_job(&self, _claim: &EmbeddingJobClaim) -> Result<(), StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn fail_embedding_job(
        &self,
        _claim: &EmbeddingJobClaim,
        _error: &str,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn enqueue_missing_embedding_jobs(
        &self,
        _owner: &Owner,
        _model_id: &str,
        _limit: i64,
    ) -> Result<u64, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn count_pending_embedding_jobs(&self, _owner: &Owner) -> Result<u64, StorageError> {
        Ok(0)
    }

    async fn create_goal_atomic(
        &self,
        _req: &CreateGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn transition_goal_atomic(
        &self,
        _req: &TransitionGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn achieve_goal_atomic(
        &self,
        _req: &AchieveGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn modify_goal_atomic(
        &self,
        _req: &ModifyGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn decompose_goal_atomic(
        &self,
        _req: &DecomposeGoalAtomicRequest<'_>,
    ) -> Result<DecomposeGoalOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn event_history(
        &self,
        _req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, StorageError> {
        Ok(EventHistoryResponse {
            events: Vec::new(),
            seq_high_water: None,
        })
    }

    async fn read_mcp_call_history(
        &self,
        _req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, StorageError> {
        Ok(McpCallHistoryResponse { calls: Vec::new() })
    }

    async fn query_memories(
        &self,
        _req: &crate::verbs::query::QueryRequest,
        _schemas: &[crate::verbs::schema::SchemaInfo],
    ) -> Result<crate::verbs::query::QueryResponse, StorageError> {
        Ok(crate::verbs::query::QueryResponse {
            memories: Vec::new(),
            goals: Vec::new(),
            edges: Vec::new(),
            seq_high_water: None,
        })
    }

    async fn read_edges(
        &self,
        _read_owners: &[Principal],
        _req: &crate::verbs::query::EdgeReadRequest,
    ) -> Result<crate::verbs::query::EdgeReadResponse, StorageError> {
        Ok(crate::verbs::query::EdgeReadResponse { edges: Vec::new() })
    }

    async fn edge_exists(
        &self,
        _read_owners: &[Principal],
        _req: &crate::verbs::query::EdgeExistsRequest,
    ) -> Result<crate::verbs::query::EdgeExistsResponse, StorageError> {
        Ok(crate::verbs::query::EdgeExistsResponse { exists: false })
    }

    async fn search_memories(
        &self,
        _req: &crate::verbs::query::MemorySearchRequest,
        _projections: &[crate::verbs::schema::MemorySearchProjection],
    ) -> Result<Vec<crate::verbs::query::MemorySearchResult>, StorageError> {
        Ok(Vec::new())
    }

    async fn facts_citing_object(
        &self,
        _read_owners: &[Principal],
        _cited_object_id: uuid::Uuid,
        _sidecars: &[SidecarSpec],
    ) -> Result<Vec<crate::personality::MemorySnapshot>, StorageError> {
        Ok(Vec::new())
    }

    async fn citation_of_fact(
        &self,
        _fact_memory_id: crate::MemoryId,
    ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError> {
        Ok(None)
    }

    async fn citation_of_entity_head(
        &self,
        _read_owners: &[Principal],
        _fact_entity_id: FactEntityId,
    ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError> {
        Ok(None)
    }

    async fn walk_memory_lineage(
        &self,
        _read_owners: &[Principal],
        _req: &crate::verbs::query::MemoryLineageRequest,
    ) -> Result<crate::verbs::query::MemoryLineageResponse, StorageError> {
        Ok(crate::verbs::query::MemoryLineageResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
        })
    }

    async fn list_active_goals(
        &self,
        _read_owners: &[Principal],
        _self_perspective_memory_id: crate::MemoryId,
        _limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        Ok(Vec::new())
    }

    async fn resolve_membership(
        &self,
        _member: &Principal,
    ) -> Result<Vec<MembershipRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn entity_is_readable(
        &self,
        _entity: EntityId,
        _read_owners: &[Principal],
    ) -> Result<bool, StorageError> {
        Ok(false)
    }

    async fn entity_home_owner(
        &self,
        _entity: EntityId,
    ) -> Result<Option<Principal>, StorageError> {
        Ok(None)
    }

    async fn add_entity_owner_share(
        &self,
        _entity: EntityId,
        _owner: &Principal,
        _granted_by: Option<uuid::Uuid>,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn remove_entity_owner_share(
        &self,
        _entity: EntityId,
        _owner: &Principal,
    ) -> Result<RemoveOwnerOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn list_entity_owners(
        &self,
        _entity: EntityId,
    ) -> Result<Vec<EntityOwnerRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn list_world_entities(
        &self,
        _limit: usize,
        _sidecars: &[SidecarSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        Ok(Vec::new())
    }

    async fn add_group_member(
        &self,
        _group_id: GroupId,
        _member_user_id: UserId,
        _relation: Relation,
        _granted_by: uuid::Uuid,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn remove_group_member(
        &self,
        _group_id: GroupId,
        _member_user_id: UserId,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn list_group_members(
        &self,
        _group_id: GroupId,
    ) -> Result<Vec<(UserId, Relation)>, StorageError> {
        Ok(Vec::new())
    }

    async fn close_batch(
        &self,
        _principal: &Principal,
        _source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn list_personality_instances(
        &self,
        _owner: &Owner,
        _include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn tombstone_personality(
        &self,
        _req: &TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn instantiate_personality(
        &self,
        _req: &InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn ensure_master_token_personality(
        &self,
        _owner: &Owner,
        _master_token_id: uuid::Uuid,
    ) -> Result<MasterTokenPersonality, StorageError> {
        Err(StorageError::Internal(
            "mock: ensure_master_token_personality not stubbed".into(),
        ))
    }

    async fn ensure_subject_personality(
        &self,
        _owner: &Owner,
        _subject: &Principal,
    ) -> Result<MasterTokenPersonality, StorageError> {
        Err(StorageError::Internal(
            "mock: ensure_subject_personality not stubbed".into(),
        ))
    }

    async fn set_wake_entries(
        &self,
        _req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn set_wake_entries_within(
        &self,
        _owner: &Owner,
        _personality_instance_id: PersonalityInstanceId,
        _mutate: WakeEntriesMutator,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn list_read_scope(
        &self,
        _req: &ListReadScopeRequest,
    ) -> Result<ListReadScopeResponse, StorageError> {
        Ok(ListReadScopeResponse {
            readable_personality_instance_ids: Vec::new(),
        })
    }

    async fn set_read_scope(
        &self,
        _req: &SetReadScopeRequest,
    ) -> Result<SetReadScopeResponse, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn upsert_fact_retention(
        &self,
        _owner: &Owner,
        _seconds: i64,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn get_fact_retention(&self, _owner: &Owner) -> Result<Option<i64>, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn clear_fact_retention(&self, _owner: &Owner) -> Result<bool, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn cleanup_due_facts(
        &self,
        _owner: &Owner,
        _fact_sidecar_tables: &[String],
        _edge_sidecar_tables: &[String],
        _citation_mapping_sidecar_tables: &[String],
        _cited_object_sidecar_tables: &[String],
    ) -> Result<CleanupDueFactsOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn tombstone_fact(
        &self,
        _owner: &Owner,
        _fact_id: uuid::Uuid,
        _fact_sidecar_tables: &[String],
        _edge_sidecar_tables: &[String],
        _citation_mapping_sidecar_tables: &[String],
        _cited_object_sidecar_tables: &[String],
    ) -> Result<TombstoneFactOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn list_change_events_after(
        &self,
        _read_owners: &[Principal],
        _after: uuid::Uuid,
        _limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_memory_batch_facts(
        &self,
        _owner: &Owner,
        _memory_id: crate::MemoryId,
        _sidecars: &[SidecarSpec],
    ) -> Result<Vec<crate::FactRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_abstraction_heads(
        &self,
        _owner: &Owner,
        _sidecars: &[SidecarSpec],
        _limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_perspective_heads(
        &self,
        _owner: &Owner,
        _instance: PersonalityInstanceId,
        _root_perspective_memory_id: crate::MemoryId,
        _sidecars: &[SidecarSpec],
        _limit: usize,
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        Ok(Vec::new())
    }

    async fn lookup_prior_personality_head(
        &self,
        _owner: &Owner,
        _instance: &PersonalityRef,
        _schema_id: &crate::SchemaId,
    ) -> Result<Option<crate::MemoryId>, StorageError> {
        Ok(None)
    }

    async fn append_personality_memories(
        &self,
        _req: &PersonalityWriteRequest<'_>,
    ) -> Result<PersonalityWriteOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn load_memory_by_id(
        &self,
        _memory_id: crate::MemoryId,
        _reader_personality_instance_id: Option<PersonalityInstanceId>,
        _sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        Ok(None)
    }
}
