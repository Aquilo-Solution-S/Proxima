//! Query verb — owner-scoped snapshot read of memories.
//!
//! See docs/14-protocol-surface.md §"Query" and
//! docs/02-memory.md.

use uuid::Uuid;

pub use crate::change_event::EdgeTargetProjection;
use crate::change_event::EntityRef;
use crate::verbs::goal_write::GoalState;
use crate::verbs::schema::SchemaTombstone;
use crate::{EdgeId, GoalId, MemoryId, Owner, OwnerRef, SchemaId, SchemaVersion, SidecarPayload};

/// Re-export the canonical `EntityKind` from `change_event` so query
/// callers don't need a second import path. The duplicate
/// definition that lived here pre-M6.5 produced two identical
/// types.
pub use crate::change_event::EntityKind;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Lexical,
    Semantic,
    Hybrid,
}

fn default_search_mode() -> SearchMode {
    SearchMode::Hybrid
}

fn default_supersession_status() -> SupersessionStatus {
    SupersessionStatus::HeadsOnly
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum TagMatch {
    #[default]
    Any,
    All,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum SearchOrder {
    #[default]
    Relevance,
    Recency,
}

/// Owner-scoped memory search. Semantic modes require the engine/tool
/// layer to populate the query embedding and active embedding-space
/// metadata before dispatching to storage.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemorySearchRequest {
    pub owner: OwnerRef,
    #[serde(skip)]
    pub read_owners: Vec<OwnerRef>,
    pub query: String,
    #[serde(default = "default_search_mode")]
    pub mode: SearchMode,
    #[serde(default = "default_supersession_status")]
    pub supersession: SupersessionStatus,
    pub limit: u32,
    pub kind: Option<EntityKind>,
    pub schema_id: Option<SchemaId>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub tag_match: TagMatch,
    #[serde(default)]
    pub since: Option<time::OffsetDateTime>,
    #[serde(default)]
    pub until: Option<time::OffsetDateTime>,
    #[serde(default)]
    pub order: SearchOrder,
    #[serde(skip)]
    pub query_embedding: Option<Vec<f32>>,
    #[serde(skip)]
    pub embedding_model_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemorySearchResult {
    pub memory_id: MemoryId,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub created_at: time::OffsetDateTime,
    pub snippet: String,
    pub score: f32,
    pub lexical_score: f32,
    pub similarity_score: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FactCitationReadback {
    pub citation_mapping_id: uuid::Uuid,
    pub mapping_schema_id: SchemaId,
    pub cited_object_id: uuid::Uuid,
    pub cited_object_schema_id: SchemaId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MemoryLineageDirection {
    Ancestors,
    Descendants,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLineageRequest {
    pub owner: OwnerRef,
    pub start_memory_id: MemoryId,
    pub direction: MemoryLineageDirection,
    pub depth: u8,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLineageNode {
    pub memory_id: MemoryId,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub snippet: String,
    pub distance: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLineageEdge {
    pub edge_id: uuid::Uuid,
    pub relation: String,
    pub relation_class: String,
    pub source_kind: EntityKind,
    pub source_memory_id: MemoryId,
    pub target: EdgeTargetProjection,
    pub distance: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLineageResponse {
    pub nodes: Vec<MemoryLineageNode>,
    pub edges: Vec<MemoryLineageEdge>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SupersessionStatus {
    /// Heads only — exclude rows that are superseded.
    HeadsOnly,
    /// Include superseded rows.
    IncludeSuperseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TombstoneFilter {
    PresentOnly,
    IncludeTombstoned,
}

fn default_tombstone_filter() -> TombstoneFilter {
    TombstoneFilter::PresentOnly
}

fn default_include_payloads() -> bool {
    true
}

/// Engine-resolved head-by-natural-key filter for stateful Fact
/// schemas (docs/03 §Stateful Fact schemas). Populated from the
/// schema registry when `Engine::query` sees a heads-only request
/// against a stateful Fact schema; storage uses it to emit the
/// per-NK head SQL. Internal — clients do not set this directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatefulHeadsFilter {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub sidecar_table: String,
    pub natural_key_columns: Vec<String>,
    pub tombstone: Option<SchemaTombstone>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum QueryCursor {
    Memory {
        created_at: time::OffsetDateTime,
        memory_id: MemoryId,
    },
    Goal {
        created_at: time::OffsetDateTime,
        goal_id: GoalId,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueryPage {
    #[serde(default)]
    pub after: Option<QueryCursor>,
}

/// One core-generic Query request. Flavor-typed filters
/// per docs/14 §"Query" land when the first flavor crate
/// registers a sidecar (M3+).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueryRequest {
    pub owner: OwnerRef,
    #[serde(skip)]
    pub read_owners: Vec<OwnerRef>,
    pub entity_kind: Option<EntityKind>,
    pub schema_id: Option<SchemaId>,
    pub supersession: SupersessionStatus,
    #[serde(default = "default_tombstone_filter")]
    pub tombstones: TombstoneFilter,
    pub limit: u32,
    #[serde(default)]
    pub page: QueryPage,
    /// Include typed payload projections in returned rows. Broad graph snapshots can
    /// set this false and hydrate selected IDs later.
    #[serde(default = "default_include_payloads")]
    pub include_payloads: bool,
    /// Identity-keyed hydration for Subscribe-driven row fetches.
    #[serde(default)]
    pub memory_ids: Vec<MemoryId>,
    #[serde(default)]
    pub goal_ids: Vec<GoalId>,
    #[serde(default)]
    pub edge_ids: Vec<uuid::Uuid>,
    /// Engine-resolved metadata for stateful-Fact heads-only queries.
    /// Skipped over the wire — clients don't set this; the engine
    /// populates it from the schema registry before dispatch.
    #[serde(skip)]
    pub stateful_heads: Vec<StatefulHeadsFilter>,
}

impl QueryRequest {
    /// Builder for the common case: heads-only, no kind/schema
    /// filter.
    #[must_use]
    pub fn for_owner(owner: OwnerRef) -> Self {
        Self {
            owner,
            read_owners: vec![owner],
            entity_kind: None,
            schema_id: None,
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: TombstoneFilter::PresentOnly,
            limit: 100,
            page: QueryPage::default(),
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: Vec::new(),
        }
    }
}

/// Snapshot of a memory row. Goal rows have their own shape
/// (M2+); not modelled here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRow {
    pub id: MemoryId,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub owner: Owner,
    /// Typed sidecar projection populated by storage at read time. Protocol
    /// adapters serialize it at the transport boundary.
    pub payload: Option<SidecarPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalRow {
    pub id: GoalId,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub owner: Owner,
    pub title: String,
    pub text: String,
    pub state: GoalState,
    pub dependency_goal_ids: Vec<GoalId>,
    pub supersedes: Option<GoalId>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EdgeRow {
    pub id: uuid::Uuid,
    pub relation: String,
    pub relation_class: String,
    pub source: EntityRef,
    pub target: EdgeTargetProjection,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EdgeFilter {
    pub relation: Option<String>,
    pub source: Option<EntityRef>,
    pub target: Option<EntityRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EdgeReadRequest {
    pub owner: OwnerRef,
    #[serde(default)]
    pub edge_ids: Vec<EdgeId>,
    #[serde(default)]
    pub filter: EdgeFilter,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeReadResponse {
    pub edges: Vec<EdgeRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EdgeExistsRequest {
    pub owner: OwnerRef,
    #[serde(default)]
    pub edge_ids: Vec<EdgeId>,
    #[serde(default)]
    pub filter: EdgeFilter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EdgeExistsResponse {
    pub exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryResponse {
    pub memories: Vec<MemoryRow>,
    pub goals: Vec<GoalRow>,
    pub edges: Vec<EdgeRow>,
    pub next_cursor: Option<QueryCursor>,
    /// docs/14 §"Cursor & resume".
    pub seq_high_water: Option<Uuid>,
}
