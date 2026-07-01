use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use futures_util::future::try_join_all;
use proxima_core::verbs::query::{
    EntityKind, QueryCursor, QueryRequest, QueryResponse, SupersessionStatus, TombstoneFilter,
};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{MemoryId, SchemaId, SchemaVersion, SidecarPayload, StorageError};
use sqlx::PgPool;

use crate::error::internal;
use crate::sidecars::{PgSidecarKey, PgSidecarReadCtx, PgSidecarRegistryFrozen};

use super::edges::query_edges;
use super::goals::query_goals;
use super::read_owner_columns;
use super::rows::{MemoryRowDb, memory_row_from_db, read_seq_high_water, validate_stateful_filter};

#[derive(Debug, Clone)]
struct StatefulSqlParams {
    schema: usize,
    version: usize,
    tombstone: Option<usize>,
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn query_memories(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &QueryRequest,
    schemas: &[SchemaInfo],
) -> Result<QueryResponse, StorageError> {
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(&req.read_owners);
    let id_hydration =
        !req.memory_ids.is_empty() || !req.goal_ids.is_empty() || !req.edge_ids.is_empty();
    let schema_id_filter = req.schema_id.as_ref().map(|s| s.as_str().to_string());
    if matches!(req.entity_kind, Some(EntityKind::Goal)) {
        let (goals, next_cursor) = query_goals(
            pool,
            req,
            &read_owner_kinds,
            &read_owner_ids,
            schema_id_filter.as_deref(),
        )
        .await?;
        return Ok(QueryResponse {
            memories: Vec::new(),
            goals,
            edges: Vec::new(),
            next_cursor,
            seq_high_water: read_seq_high_water(pool, &read_owner_kinds, &read_owner_ids).await?,
        });
    }

    let stateful = validated_stateful_filters(req)?;
    let cursor = match &req.page.after {
        Some(QueryCursor::Memory {
            created_at,
            memory_id,
        }) => Some((*created_at, memory_id.into_inner())),
        _ => None,
    };
    let single_memory_stream = matches!(
        req.entity_kind,
        Some(EntityKind::Fact | EntityKind::Abstraction | EntityKind::Perspective)
    );
    let fetch_limit = if single_memory_stream {
        u64::from(req.limit) + 1
    } else {
        u64::from(req.limit)
    };

    let mut sql = String::from(
        "SELECT page.memory_id, page.created_at, page.owner_kind, page.owner_id, \
                page.schema_id, page.schema_version, page.kind \
         FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id) \
         JOIN LATERAL ( \
             SELECT m.memory_id, m.created_at, m.owner_kind, m.owner_id, \
                    m.schema_id, m.schema_version, m.kind \
               FROM proxima_core.memories m",
    );

    // Bindings: $1=read_owner_kinds, $2=read_owner_ids.
    let mut next_param = 3;
    let schema = schema_id_filter.as_ref().map(|_| {
        let param = next_param;
        next_param += 1;
        param
    });
    let memory_ids: Vec<uuid::Uuid> = req.memory_ids.iter().map(|id| id.into_inner()).collect();
    let memory_ids_param = (!memory_ids.is_empty()).then(|| {
        let param = next_param;
        next_param += 1;
        param
    });
    let stateful_params = allocate_stateful_params(&stateful, &mut next_param);
    let cursor_params = cursor.map(|_| {
        let created_at = next_param;
        next_param += 1;
        let memory_id = next_param;
        (created_at, memory_id)
    });
    // The stateful JOINs (for head-by-natural-key filtering) use
    // explicit `ON sf_i.memory_id = m.memory_id` so generated aliases
    // stay unambiguous.
    for (idx, sf) in stateful.iter().enumerate() {
        let alias = stateful_alias(idx);
        write!(
            sql,
            " LEFT JOIN {sidecar} {alias} ON {alias}.memory_id = m.memory_id",
            sidecar = sf.sidecar_table,
            alias = alias,
        )
        .expect("write to String is infallible");
    }

    sql.push_str(
        // Cognitive rows cannot be world-owned (`*_world_not_write_owner_chk`),
        // so non-null owner equality preserves the keyset indexes.
        " WHERE m.owner_kind = s.kind \
            AND m.owner_id = s.id \
            AND m.tombstoned_at IS NULL",
    );

    if let Some(param) = memory_ids_param {
        write!(sql, " AND m.memory_id = ANY(${param})").expect("write to String is infallible");
    } else if id_hydration {
        sql.push_str(" AND false");
    }

    push_heads_predicate(&mut sql, req, schema, &stateful, &stateful_params);

    if let Some((created_at_param, memory_id_param)) = cursor_params {
        write!(
            sql,
            " AND (m.created_at, m.memory_id) < (${created_at_param}, ${memory_id_param})"
        )
        .expect("write to String is infallible");
    }

    sql.push_str(" ORDER BY m.created_at DESC, m.memory_id DESC LIMIT ");
    sql.push_str(&fetch_limit.to_string());
    sql.push_str(") page ON TRUE ORDER BY page.created_at DESC, page.memory_id DESC LIMIT ");
    sql.push_str(&fetch_limit.to_string());

    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query_as::<_, MemoryRowDb>(&sql)
        .bind(&read_owner_kinds)
        .bind(&read_owner_ids);
    if let Some(sid) = &schema_id_filter {
        q = q.bind(sid.clone());
    }
    if !memory_ids.is_empty() {
        q = q.bind(memory_ids);
    }
    q = bind_stateful_filters(q, &stateful);
    if let Some((created_at, memory_id)) = cursor {
        q = q.bind(created_at).bind(memory_id);
    }

    let mut rows: Vec<MemoryRowDb> = q.fetch_all(pool).await.map_err(internal)?;
    let limit = usize::try_from(req.limit)
        .map_err(|_| StorageError::Internal("query limit does not fit usize".into()))?;
    let next_memory_cursor = if single_memory_stream && rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|row| QueryCursor::Memory {
            created_at: row.created_at,
            memory_id: MemoryId::new(row.memory_id),
        })
    } else {
        None
    };

    let mut payloads = if req.include_payloads {
        load_row_payloads_batch(pool, sidecars, &rows).await?
    } else {
        HashMap::new()
    };
    let mut memories = Vec::with_capacity(rows.len());
    for row in rows {
        let payload = payloads.remove(&MemoryId::new(row.memory_id));
        memories.push(memory_row_from_db(row, payload)?);
    }

    let (goals, next_goal_cursor) =
        if req.entity_kind.is_none() || matches!(req.entity_kind, Some(EntityKind::Goal)) {
            query_goals(
                pool,
                req,
                &read_owner_kinds,
                &read_owner_ids,
                schema_id_filter.as_deref(),
            )
            .await?
        } else {
            (Vec::new(), None)
        };
    let visible_memory_ids: Vec<uuid::Uuid> =
        memories.iter().map(|row| row.id.into_inner()).collect();
    let visible_goal_ids: Vec<uuid::Uuid> = goals.iter().map(|row| row.id.into_inner()).collect();
    let edges = query_edges(
        pool,
        req,
        &read_owner_kinds,
        &read_owner_ids,
        &visible_memory_ids,
        &visible_goal_ids,
        schemas,
    )
    .await?;
    let seq_high_water = read_seq_high_water(pool, &read_owner_kinds, &read_owner_ids).await?;

    Ok(QueryResponse {
        memories,
        goals,
        edges,
        next_cursor: next_memory_cursor.or(next_goal_cursor),
        seq_high_water,
    })
}

