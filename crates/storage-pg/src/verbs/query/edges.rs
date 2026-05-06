use proxima_core::StorageError;
use proxima_core::verbs::query::{EdgeRow, QueryRequest};
use sqlx::PgPool;

use super::rows::{EdgeRowDb, edge_row_from_db};

/// Hard upper bound on edges returned by snapshot-edge mode.
/// Decoupled from `QueryRequest::limit`, which sizes the node window.
pub const MAX_SNAPSHOT_EDGES: usize = 50_000;

pub(super) async fn query_edges(
    pool: &PgPool,
    req: &QueryRequest,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
    visible_memory_ids: &[uuid::Uuid],
    visible_goal_ids: &[uuid::Uuid],
) -> Result<Vec<EdgeRow>, StorageError> {
    let edge_ids = req.edge_ids.clone();
    let id_hydration =
        !req.memory_ids.is_empty() || !req.goal_ids.is_empty() || !req.edge_ids.is_empty();

    if !edge_ids.is_empty() {
        return query_edges_by_id(pool, req, &edge_ids, owner_kind, owner_principal_id).await;
    }
    // Focused identity and entity-kind queries should not return graph
    // closure as a side effect. Atlas uses entity_kind = None to opt in.
    if id_hydration || req.entity_kind.is_some() {
        return Ok(Vec::new());
    }
    if visible_memory_ids.is_empty() && visible_goal_ids.is_empty() {
        return Ok(Vec::new());
    }
    query_edges_between_visible_nodes(
        pool,
        owner_kind,
        owner_principal_id,
        visible_memory_ids,
        visible_goal_ids,
    )
    .await
}

async fn query_edges_by_id(
    pool: &PgPool,
    req: &QueryRequest,
    edge_ids: &[uuid::Uuid],
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
) -> Result<Vec<EdgeRow>, StorageError> {
    let rows = sqlx::query_as::<_, EdgeRowDb>(
        "SELECT edge_id, relation, relation_class, source_memory_id, source_goal_id, \
                target_memory_id, target_goal_id, owner_principal_kind, \
                owner_principal_id, owner_org_id \
         FROM proxima_core.edges \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND edge_id = ANY($3::uuid[]) \
         ORDER BY created_at DESC \
         LIMIT $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(edge_ids)
    .bind(i64::from(req.limit))
    .fetch_all(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;
    rows.into_iter().map(edge_row_from_db).collect()
}

async fn query_edges_between_visible_nodes(
    pool: &PgPool,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
    visible_memory_ids: &[uuid::Uuid],
    visible_goal_ids: &[uuid::Uuid],
) -> Result<Vec<EdgeRow>, StorageError> {
    let rows = sqlx::query_as::<_, EdgeRowDb>(
        "SELECT edge_id, relation, relation_class, source_memory_id, source_goal_id, \
                target_memory_id, target_goal_id, owner_principal_kind, \
                owner_principal_id, owner_org_id \
         FROM proxima_core.edges \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND ( \
             (source_memory_id = ANY($3::uuid[]) OR source_goal_id = ANY($4::uuid[])) \
             AND \
             (target_memory_id = ANY($3::uuid[]) OR target_goal_id = ANY($4::uuid[])) \
           ) \
         ORDER BY created_at DESC \
         LIMIT $5",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(visible_memory_ids)
    .bind(visible_goal_ids)
    .bind(i64::try_from(MAX_SNAPSHOT_EDGES).expect("MAX_SNAPSHOT_EDGES fits in i64"))
    .fetch_all(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;
    rows.into_iter().map(edge_row_from_db).collect()
}
