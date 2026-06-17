use proxima_core::verbs::query::{EdgeRow, QueryRequest};
use proxima_core::verbs::schema::SchemaInfo;
use proxima_core::{OwnerPrincipalKind, StorageError};
use sqlx::PgPool;

use crate::error::internal;

use super::memories::visible_ids_for;
use super::resolve_head;
use super::rows::{EdgeRowDb, edge_row_from_db};

/// Hard upper bound on edges returned by snapshot-edge mode.
/// Decoupled from `QueryRequest::limit`, which sizes the node window.
pub const MAX_SNAPSHOT_EDGES: usize = 50_000;

pub(super) async fn query_edges(
    pool: &PgPool,
    req: &QueryRequest,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: uuid::Uuid,
    visible_memory_ids: &[uuid::Uuid],
    visible_goal_ids: &[uuid::Uuid],
    schemas: &[SchemaInfo],
) -> Result<Vec<EdgeRow>, StorageError> {
    let edge_ids = req.edge_ids.clone();
    let id_hydration =
        !req.memory_ids.is_empty() || !req.goal_ids.is_empty() || !req.edge_ids.is_empty();

    if !edge_ids.is_empty() {
        return query_edges_by_id(
            pool,
            req,
            &edge_ids,
            owner_kind,
            owner_principal_id,
            schemas,
        )
        .await;
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
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: uuid::Uuid,
    schemas: &[SchemaInfo],
) -> Result<Vec<EdgeRow>, StorageError> {
    let mut rows = sqlx::query_as::<_, EdgeRowDb>(
        "SELECT edge_id, relation, relation_class, \
                source_memory_id, source_goal_id, source_fact_entity_id, \
                target_memory_id, target_goal_id, target_fact_entity_id, \
                owner_principal_kind, \
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
    .map_err(internal)?;
    hydrate_fact_entity_heads(pool, &mut rows).await?;
    let endpoint_memory_ids = rows
        .iter()
        .flat_map(|row| [row.source_memory_id, row.target_memory_id])
        .flatten()
        .collect::<Vec<_>>();
    let endpoint_goal_ids = rows
        .iter()
        .flat_map(|row| [row.source_goal_id, row.target_goal_id])
        .flatten()
        .collect::<Vec<_>>();
    let (visible_memory_ids, visible_goal_ids) = visible_ids_for(
        pool,
        req,
        owner_kind,
        owner_principal_id,
        &endpoint_memory_ids,
        &endpoint_goal_ids,
        schemas,
    )
    .await?;
    rows.into_iter()
        .filter(|row| {
            endpoint_visible(
                row.source_memory_id,
                row.source_goal_id,
                &visible_memory_ids,
                &visible_goal_ids,
            ) && endpoint_visible(
                row.target_memory_id,
                row.target_goal_id,
                &visible_memory_ids,
                &visible_goal_ids,
            )
        })
        .map(edge_row_from_db)
        .collect()
}

fn endpoint_visible(
    memory_id: Option<uuid::Uuid>,
    goal_id: Option<uuid::Uuid>,
    visible_memory_ids: &std::collections::HashSet<uuid::Uuid>,
    visible_goal_ids: &std::collections::HashSet<uuid::Uuid>,
) -> bool {
    match (memory_id, goal_id) {
        (Some(id), None) => visible_memory_ids.contains(&id),
        (None, Some(id)) => visible_goal_ids.contains(&id),
        _ => false,
    }
}

async fn query_edges_between_visible_nodes(
    pool: &PgPool,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: uuid::Uuid,
    visible_memory_ids: &[uuid::Uuid],
    visible_goal_ids: &[uuid::Uuid],
) -> Result<Vec<EdgeRow>, StorageError> {
    let rows = sqlx::query_as::<_, EdgeRowDb>(
        "SELECT e.edge_id, e.relation, e.relation_class, \
                COALESCE(e.source_memory_id, sfe.current_memory_id) AS source_memory_id, \
                e.source_goal_id, e.source_fact_entity_id, \
                COALESCE(e.target_memory_id, tfe.current_memory_id) AS target_memory_id, \
                e.target_goal_id, e.target_fact_entity_id, \
                e.owner_principal_kind, \
                e.owner_principal_id, e.owner_org_id \
         FROM proxima_core.edges e \
         LEFT JOIN proxima_core.fact_entities sfe \
           ON sfe.fact_entity_id = e.source_fact_entity_id \
          AND sfe.owner_principal_kind = e.owner_principal_kind \
          AND sfe.owner_principal_id = e.owner_principal_id \
          AND sfe.owner_org_id = e.owner_org_id \
         LEFT JOIN proxima_core.fact_entities tfe \
           ON tfe.fact_entity_id = e.target_fact_entity_id \
          AND tfe.owner_principal_kind = e.owner_principal_kind \
          AND tfe.owner_principal_id = e.owner_principal_id \
          AND tfe.owner_org_id = e.owner_org_id \
         WHERE e.owner_principal_kind = $1 \
           AND e.owner_principal_id = $2 \
           AND ( \
             (e.source_memory_id = ANY($3::uuid[]) \
              OR e.source_goal_id = ANY($4::uuid[]) \
              OR sfe.current_memory_id = ANY($3::uuid[])) \
             AND \
             (e.target_memory_id = ANY($3::uuid[]) \
              OR e.target_goal_id = ANY($4::uuid[]) \
              OR tfe.current_memory_id = ANY($3::uuid[])) \
           ) \
         ORDER BY e.created_at DESC \
         LIMIT $5",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(visible_memory_ids)
    .bind(visible_goal_ids)
    .bind(i64::try_from(MAX_SNAPSHOT_EDGES).expect("MAX_SNAPSHOT_EDGES fits in i64"))
    .fetch_all(pool)
    .await
    .map_err(internal)?;
    rows.into_iter().map(edge_row_from_db).collect()
}

async fn hydrate_fact_entity_heads(
    pool: &PgPool,
    rows: &mut [EdgeRowDb],
) -> Result<(), StorageError> {
    let mut groups = std::collections::HashMap::<
        (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid),
        Vec<uuid::Uuid>,
    >::new();
    for row in rows.iter() {
        let key = (
            row.owner_principal_kind,
            row.owner_principal_id,
            row.owner_org_id,
        );
        if let Some(id) = row.source_fact_entity_id {
            groups.entry(key).or_default().push(id);
        }
        if let Some(id) = row.target_fact_entity_id {
            groups.entry(key).or_default().push(id);
        }
    }
    if groups.is_empty() {
        return Ok(());
    }

    let mut resolved = std::collections::HashMap::new();
    for ((owner_kind, owner_principal_id, owner_org_id), mut ids) in groups {
        ids.sort_unstable();
        ids.dedup();
        resolved
            .extend(resolve_head(pool, owner_kind, owner_principal_id, owner_org_id, &ids).await?);
    }

    for row in rows {
        if let Some(id) = row.source_fact_entity_id {
            let head = resolved.get(&id).copied().ok_or_else(|| {
                StorageError::Internal(format!("fact entity {id} has no owner-scoped head"))
            })?;
            row.source_memory_id = Some(head);
        }
        if let Some(id) = row.target_fact_entity_id {
            let head = resolved.get(&id).copied().ok_or_else(|| {
                StorageError::Internal(format!("fact entity {id} has no owner-scoped head"))
            })?;
            row.target_memory_id = Some(head);
        }
    }
    Ok(())
}
