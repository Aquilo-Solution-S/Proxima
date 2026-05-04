//! Query verb — owner-scoped snapshot read of memories.
//!
//! See docs/14-protocol-surface.md §"Query" and
//! docs/02-memory.md.

use uuid::Uuid;

use crate::{MemoryId, Owner, SchemaId, SchemaVersion};

/// Per docs/14 §"Subscribe" `ChangeKind::EntityAppend.entity_kind`
/// and docs/02 §"Edges". Goal is included here as an entity-kind
/// tag (Goal is a distinct entity per AGENTS.md invariant 11);
/// goal payload retrieval is its own verb (`GoalWrite` for the
/// write side, future `Query(entity_kind=Goal, …)` for reads).
/// For M1 the store has no goals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EntityKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SupersessionStatus {
    /// Heads only — exclude rows that are superseded.
    HeadsOnly,
    /// Include superseded rows.
    IncludeSuperseded,
}

/// One core-generic Query request. Flavor-typed filters
/// per docs/14 §"Query" land when the first flavor crate
/// registers a sidecar (M3+).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueryRequest {
    pub owner: Owner,
    pub entity_kind: Option<EntityKind>,
    pub schema_id: Option<SchemaId>,
    pub supersession: SupersessionStatus,
    pub limit: u32,
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
            limit: 100,
        }
    }
}

/// Snapshot of a memory row. Goal rows have their own shape
/// (M2+); not modelled here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemoryRow {
    pub id: MemoryId,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub owner: Owner,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueryResponse {
    pub memories: Vec<MemoryRow>,
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
            .filter(|m| m.owner == req.owner)
            .filter(|m| req.entity_kind.is_none_or(|k| m.kind == k))
            .filter(|m| req.schema_id.as_ref().is_none_or(|s| &m.schema_id == s))
            .take(req.limit as usize)
            .cloned()
            .collect();
        // SupersessionStatus is unused for M1 (no superseded
        // rows exist yet); honoured properly when storage lands.
        let _ = req.supersession;
        QueryResponse {
            memories,
            seq_high_water: self.seq_high_water,
        }
    }
}
