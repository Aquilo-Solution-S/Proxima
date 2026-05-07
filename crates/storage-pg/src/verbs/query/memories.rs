use std::collections::HashSet;
use std::fmt::Write as _;

use proxima_core::verbs::query::{
    EntityKind, MemoryRow, QueryRequest, QueryResponse, SupersessionStatus, TombstoneFilter,
};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{Principal, StorageError};
use sqlx::PgPool;

use crate::pg_ident::PgIdent;

use super::edges::query_edges;
use super::goals::query_goals;
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
    req: &QueryRequest,
    schemas: &[SchemaInfo],
) -> Result<QueryResponse, StorageError> {
    let owner_kind: &str = match &req.owner.principal {
        Principal::User(_) => "User",
        Principal::Group(_) => "Group",
    };
    let owner_principal_id = match &req.owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    let id_hydration =
        !req.memory_ids.is_empty() || !req.goal_ids.is_empty() || !req.edge_ids.is_empty();
    let schema_id_filter = req.schema_id.as_ref().map(|s| s.as_str().to_string());
    if matches!(req.entity_kind, Some(EntityKind::Goal)) {
        return Ok(QueryResponse {
            memories: Vec::new(),
            goals: query_goals(
                pool,
                req,
                owner_kind,
                owner_principal_id,
                schema_id_filter.as_deref(),
            )
            .await?,
            edges: Vec::new(),
            seq_high_water: read_seq_high_water(pool, owner_kind, owner_principal_id).await?,
        });
    }

    let stateful = validated_stateful_filters(req)?;

    // Build payload projection: for each F/A/P schema with a sidecar
    // table, LEFT JOIN the sidecar and emit a CASE expression that
    // picks the matching row value for CBOR encoding.
    //
    // Edge sidecars are keyed on `edge_id`, not `memory_id` — they
    // don't participate in memory queries. Goal sidecars don't either:
    // Goals are a distinct entity (AGENTS.md invariant 11), and goal
    // queries short-circuit above.
    let schemas_with_sidecar: Vec<&SchemaInfo> = schemas
        .iter()
        .filter(|s| {
            s.sidecar_table.is_some()
                && matches!(
                    s.kind,
                    PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
                )
        })
        .collect();

    let mut sql = String::from(
        "SELECT m.memory_id, m.owner_principal_kind, m.owner_principal_id, \
                m.owner_org_id, m.schema_id, m.schema_version, m.kind, m.event_id,",
    );

    // Bindings: $1=owner_kind, $2=owner_principal_id.
    // Schema-id literals for the payload-projection CASE always come
    // after optional filters.
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
    let stateful_params: Vec<StatefulSqlParams> = stateful
        .iter()
        .map(|sf| {
            let schema = next_param;
            next_param += 1;
            let version = next_param;
            next_param += 1;
            let tombstone = sf.tombstone.as_ref().map(|_| {
                let param = next_param;
                next_param += 1;
                param
            });
            StatefulSqlParams {
                schema,
                version,
                tombstone,
            }
        })
        .collect();
    let case_param_base = next_param;

    // If there are schemas with sidecars, add the payload_json CASE expression.
    // Otherwise, just add NULL as payload_json.
    if schemas_with_sidecar.is_empty() {
        sql.push_str(" NULL AS payload_json");
    } else {
        sql.push_str(" CASE");
        for (idx, schema) in schemas_with_sidecar.iter().enumerate() {
            let sidecar_table = schema.sidecar_table.as_ref().unwrap();
            PgIdent::table(sidecar_table)?;
            let alias = format!("s_{idx}");
            write!(
                sql,
                " WHEN m.schema_id = ${} AND m.schema_version = ${} \
                  THEN row_to_json({alias})::text",
                case_param_base + (idx * 2),
                case_param_base + (idx * 2) + 1,
                alias = alias
            )
            .expect("write to String is infallible");
        }
        sql.push_str(" ELSE NULL END AS payload_json");
    }

    sql.push_str(
        " \
         FROM proxima_core.memories m",
    );

    // Add LEFT JOINs for each schema with a sidecar table
    for (idx, schema) in schemas_with_sidecar.iter().enumerate() {
        let sidecar_table = PgIdent::table(schema.sidecar_table.as_ref().unwrap())?;
        let alias = format!("s_{idx}");
        write!(
            sql,
            " LEFT JOIN {sidecar_table} {alias} ON {alias}.memory_id = m.memory_id",
            sidecar_table = sidecar_table.as_str(),
        )
        .expect("write to String is infallible");
    }

    // The stateful JOINs (for head-by-natural-key filtering) use
    // separate alias to avoid colliding with the payload JOINs. We
    // use explicit `ON sf_i.memory_id = m.memory_id` rather than
    // `USING (memory_id)` because the payload LEFT JOINs above each
    // contribute their own `memory_id` to the left side, and
    // `USING` would barf with "common column name appears more than
    // once".
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

    sql.push_str(" WHERE m.owner_principal_kind = $1 AND m.owner_principal_id = $2");

    match req.entity_kind {
        None => {}
        Some(EntityKind::Fact) => {
            sql.push_str(" AND m.event_id IS NOT NULL AND m.kind IS NULL");
        }
        Some(EntityKind::Abstraction) => sql.push_str(" AND m.kind = 'Abstraction'"),
        Some(EntityKind::Perspective) => sql.push_str(" AND m.kind = 'Perspective'"),
        Some(EntityKind::Goal) => unreachable!(),
    }

    if schema_id_filter.is_some() {
        write!(sql, " AND m.schema_id = ${}", schema.unwrap())
            .expect("write to String is infallible");
    }

    if let Some(param) = memory_ids_param {
        write!(sql, " AND m.memory_id = ANY(${param})").expect("write to String is infallible");
    } else if id_hydration {
        sql.push_str(" AND false");
    }

    if matches!(req.supersession, SupersessionStatus::HeadsOnly) {
        if stateful.is_empty() {
            sql.push_str(
                " AND NOT EXISTS (SELECT 1 FROM proxima_core.memories m2 \
                                  WHERE m2.supersedes = m.memory_id)",
            );
        } else {
            sql.push_str(" AND (");
            for (idx, sf) in stateful.iter().enumerate() {
                if idx > 0 {
                    sql.push_str(" OR ");
                }
                push_stateful_head_branch(&mut sql, idx, sf, &stateful_params[idx]);
            }
            sql.push_str(" OR (");
            push_not_stateful_match(&mut sql, &stateful_params);
            sql.push_str(
                " AND NOT EXISTS (SELECT 1 FROM proxima_core.memories m2 \
                                  WHERE m2.supersedes = m.memory_id))",
            );
            sql.push(')');
        }
    }

    push_tombstone_exclusion(&mut sql, req, &stateful, &stateful_params);

    sql.push_str(" ORDER BY m.created_at DESC LIMIT ");
    sql.push_str(&u64::from(req.limit).to_string());

    let mut q = sqlx::query_as::<_, MemoryRowDb>(&sql)
        .bind(owner_kind)
        .bind(owner_principal_id);
    if let Some(sid) = &schema_id_filter {
        q = q.bind(sid.clone());
    }
    if !memory_ids.is_empty() {
        q = q.bind(memory_ids);
    }
    for (sf, params) in stateful.iter().zip(stateful_params.iter()) {
        let _ = params;
        q = q.bind(sf.schema_id.as_str().to_string());
        q = q.bind(sf.schema_version.into_inner().cast_signed());
        if let Some(tombstone) = &sf.tombstone {
            q = q.bind(tombstone.value.clone());
        }
    }
    // Bind (schema_id, schema_version) values for the CASE expression
    // in payload projection. Multiple schema versions can share the
    // same schema_id while living in different sidecar tables.
    for schema in &schemas_with_sidecar {
        q = q.bind(schema.schema_id.as_str().to_string());
        q = q.bind(schema.schema_version.into_inner().cast_signed());
    }

    let rows: Vec<MemoryRowDb> = q
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    let memories: Vec<MemoryRow> = rows
        .into_iter()
        .map(|row| memory_row_from_db(row, schemas))
        .collect::<Result<Vec<_>, _>>()?;

    let goals = if req.entity_kind.is_none() || matches!(req.entity_kind, Some(EntityKind::Goal)) {
        query_goals(
            pool,
            req,
            owner_kind,
            owner_principal_id,
            schema_id_filter.as_deref(),
        )
        .await?
    } else {
        Vec::new()
    };
    let visible_memory_ids: Vec<uuid::Uuid> =
        memories.iter().map(|row| row.id.into_inner()).collect();
    let visible_goal_ids: Vec<uuid::Uuid> = goals.iter().map(|row| row.id.into_inner()).collect();
    let edges = query_edges(
        pool,
        req,
        owner_kind,
        owner_principal_id,
        &visible_memory_ids,
        &visible_goal_ids,
    )
    .await?;
    let seq_high_water = read_seq_high_water(pool, owner_kind, owner_principal_id).await?;

    Ok(QueryResponse {
        memories,
        goals,
        edges,
        seq_high_water,
    })
}

