use proxima_core::storage_ports::InboundPinQuery;
use proxima_core::verbs::query::QueryRequest;
use proxima_core::{
    Edge, EdgeKind, EntityKind, GoalId, MemoryId, OwnerRef, PinNode, StorageError,
    project_window_edges,
};
use sqlx::PgPool;

use crate::error::map_err;

/// Hard upper bound on edges returned by snapshot-edge mode.
/// Decoupled from `QueryRequest::limit`, which sizes the node window.
pub const MAX_SNAPSHOT_EDGES: usize = 50_000;

#[derive(Debug, sqlx::FromRow)]
struct PinNodeRow {
    t: uuid::Uuid,
    kind: String,
    schema_id: String,
    origins: Vec<uuid::Uuid>,
    refs: Vec<uuid::Uuid>,
}

impl PinNodeRow {
    fn into_pin_node(self) -> Option<PinNode> {
        Some(PinNode {
            id: MemoryId::new(self.t),
            kind: parse_kind(&self.kind)?,
            schema_id: proxima_core::SchemaId::new(self.schema_id),
            origins: self.origins.into_iter().map(MemoryId::new).collect(),
            refs: self.refs.into_iter().map(MemoryId::new).collect(),
            goal_refs: Vec::new(),
        })
    }
}

/// Resolve readable Goal ids in a batch. Unreadable or unknown ids are left
/// in the raw reference carrier so the engine can redact them.
pub(crate) async fn load_visible_goal_ids(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    goal_ids: &[GoalId],
) -> Result<Vec<GoalId>, StorageError> {
    if goal_ids.is_empty() || read_owners.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<uuid::Uuid> = goal_ids.iter().map(|id| id.into_inner()).collect();
    let owner_ids = owner_ids(read_owners);
    let rows: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT t
           FROM proxima_core.goal
          WHERE t = ANY($1::uuid[])
            AND owner_id = ANY($2::uuid[])",
    )
    .bind(&ids)
    .bind(&owner_ids)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(GoalId::new).collect())
}

/// Owner-scoped PK load of pin carriers.
pub(crate) async fn load_pin_nodes(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    memory_ids: &[MemoryId],
) -> Result<Vec<PinNode>, StorageError> {
    if memory_ids.is_empty() || read_owners.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<uuid::Uuid> = memory_ids.iter().map(|id| id.into_inner()).collect();
    let owner_ids = owner_ids(read_owners);
    let rows: Vec<PinNodeRow> = sqlx::query_as(
        "SELECT t, kind::text, schema_id, origins, refs
           FROM proxima_core.memory
          WHERE t = ANY($1::uuid[])
            AND owner_id = ANY($2::uuid[])",
    )
    .bind(&ids)
    .bind(&owner_ids)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .filter_map(PinNodeRow::into_pin_node)
        .collect())
}

/// Owner-scoped GIN page of rows that list any of `query.targets`.
pub(crate) async fn load_inbound_pin_nodes(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    query: InboundPinQuery<'_>,
) -> Result<Vec<PinNode>, StorageError> {
    if query.targets.is_empty() || read_owners.is_empty() {
        return Ok(Vec::new());
    }
    if query.limit == 0 {
        return Err(StorageError::ConstraintViolation(
            "inbound pin page limit must be at least 1".into(),
        ));
    }
    let ids: Vec<uuid::Uuid> = query.targets.iter().map(|id| id.into_inner()).collect();
    let owner_ids = owner_ids(read_owners);
    let after = query.after.map(MemoryId::into_inner);
    let limit = i64::from(query.limit);
    let sql = inbound_pin_sql(query.heads_only, query.kind);
    // SQL-POLICY: fixed-fragment — from/kind arms are compile-time literals.
    let rows: Vec<PinNodeRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(&ids)
        .bind(&owner_ids)
        .bind(after)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .filter_map(PinNodeRow::into_pin_node)
        .collect())
}