async fn load_row_payloads_batch(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    rows: &[MemoryRowDb],
) -> Result<HashMap<MemoryId, SidecarPayload>, StorageError> {
    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for row in rows {
        let schema_version = u32::try_from(row.schema_version).map_err(|_| {
            StorageError::Internal(format!(
                "invalid memory schema_version {} for memory {}",
                row.schema_version, row.memory_id
            ))
        })?;
        let kind = match row.kind.unwrap_or(EntityKind::Fact) {
            EntityKind::Fact => PayloadKind::Fact,
            EntityKind::Abstraction => PayloadKind::Abstraction,
            EntityKind::Perspective => PayloadKind::Perspective,
            EntityKind::Goal => continue,
        };
        let key = PgSidecarKey::new(
            kind,
            SchemaId::new(row.schema_id.clone()),
            SchemaVersion::new(schema_version),
        );
        if sidecars.contains(&key) {
            ids_by_key
                .entry(key)
                .or_default()
                .push(MemoryId::new(row.memory_id));
        }
    }
    let batches = ids_by_key.into_iter().map(|(key, ids)| async move {
        sidecars
            .load_memory_payloads_batch(PgSidecarReadCtx::from(pool), &key, &ids)
            .await
    });
    let rows = try_join_all(batches).await?;
    Ok(rows.into_iter().flatten().collect())
}

