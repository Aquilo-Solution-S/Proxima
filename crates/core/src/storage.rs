//! Storage DTOs and backend-neutral errors.
//!
//! Port traits live in [`crate::storage_ports`]. See docs/07-storage.md and
//! AGENTS.md invariants 2, 3, 5.

use std::any::{Any, TypeId};
use std::sync::Arc;

use crate::personality::{PersonalityInstanceId, WakeEntryDraft};
use crate::{
    AbstractionPayload, CitationMappingPayload, CitedObjectPayload, EdgePayload, FactPayload,
    GoalPayload, PerspectivePayload,
};
use crate::{
    EdgeAuthorshipKind, EdgeId, EntityKind, MemoryId, MemoryOperatorKind, Owner,
    RegisteredRelation, SchemaId, SchemaVersion,
};

#[derive(Debug, Clone, thiserror::Error)]
pub enum StorageError {
    #[error("storage backend unavailable: {0}")]
    Unavailable(String),
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error(
        "database contains pre-v0.0.4 Proxima schema artifacts; export/reset is required before running the v0.0.4 baseline: {details}"
    )]
    V004ResetRequired { details: String },
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
/// Returned by the personality write storage port.
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
