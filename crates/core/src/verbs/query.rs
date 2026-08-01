//! Query verb — owner-scoped snapshot read of memories.
//!
//! See docs/14-protocol-surface.md §"Query" and
//! docs/02-memory.md.

use uuid::Uuid;

use crate::change_event::EntityRef;
pub use crate::edge::EdgeTargetProjection;
use crate::edge::{Edge, EdgeKind};
use crate::verbs::goal_write::GoalState;
use crate::verbs::schema::SchemaTombstone;
use crate::{GoalId, MemoryId, Owner, OwnerRef, SchemaId, SchemaVersion, SidecarPayload};

/// Re-export the canonical `EntityKind` from `change_event` so query
/// callers don't need a second import path. The duplicate
/// definition that lived here previously produced two identical
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
    #[serde(alias = "Any", alias = "ANY")]
    Any,
    #[serde(alias = "All", alias = "ALL")]
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
    #[serde(alias = "Relevance", alias = "RELEVANCE")]
    Relevance,
    #[serde(alias = "Recency", alias = "RECENCY")]
    Recency,
}

/// Hard per-page cap on memory search results. Callers page past it
/// with [`MemorySearchRequest::after`] / the MCP-level cursor.
pub const MAX_SEARCH_PAGE_LIMIT: u32 = 50;

/// Hybrid fusion weight on the semantic component when the request
/// does not override it; the lexical component gets the complement.
pub const DEFAULT_HYBRID_SEMANTIC_WEIGHT: f32 = 0.6;

/// Upper bound on [`SearchCursor::Relevance`] depth (`seen`). Relevance
/// keysets re-rank an overfetched candidate window that grows with
/// depth, so depth is bounded; recency keysets push into SQL and page
/// without bound.
pub const MAX_RELEVANCE_SEARCH_DEPTH: u32 = 5_000;

/// Resume point for paged memory search. The variant must match the
/// request's `order`. A `Relevance` cursor carries the fused score of
/// the last emitted row as exact bits ([`f32::to_bits`]) plus the total
/// rows already emitted (`seen`), which storage uses to widen its
/// candidate overfetch window; a `Recency` cursor is a plain
/// `(created_at, memory_id)` keyset pushed into SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SearchCursor {
    Relevance {
        score_bits: u32,
        memory_id: MemoryId,
        seen: u32,
    },
    Recency {
        created_at: time::OffsetDateTime,
        memory_id: MemoryId,
        seen: u32,
    },
}

impl SearchCursor {
    /// The ordering this cursor was issued under.
    #[must_use]
    pub fn order(&self) -> SearchOrder {
        match self {
            Self::Relevance { .. } => SearchOrder::Relevance,
            Self::Recency { .. } => SearchOrder::Recency,
        }
    }

    /// Total results emitted by the pages before this cursor.
    #[must_use]
    pub fn seen(&self) -> u32 {
        match *self {
            Self::Relevance { seen, .. } | Self::Recency { seen, .. } => seen,
        }
    }
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
    /// Drop results whose mode-appropriate fused score is below this
    /// floor (0..=1). `None` disables the floor.
    #[serde(default)]
    pub min_score: Option<f32>,
    /// Hybrid fusion weight on the semantic component (0..=1); the
    /// lexical component gets the complement. `None` uses
    /// [`DEFAULT_HYBRID_SEMANTIC_WEIGHT`]. Unused outside `Hybrid`.
    #[serde(default)]
    pub semantic_weight: Option<f32>,
    /// Resume point from a previous page. The variant must match
    /// `order`.
    #[serde(default)]
    pub after: Option<SearchCursor>,
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

/// One page of search results. `has_more` reports whether at least one
/// further post-floor match exists past the last returned row inside
/// the ranking horizon (relevance ordering re-ranks an overfetched
/// candidate window, so a false negative is possible at extreme
/// depths; recency ordering is exact).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemorySearchPage {
    pub results: Vec<MemorySearchResult>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FactCitationReadback {
    pub citation_mapping_id: uuid::Uuid,
    pub mapping_schema_id: SchemaId,
    pub cited_object_id: uuid::Uuid,
    pub cited_object_schema_id: SchemaId,
    /// The `core/uploaded-blob-page-span-v1` locator, when the mapping
    /// carries one.
    pub page_span: Option<crate::citations::UploadedBlobPageSpanV1>,
    /// Uploaded-document metadata, when the cited object is a
    /// `core/uploaded-blob-v1`.
    pub uploaded_blob: Option<UploadedBlobRef>,
}

/// Client-safe description of an uploaded cited blob: what the document
/// IS, never where it lives. `bucket`/`object_key` are deliberately
/// absent (docs/10 §Large Artefact S3 — presigned URLs only); fetching
/// bytes goes through `core_upload`'s `read_url`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedBlobRef {
    pub filename: String,
    pub mime: String,
    pub byte_len: u64,
    pub sha256_hex: String,
    pub uploaded_at: time::OffsetDateTime,
}

