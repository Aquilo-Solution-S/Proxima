use proxima_core::StorageError;
use proxima_core::verbs::query::{EdgeRow, QueryRequest};
use sqlx::PgPool;

use super::rows::{EdgeRowDb, edge_row_from_db};

pub(super) async fn query_edges(
    pool: &PgPool,
    req: &QueryRequest,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
) -> Result<Vec<EdgeRow>, StorageError> {
    let edge_ids = req.edge_ids.clone();
    let id_hydration =
        !req.memory_ids.is_empty() || !req.goal_ids.is_empty() || !req.edge_ids.is_empty();
    if id_hydration && edge_ids.is_empty() {
        return Ok(Vec::new());
    }
    if edge_ids.is_empty() && req.entity_kind.is_some() {
        return Ok(Vec::new());
    }
    let mut sql = String::from(
        "SELECT edge_id, relation, relation_class, source_memory_id, source_goal_id, \
                target_memory_id, target_goal_id, owner_principal_kind, \
                owner_principal_id, owner_org_id \
         FROM proxima_core.edges \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2",
    );
    if !edge_ids.is_empty() {
        sql.push_str(" AND edge_id = ANY($3)");
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT ");
    sql.push_str(&u64::from(req.limit).to_string());

    let mut q = sqlx::query_as::<_, EdgeRowDb>(&sql)
        .bind(owner_kind)
        .bind(owner_principal_id);
    if !edge_ids.is_empty() {
        q = q.bind(edge_ids);
    }
    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
    rows.into_iter().map(edge_row_from_db).collect()
}
