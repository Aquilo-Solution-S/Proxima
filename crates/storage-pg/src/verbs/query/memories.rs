use std::collections::HashMap;
use std::fmt::Write as _;

use futures_util::future::try_join_all;
use proxima_core::verbs::query::{
    EntityKind, QueryCursor, QueryRequest, QueryResponse, SupersessionStatus, TombstoneFilter,
};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{MemoryId, SchemaId, SchemaVersion, SidecarPayload, StorageError};
use sqlx::PgPool;

use crate::error::map_err;
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
    _schemas: &[SchemaInfo],
) -> Result<QueryResponse, StorageError> {
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(&req.read_owners);
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

    let memory_ids: Vec<uuid::Uuid> = req.memory_ids.iter().map(|id| id.into_inner()).collect();
    let sql = memory_page_sql(
        req,
        schema_id_filter.is_some(),
        !memory_ids.is_empty(),
        &stateful,
        cursor.is_some(),
        fetch_limit,
    );

    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query_as::<_, MemoryRowDb>(sqlx::AssertSqlSafe(sql))
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

    let mut rows: Vec<MemoryRowDb> = q.fetch_all(pool).await.map_err(map_err)?;
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
    let edges = query_edges(pool, req, &visible_memory_ids, &visible_goal_ids).await?;
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
        let kind = match row.kind {
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

/// The keyset page statement for one tuning arm. `$1`/`$2` are the read
/// owner arrays; the remaining parameter numbers are allocated here in the
/// same order [`query_memories`] binds them.
#[allow(clippy::fn_params_excessive_bools)]
fn memory_page_sql(
    req: &QueryRequest,
    has_schema_filter: bool,
    has_memory_ids: bool,
    stateful: &[&proxima_core::verbs::query::StatefulHeadsFilter],
    has_cursor: bool,
    fetch_limit: u64,
) -> String {
    let id_hydration = !req.memory_ids.is_empty() || !req.goal_ids.is_empty();
    // Bindings: $1=read_owner_kinds, $2=read_owner_ids.
    let mut next_param = 3;
    let schema = has_schema_filter.then(|| {
        let param = next_param;
        next_param += 1;
        param
    });
    let memory_ids_param = has_memory_ids.then(|| {
        let param = next_param;
        next_param += 1;
        param
    });
    let stateful_params = allocate_stateful_params(stateful, &mut next_param);
    let cursor_params = has_cursor.then(|| {
        let created_at = next_param;
        next_param += 1;
        let memory_id = next_param;
        (created_at, memory_id)
    });
    // One owner-scoped page body, shared by every arm below.
    let inner_page = |owner_predicate: &str| -> String {
        let mut inner = String::from(
            "SELECT m.memory_id, m.created_at, m.owner_kind, m.owner_id, \
                    m.schema_id, m.schema_version, m.kind \
               FROM proxima_core.memories m",
        );
        // The stateful JOINs (for head-by-natural-key filtering) use
        // explicit `ON sf_i.memory_id = m.memory_id` so generated aliases
        // stay unambiguous.
        for (idx, sf) in stateful.iter().enumerate() {
            let alias = stateful_alias(idx);
            write!(
                inner,
                " LEFT JOIN {sidecar} {alias} ON {alias}.memory_id = m.memory_id",
                sidecar = sf.sidecar_table,
                alias = alias,
            )
            .expect("write to String is infallible");
        }
        write!(
            inner,
            " WHERE {owner_predicate} \
                AND m.tombstoned_at IS NULL",
        )
        .expect("write to String is infallible");
        if let Some(param) = memory_ids_param {
            write!(inner, " AND m.memory_id = ANY(${param})")
                .expect("write to String is infallible");
        } else if id_hydration {
            inner.push_str(" AND false");
        }
        push_heads_predicate(&mut inner, req, schema, stateful, &stateful_params);
        if let Some((created_at_param, memory_id_param)) = cursor_params {
            write!(
                inner,
                " AND (m.created_at, m.memory_id) < (${created_at_param}, ${memory_id_param})"
            )
            .expect("write to String is infallible");
        }
        write!(
            inner,
            " ORDER BY m.created_at DESC, m.memory_id DESC LIMIT {fetch_limit}"
        )
        .expect("write to String is infallible");
        inner
    };

    // `memories.owner_id` is NULL for a World-owned row (0008_v005 dropped
    // `memories_world_not_write_owner_chk`: World is a valid persisted
    // owner via `Engine::publish_to_world`, not just a fresh-write guard
    // target). `s.id` is NULL for the World member of the caller's read-
    // owner set, so plain `=` would silently drop every World-owned row
    // (NULL = NULL is NULL, not true) even though the caller is authorized
    // to read World. INDF is correct but not indexable. This joins with
    // plain `=` — restoring the `(owner_kind, owner_id, created_at,
    // memory_id)` index prefix — and appends the page body once more as a
    // constant World arm, only when World is actually in the read set: the
    // split `search.rs::push_read_owner_scope` already ships. The
    // arms are disjoint (an equality join never matches a NULL-id read-set
    // row, and the World arm's rows carry a kind no non-World member has),
    // and each arm keeps the per-arm ORDER/LIMIT, so the merged page is the
    // same row set the IS NOT DISTINCT FROM join returned.
    {
        let member_arm = format!(
            "SELECT lat.* \
             FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id) \
             JOIN LATERAL ( {inner}) lat ON TRUE",
            inner = inner_page(&super::read_owner_equality_predicate("m", "s")),
        );
        let world_in_read_set = req
            .read_owners
            .iter()
            .any(|owner| matches!(owner, proxima_core::OwnerRef::World));
        let body = if world_in_read_set {
            // Each arm keeps its own ORDER/LIMIT, so both must be
            // parenthesized: unparenthesized, the World arm's ORDER BY
            // would bind to the whole union — where `m` is out of scope —
            // instead of to its arm (PostgreSQL docs §7.4, "Combining
            // Queries": ORDER BY/LIMIT apply to a set-operation branch
            // only when the branch is enclosed in parentheses).
            format!(
                "({member_arm}) UNION ALL ({world_arm})",
                world_arm = inner_page("m.owner_kind = 'world' AND m.owner_id IS NULL"),
            )
        } else {
            member_arm
        };
        format!(
            "SELECT page.memory_id, page.created_at, page.owner_kind, page.owner_id, \
                    page.schema_id, page.schema_version, page.kind \
             FROM ( {body}) page \
             ORDER BY page.created_at DESC, page.memory_id DESC LIMIT {fetch_limit}"
        )
    }
}

/// [`memory_page_sql`] with the request-derived inputs recomputed exactly
/// as [`query_memories`] derives them, for plan and equivalence assertions
/// in tests. Same cfg gate as the search `*_sql_for_tests` exports.
///
/// # Errors
///
/// Returns the same validation error [`query_memories`] would for a
/// malformed stateful-heads filter.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn memory_page_sql_for_tests(req: &QueryRequest) -> Result<String, StorageError> {
    let stateful = validated_stateful_filters(req)?;
    let single_memory_stream = matches!(
        req.entity_kind,
        Some(EntityKind::Fact | EntityKind::Abstraction | EntityKind::Perspective)
    );
    let fetch_limit = if single_memory_stream {
        u64::from(req.limit) + 1
    } else {
        u64::from(req.limit)
    };
    Ok(memory_page_sql(
        req,
        req.schema_id.is_some(),
        !req.memory_ids.is_empty(),
        &stateful,
        matches!(&req.page.after, Some(QueryCursor::Memory { .. })),
        fetch_limit,
    ))
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
            sql.push_str(" AND m.kind = 'Fact'");
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