pub(super) async fn visible_ids_for(
    pool: &PgPool,
    req: &QueryRequest,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
    candidate_memory_ids: &[uuid::Uuid],
    candidate_goal_ids: &[uuid::Uuid],
) -> Result<(HashSet<uuid::Uuid>, HashSet<uuid::Uuid>), StorageError> {
    let memory_ids = query_visible_memory_ids(
        pool,
        req,
        owner_kind,
        owner_principal_id,
        candidate_memory_ids,
    )
    .await?;
    let goal_ids = query_visible_goal_ids(
        pool,
        req,
        owner_kind,
        owner_principal_id,
        candidate_goal_ids,
    )
    .await?;
    Ok((memory_ids, goal_ids))
}

#[allow(clippy::too_many_lines)]
async fn query_visible_memory_ids(
    pool: &PgPool,
    req: &QueryRequest,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
    candidate_memory_ids: &[uuid::Uuid],
) -> Result<HashSet<uuid::Uuid>, StorageError> {
    if candidate_memory_ids.is_empty() || matches!(req.entity_kind, Some(EntityKind::Goal)) {
        return Ok(HashSet::new());
    }
    let stateful = validated_stateful_filters(req)?;
    let schema_id_filter = req.schema_id.as_ref().map(|s| s.as_str().to_string());

    let mut sql = String::from("SELECT m.memory_id FROM proxima_core.memories m");
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

    sql.push_str(" WHERE m.owner_principal_kind = $1 AND m.owner_principal_id = $2");
    sql.push_str(" AND m.memory_id = ANY($3::uuid[])");
    let mut next_param = 4;
    let schema = schema_id_filter.as_ref().map(|_| {
        let param = next_param;
        next_param += 1;
        param
    });
    let stateful_params: Vec<StatefulSqlParams> = stateful
        .iter()
        .map(|sf| {
            let schema = next_param;
            next_param += 1;
            let version = next_param;
            next_param += 1;
            let tombstone = sf.tombstone.as_ref().map(|_| {
                let param = next_param;
                next_param += 1;
                param
            });
            StatefulSqlParams {
                schema,
                version,
                tombstone,
            }
        })
        .collect();

    match req.entity_kind {
        None => {}
        Some(EntityKind::Fact) => {
            sql.push_str(" AND m.event_id IS NOT NULL AND m.kind IS NULL");
        }
        Some(EntityKind::Abstraction) => sql.push_str(" AND m.kind = 'Abstraction'"),
        Some(EntityKind::Perspective) => sql.push_str(" AND m.kind = 'Perspective'"),
        Some(EntityKind::Goal) => unreachable!(),
    }
    if let Some(param) = schema {
        write!(sql, " AND m.schema_id = ${param}").expect("write to String is infallible");
    }
    if matches!(req.supersession, SupersessionStatus::HeadsOnly) {
        if stateful.is_empty() {
            sql.push_str(
                " AND NOT EXISTS (SELECT 1 FROM proxima_core.memories m2 \
                                  WHERE m2.supersedes = m.memory_id)",
            );
        } else {
            sql.push_str(" AND (");
            for (idx, sf) in stateful.iter().enumerate() {
                if idx > 0 {
                    sql.push_str(" OR ");
                }
                push_stateful_head_branch(&mut sql, idx, sf, &stateful_params[idx]);
            }
            sql.push_str(" OR (");
            push_not_stateful_match(&mut sql, &stateful_params);
            sql.push_str(
                " AND NOT EXISTS (SELECT 1 FROM proxima_core.memories m2 \
                                  WHERE m2.supersedes = m.memory_id))",
            );
            sql.push(')');
        }
    }
    push_tombstone_exclusion(&mut sql, req, &stateful, &stateful_params);

    let mut q = sqlx::query_as::<_, (uuid::Uuid,)>(&sql)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(candidate_memory_ids);
    if let Some(sid) = &schema_id_filter {
        q = q.bind(sid.clone());
    }
    for sf in &stateful {
        q = q.bind(sf.schema_id.as_str().to_string());
        q = q.bind(sf.schema_version.into_inner().cast_signed());
        if let Some(tombstone) = &sf.tombstone {
            q = q.bind(tombstone.value.clone());
        }
    }
    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

async fn query_visible_goal_ids(
    pool: &PgPool,
    req: &QueryRequest,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
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
        "SELECT g.goal_id FROM proxima_core.goals g \
         WHERE g.owner_principal_kind = $1 \
           AND g.owner_principal_id = $2 \
           AND g.goal_id = ANY($3::uuid[])",
    );
    let schema_id_filter = req.schema_id.as_ref().map(|s| s.as_str().to_string());
    if schema_id_filter.is_some() {
        sql.push_str(" AND g.schema_id = $4");
    }
    if matches!(req.supersession, SupersessionStatus::HeadsOnly) {
        sql.push_str(
            " AND NOT EXISTS (SELECT 1 FROM proxima_core.goals g2 \
                              WHERE g2.supersedes = g.goal_id)",
        );
    }
    let mut q = sqlx::query_as::<_, (uuid::Uuid,)>(&sql)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(candidate_goal_ids);
    if let Some(sid) = schema_id_filter {
        q = q.bind(sid);
    }
    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
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
              AND m2.owner_principal_kind = m.owner_principal_kind \
              AND m2.owner_principal_id = m.owner_principal_id \
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
              AND {alias}.{column} = ${tombstone})",
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