#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn inbound_pin_sql_for_tests(heads_only: bool, kind: Option<EdgeKind>) -> &'static str {
    inbound_pin_sql(heads_only, kind)
}

fn inbound_pin_sql(heads_only: bool, kind: Option<EdgeKind>) -> &'static str {
    if heads_only {
        match kind {
            Some(EdgeKind::Origin) => {
                "SELECT m.t, m.kind::text, m.schema_id, m.origins, m.refs
                   FROM proxima_core.memory_head h
                   JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
                  WHERE h.owner_id = ANY($2::uuid[])
                    AND m.origins && $1::uuid[]
                    AND ($3::uuid IS NULL OR m.t < $3)
                  ORDER BY m.t DESC
                  LIMIT $4"
            }
            Some(EdgeKind::Reference) => {
                "SELECT m.t, m.kind::text, m.schema_id, m.origins, m.refs
                   FROM proxima_core.memory_head h
                   JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
                  WHERE h.owner_id = ANY($2::uuid[])
                    AND m.refs && $1::uuid[]
                    AND ($3::uuid IS NULL OR m.t < $3)
                  ORDER BY m.t DESC
                  LIMIT $4"
            }
            None => {
                "SELECT m.t, m.kind::text, m.schema_id, m.origins, m.refs
                   FROM proxima_core.memory_head h
                   JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
                  WHERE h.owner_id = ANY($2::uuid[])
                    AND (m.origins && $1::uuid[] OR m.refs && $1::uuid[])
                    AND ($3::uuid IS NULL OR m.t < $3)
                  ORDER BY m.t DESC
                  LIMIT $4"
            }
        }
    } else {
        match kind {
            Some(EdgeKind::Origin) => {
                "SELECT m.t, m.kind::text, m.schema_id, m.origins, m.refs
                   FROM proxima_core.memory m
                  WHERE m.owner_id = ANY($2::uuid[])
                    AND m.origins && $1::uuid[]
                    AND ($3::uuid IS NULL OR m.t < $3)
                  ORDER BY m.t DESC
                  LIMIT $4"
            }
            Some(EdgeKind::Reference) => {
                "SELECT m.t, m.kind::text, m.schema_id, m.origins, m.refs
                   FROM proxima_core.memory m
                  WHERE m.owner_id = ANY($2::uuid[])
                    AND m.refs && $1::uuid[]
                    AND ($3::uuid IS NULL OR m.t < $3)
                  ORDER BY m.t DESC
                  LIMIT $4"
            }
            None => {
                "SELECT m.t, m.kind::text, m.schema_id, m.origins, m.refs
                   FROM proxima_core.memory m
                  WHERE m.owner_id = ANY($2::uuid[])
                    AND (m.origins && $1::uuid[] OR m.refs && $1::uuid[])
                    AND ($3::uuid IS NULL OR m.t < $3)
                  ORDER BY m.t DESC
                  LIMIT $4"
            }
        }
    }
}

/// Snapshot-mode edges: pins whose two endpoints are both in the Query window.
pub(super) fn query_edges(
    _req: &QueryRequest,
    memories: &[proxima_core::verbs::query::MemoryRow],
    visible_goal_ids: &[uuid::Uuid],
) -> Vec<Edge> {
    let visible_goal_ids: Vec<GoalId> = visible_goal_ids.iter().copied().map(GoalId::new).collect();
    let mut nodes: Vec<PinNode> = memories.iter().map(PinNode::from).collect();
    for node in &mut nodes {
        node.resolve_visible_goal_refs(&visible_goal_ids);
    }
    project_window_edges(&nodes, MAX_SNAPSHOT_EDGES)
}

fn owner_ids(read_owners: &[OwnerRef]) -> Vec<uuid::Uuid> {
    read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect()
}

fn parse_kind(kind: &str) -> Option<EntityKind> {
    match kind {
        "fact" => Some(EntityKind::Fact),
        "abstraction" => Some(EntityKind::Abstraction),
        "perspective" => Some(EntityKind::Perspective),
        _ => None,
    }
}