/// Keyset position in the citing-Facts total order
/// (`created_at DESC, memory_id DESC`): the next page starts strictly
/// after this Fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactCitationCursor {
    pub created_at: time::OffsetDateTime,
    pub memory_id: MemoryId,
}

/// One page of Facts citing an object, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactCitationPage {
    pub facts: Vec<crate::read_models::MemorySnapshot>,
    /// Resume point for the page after this one; `Some` iff `has_more`.
    pub next_cursor: Option<FactCitationCursor>,
    pub has_more: bool,
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
    /// Keyset resume point from a previous response's `next_cursor`;
    /// `None` starts from the first page.
    pub after: Option<MemoryLineageCursor>,
}

/// Keyset position in the lineage walk's total order
/// (`distance ASC`, then the edge primary key descending): the next page
/// starts strictly after this edge. Pages recompute the walk, so the
/// usual keyset caveat applies — mutations between pages shift later
/// pages.
///
/// The position is the edge itself because an edge has no id: its
/// content is its identity, and `(source, target)` is what remains to
/// order by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemoryLineageCursor {
    pub distance: u8,
    pub source: EntityRef,
    pub target: EntityRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLineageNode {
    pub memory_id: MemoryId,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub snippet: String,
    pub distance: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryLineageEdge {
    pub edge: Edge,
    pub distance: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLineageResponse {
    pub nodes: Vec<MemoryLineageNode>,
    pub edges: Vec<MemoryLineageEdge>,
    pub truncated: bool,
    /// Resume point for the page after this one; `Some` when `truncated`
    /// and the page carries at least one edge to resume after. The wire
    /// layer derives its `has_more` from this field, not `truncated`.
    pub next_cursor: Option<MemoryLineageCursor>,
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
    /// Goal-stream state filter. Only meaningful with
    /// `entity_kind == Some(EntityKind::Goal)`; other streams ignore it.
    #[serde(default)]
    pub goal_state: Option<GoalState>,
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
            goal_state: None,
            limit: 100,
            page: QueryPage::default(),
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
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

/// Edge listing filter. Every field is a narrowing predicate over the
/// index; a request with none of them is refused by the read surface
/// rather than dumping the graph.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EdgeFilter {
    pub kind: Option<EdgeKind>,
    pub source: Option<EntityRef>,
    pub target: Option<EntityRef>,
}

/// Keyset cursor over the newest-first edge order. An edge has no id, so
/// the tiebreaker after `created_at` is the rest of the primary key —
/// `(source, target, kind)` — which is exactly what makes the order
/// total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EdgeReadCursor {
    pub created_at: time::OffsetDateTime,
    pub source: EntityRef,
    pub target: EntityRef,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EdgeReadRequest {
    pub owner: OwnerRef,
    #[serde(default)]
    pub filter: EdgeFilter,
    pub limit: u32,
    /// Resume after this keyset position (newest-first order).
    #[serde(default)]
    pub cursor: Option<EdgeReadCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeReadResponse {
    pub edges: Vec<Edge>,
    /// Present when more edges match beyond this page; pass back via
    /// [`EdgeReadRequest::cursor`] to resume.
    pub next_cursor: Option<EdgeReadCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EdgeExistsRequest {
    pub owner: OwnerRef,
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
    pub edges: Vec<Edge>,
    pub next_cursor: Option<QueryCursor>,
    /// docs/14 §"Cursor & resume".
    pub seq_high_water: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::{SearchOrder, TagMatch};

    #[test]
    fn tag_match_and_order_accept_mixed_case() {
        assert_eq!(
            serde_json::from_value::<TagMatch>(serde_json::json!("All")).unwrap(),
            TagMatch::All
        );
        assert_eq!(
            serde_json::from_value::<TagMatch>(serde_json::json!("any")).unwrap(),
            TagMatch::Any
        );
        assert_eq!(
            serde_json::from_value::<SearchOrder>(serde_json::json!("Recency")).unwrap(),
            SearchOrder::Recency
        );
        assert_eq!(
            serde_json::from_value::<SearchOrder>(serde_json::json!("RELEVANCE")).unwrap(),
            SearchOrder::Relevance
        );
    }
}
