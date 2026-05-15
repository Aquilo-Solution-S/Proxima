//! Query verb — owner-scoped snapshot read of memories.
//!
//! See docs/14-protocol-surface.md §"Query" and
//! docs/02-memory.md.

use uuid::Uuid;

use crate::outbox::EntityRef;
use crate::verbs::goal_write::GoalState;
use crate::verbs::schema::SchemaTombstone;
use crate::{GoalId, MemoryId, Owner, SchemaId, SchemaVersion};

/// Re-export the canonical `EntityKind` from `outbox` so query
/// callers don't need a second import path. The duplicate
/// definition that lived here pre-M6.5 produced two identical
/// types; specta's type-name uniqueness check caught it.
pub use crate::outbox::EntityKind;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    specta::Type,
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

/// Owner-scoped memory search. Semantic modes require the engine/tool
/// layer to populate the query embedding and active embedding-space
/// metadata before dispatching to storage.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct MemorySearchRequest {
    pub owner: Owner,
    pub query: String,
    #[serde(default = "default_search_mode")]
    pub mode: SearchMode,
    pub limit: u32,
    pub kind: Option<EntityKind>,
    pub schema_id: Option<SchemaId>,
    #[serde(skip)]
    pub query_embedding: Option<Vec<f32>>,
    #[serde(skip)]
    pub embedding_model_id: Option<String>,
    #[serde(skip)]
    pub embedding_dim: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct MemorySearchResult {
    pub memory_id: MemoryId,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub snippet: String,
    pub score: f32,
    pub lexical_score: f32,
    pub similarity_score: f32,
    pub wake_chain_depth: crate::WakeChainDepth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum SupersessionStatus {
    /// Heads only — exclude rows that are superseded.
    HeadsOnly,
    /// Include superseded rows.
    IncludeSuperseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum TombstoneFilter {
    PresentOnly,
    IncludeTombstoned,
}

fn default_tombstone_filter() -> TombstoneFilter {
    TombstoneFilter::PresentOnly
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum PersonalityRootFilter {
    /// Include only active root/self Perspective rows. Non-root
    /// Perspectives are unaffected.
    ActiveOnly,
    /// Include inactive, tombstoned, and orphan root/self Perspective
    /// rows when they otherwise match the query.
    IncludeInactive,
}

fn default_personality_root_filter() -> PersonalityRootFilter {
    PersonalityRootFilter::IncludeInactive
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

/// One core-generic Query request. Flavor-typed filters
/// per docs/14 §"Query" land when the first flavor crate
/// registers a sidecar (M3+).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct QueryRequest {
    pub owner: Owner,
    pub entity_kind: Option<EntityKind>,
    pub schema_id: Option<SchemaId>,
    pub supersession: SupersessionStatus,
    #[serde(default = "default_tombstone_filter")]
    pub tombstones: TombstoneFilter,
    #[serde(default = "default_personality_root_filter")]
    pub personality_roots: PersonalityRootFilter,
    pub limit: u32,
    /// Include typed payload bytes in returned rows. Broad graph snapshots can
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
    /// filter. Pagination cursor lands when M2 introduces real
    /// data.
    pub fn for_owner(owner: Owner) -> Self {
        Self {
            owner,
            entity_kind: None,
            schema_id: None,
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: TombstoneFilter::PresentOnly,
            personality_roots: PersonalityRootFilter::IncludeInactive,
            limit: 100,
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct MemoryRow {
    pub id: MemoryId,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub owner: Owner,
    /// CBOR projection of the sidecar row, populated by storage at read
    /// time. Empty when the schema has no sidecar or when an
    /// identity-only query mode is added.
    /// Wire-only field — never persisted (docs/07).
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct GoalRow {
    pub id: GoalId,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub owner: Owner,
    pub title: String,
    pub text: String,
    pub state: GoalState,
    pub parent_goal_ids: Vec<GoalId>,
    pub supersedes: Option<GoalId>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct EdgeRow {
    pub id: uuid::Uuid,
    pub relation: String,
    pub relation_class: String,
    pub source: EntityRef,
    pub target: EntityRef,
    pub owner: Owner,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct QueryResponse {
    pub memories: Vec<MemoryRow>,
    pub goals: Vec<GoalRow>,
    pub edges: Vec<EdgeRow>,
    /// docs/14 §"Cursor & resume" — None when the store has
    /// not yet recorded any change events.
    pub seq_high_water: Option<Uuid>,
}

/// In-memory store. Empty for M1; storage adapters land in M2
/// per ROADMAP.
#[derive(Debug, Default)]
pub struct MemoryStore {
    memories: Vec<MemoryRow>,
    seq_high_water: Option<Uuid>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn query(&self, req: &QueryRequest) -> QueryResponse {
        let memories: Vec<MemoryRow> = self
            .memories
            .iter()
            .filter(|m| m.owner.principal == req.owner.principal)
            .filter(|m| req.entity_kind.is_none_or(|k| m.kind == k))
            .filter(|m| req.schema_id.as_ref().is_none_or(|s| &m.schema_id == s))
            .take(req.limit as usize)
            .map(|m| MemoryRow {
                id: m.id,
                kind: m.kind,
                schema_id: m.schema_id.clone(),
                schema_version: m.schema_version,
                owner: m.owner.clone(),
                payload: Vec::new(),
            })
            .collect();
        // SupersessionStatus is unused for M1 (no superseded
        // rows exist yet); honoured properly when storage lands.
        let _ = req.supersession;
        QueryResponse {
            memories,
            goals: Vec::new(),
            edges: Vec::new(),
            seq_high_water: self.seq_high_water,
        }
    }
}