pub(super) async fn visible_ids_for(
    pool: &PgPool,
    req: &QueryRequest,
    read_owner_kinds: &[proxima_core::OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    candidate_memory_ids: &[uuid::Uuid],
    candidate_goal_ids: &[uuid::Uuid],
    schemas: &[SchemaInfo],
) -> Result<(HashSet<uuid::Uuid>, HashSet<uuid::Uuid>), StorageError> {
    let memory_ids = query_visible_memory_ids(
        pool,
        req,
        read_owner_kinds,
        read_owner_ids,
        candidate_memory_ids,
        schemas,
    )
    .await?;
    let goal_ids = query_visible_goal_ids(
        pool,
        req,
        read_owner_kinds,
        read_owner_ids,
        candidate_goal_ids,
    )
    .await?;
    Ok((memory_ids, goal_ids))
}

async fn query_visible_memory_ids(
    pool: &PgPool,
    req: &QueryRequest,
    read_owner_kinds: &[proxima_core::OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    candidate_memory_ids: &[uuid::Uuid],
    _schemas: &[SchemaInfo],
) -> Result<HashSet<uuid::Uuid>, StorageError> {
    if candidate_memory_ids.is_empty() || matches!(req.entity_kind, Some(EntityKind::Goal)) {
        return Ok(HashSet::new());
    }
    let stateful = validated_stateful_filters(req)?;
    let schema_id_filter = req.schema_id.as_ref().map(|s| s.as_str().to_string());

    let mut sql = String::from(
        "SELECT m.memory_id \
           FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id) \
           JOIN proxima_core.memories m \
             ON m.owner_kind = s.kind \
            AND m.owner_id IS NOT DISTINCT FROM s.id \
        ",
    );
    for (idx, sf) in stateful.iter().enumerate() {
        let alias = stateful_alias(idx);
        write!(
            sql,
            " LEFT JOIN {sidecar} {alias} ON {alias}.memory_id = m.memory_id",
            sidecar = sf.sidecar_table,
            alias = alias,
        )
        .expect("write to String is infallible");
    }

    sql.push_str(" WHERE m.tombstoned_at IS NULL");
    sql.push_str(" AND m.memory_id = ANY($3::uuid[])");
    let mut next_param = 4;
    let schema = schema_id_filter.as_ref().map(|_| {
        let param = next_param;
        next_param += 1;
        param
    });
    let stateful_params = allocate_stateful_params(&stateful, &mut next_param);

    push_heads_predicate(&mut sql, req, schema, &stateful, &stateful_params);

    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query_as::<_, (uuid::Uuid,)>(&sql)
        .bind(read_owner_kinds)
        .bind(read_owner_ids)
        .bind(candidate_memory_ids);
    if let Some(sid) = &schema_id_filter {
        q = q.bind(sid.clone());
    }
    q = bind_stateful_filters(q, &stateful);
    let rows = q.fetch_all(pool).await.map_err(internal)?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

async fn query_visible_goal_ids(
    pool: &PgPool,
    req: &QueryRequest,
    read_owner_kinds: &[proxima_core::OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    candidate_goal_ids: &[uuid::Uuid],
) -> Result<HashSet<uuid::Uuid>, StorageError> {
    if candidate_goal_ids.is_empty()
        || matches!(
            req.entity_kind,
            Some(EntityKind::Fact | EntityKind::Abstraction | EntityKind::Perspective)
        )
    {
        return Ok(HashSet::new());
    }
    let mut sql = String::from(
        "SELECT g.goal_id \
           FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id) \
           JOIN proxima_core.goals g \
             ON g.owner_kind = s.kind \
            AND g.owner_id IS NOT DISTINCT FROM s.id \
          WHERE g.goal_id = ANY($3::uuid[])",
    );
    let schema_id_filter = req.schema_id.as_ref().map(|s| s.as_str().to_string());
    if schema_id_filter.is_some() {
        // SQL-POLICY: fixed-fragment
        sql.push_str(" AND g.schema_id = $4");
    }
    if matches!(req.supersession, SupersessionStatus::HeadsOnly) {
        // SQL-POLICY: fixed-fragment
        sql.push_str(
            " AND NOT EXISTS (SELECT 1 FROM proxima_core.goals g2 \
                              WHERE g2.supersedes = g.goal_id)",
        );
    }
    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query_as::<_, (uuid::Uuid,)>(&sql)
        .bind(read_owner_kinds)
        .bind(read_owner_ids)
        .bind(candidate_goal_ids);
    if let Some(sid) = schema_id_filter {
        q = q.bind(sid);
    }
    let rows = q.fetch_all(pool).await.map_err(internal)?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

fn validated_stateful_filters(
    req: &QueryRequest,
) -> Result<Vec<&proxima_core::verbs::query::StatefulHeadsFilter>, StorageError> {
    req.stateful_heads
        .iter()
        .map(validate_stateful_filter)
        .collect()
}

fn allocate_stateful_params(
    stateful: &[&proxima_core::verbs::query::StatefulHeadsFilter],
    next_param: &mut usize,
) -> Vec<StatefulSqlParams> {
    stateful
        .iter()
        .map(|sf| {
            let schema = *next_param;
            *next_param += 1;
            let version = *next_param;
            *next_param += 1;
            let tombstone = sf.tombstone.as_ref().map(|_| {
                let param = *next_param;
                *next_param += 1;
                param
            });
            StatefulSqlParams {
                schema,
                version,
                tombstone,
            }
        })
        .collect()
}

fn bind_stateful_filters<'q, O>(
    mut q: sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>,
    stateful: &[&proxima_core::verbs::query::StatefulHeadsFilter],
) -> sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments> {
    for sf in stateful {
        q = q.bind(sf.schema_id.as_str().to_string());
        q = q.bind(sf.schema_version.into_inner().cast_signed());
        if let Some(tombstone) = &sf.tombstone {
            q = q.bind(tombstone.value.clone());
        }
    }
    q
}

fn push_heads_predicate(
    sql: &mut String,
    req: &QueryRequest,
    schema: Option<usize>,
    stateful: &[&proxima_core::verbs::query::StatefulHeadsFilter],
    stateful_params: &[StatefulSqlParams],
) {
    match req.entity_kind {
        None => {}
        Some(EntityKind::Fact) => {
            sql.push_str(" AND m.kind IS NULL");
        }
        Some(EntityKind::Abstraction) => sql.push_str(" AND m.kind = 'Abstraction'"),
        Some(EntityKind::Perspective) => sql.push_str(" AND m.kind = 'Perspective'"),
        // Goals are an entity, not a Memory kind (inv 11); they are queried
        // via the goal path and never reach the memories head predicate.
        Some(EntityKind::Goal) => {
            unreachable!(
                "Goal is not a Memory kind; query_memories never receives EntityKind::Goal"
            )
        }
    }
    if let Some(param) = schema {
        write!(sql, " AND m.schema_id = ${param}").expect("write to String is infallible");
    }
    if matches!(req.supersession, SupersessionStatus::HeadsOnly) {
        if stateful.is_empty() {
            // SQL-POLICY: fixed-fragment
            sql.push_str(
                " AND NOT EXISTS (SELECT 1 FROM proxima_core.memories m2 \
                                  WHERE m2.supersedes = m.memory_id \
                                    AND m2.tombstoned_at IS NULL",
            );
            super::push_same_home_owner_successor_predicate(sql, "m2", "m");
            sql.push(')');
        } else {
            sql.push_str(" AND (");
            for (idx, sf) in stateful.iter().enumerate() {
                if idx > 0 {
                    sql.push_str(" OR ");
                }
                push_stateful_head_branch(sql, idx, sf, &stateful_params[idx]);
            }
            sql.push_str(" OR (");
            push_not_stateful_match(sql, stateful_params);
            // SQL-POLICY: fixed-fragment
            sql.push_str(
                " AND NOT EXISTS (SELECT 1 FROM proxima_core.memories m2 \
                                  WHERE m2.supersedes = m.memory_id \
                                    AND m2.tombstoned_at IS NULL",
            );
            super::push_same_home_owner_successor_predicate(sql, "m2", "m");
            sql.push(')');
            sql.push(')');
            sql.push(')');
        }
    }
    push_tombstone_exclusion(sql, req, stateful, stateful_params);
}

fn stateful_alias(idx: usize) -> String {
    format!("sf_{idx}")
}

fn stateful_newer_alias(idx: usize) -> String {
    format!("sf2_{idx}")
}

fn push_stateful_head_branch(
    sql: &mut String,
    idx: usize,
    sf: &proxima_core::verbs::query::StatefulHeadsFilter,
    params: &StatefulSqlParams,
) {
    let alias = stateful_alias(idx);
    let newer_alias = stateful_newer_alias(idx);
    let nk_pairs = sf
        .natural_key_columns
        .iter()
        .map(|c| format!("{newer_alias}.{c} = {alias}.{c}"))
        .collect::<Vec<_>>()
        .join(" AND ");
    write!(
        sql,
        "(m.schema_id = ${schema} \
          AND m.schema_version = ${version} \
          AND NOT EXISTS ( \
            SELECT 1 FROM proxima_core.memories m2 \
            JOIN {sidecar} {newer_alias} USING (memory_id) \
            WHERE m2.schema_id = m.schema_id \
              AND m2.schema_version = m.schema_version \
              AND m2.owner_kind = m.owner_kind \
              AND m2.owner_id IS NOT DISTINCT FROM m.owner_id \
              AND m2.tombstoned_at IS NULL \
              AND {nk_pairs} \
              AND m2.created_at > m.created_at \
          ))",
        schema = params.schema,
        version = params.version,
        sidecar = sf.sidecar_table,
        newer_alias = newer_alias,
        nk_pairs = nk_pairs,
    )
    .expect("write to String is infallible");
}

fn push_not_stateful_match(sql: &mut String, params: &[StatefulSqlParams]) {
    if params.is_empty() {
        sql.push_str("TRUE");
        return;
    }
    sql.push_str("NOT (");
    for (idx, p) in params.iter().enumerate() {
        if idx > 0 {
            sql.push_str(" OR ");
        }
        write!(
            sql,
            "(m.schema_id = ${} AND m.schema_version = ${})",
            p.schema, p.version,
        )
        .expect("write to String is infallible");
    }
    sql.push(')');
}

fn push_tombstone_exclusion(
    sql: &mut String,
    req: &QueryRequest,
    stateful: &[&proxima_core::verbs::query::StatefulHeadsFilter],
    params: &[StatefulSqlParams],
) {
    if !matches!(req.tombstones, TombstoneFilter::PresentOnly) {
        return;
    }
    let tombstone_filters = stateful
        .iter()
        .zip(params.iter())
        .enumerate()
        .filter_map(|(idx, (sf, p))| sf.tombstone.as_ref().map(|t| (idx, sf, p, t)))
        .collect::<Vec<_>>();
    if tombstone_filters.is_empty() {
        return;
    }
    sql.push_str(" AND NOT (");
    for (branch_idx, (stateful_idx, _sf, p, tombstone)) in tombstone_filters.iter().enumerate() {
        if branch_idx > 0 {
            sql.push_str(" OR ");
        }
        let alias = stateful_alias(*stateful_idx);
        write!(
            sql,
            "(m.schema_id = ${schema} \
              AND m.schema_version = ${version} \
              AND {alias}.{column}::text = ${tombstone})",
            schema = p.schema,
            version = p.version,
            alias = alias,
            column = tombstone.column,
            tombstone = p
                .tombstone
                .expect("tombstone param exists when tombstone exists"),
        )
        .expect("write to String is infallible");
    }
    sql.push(')');
}
