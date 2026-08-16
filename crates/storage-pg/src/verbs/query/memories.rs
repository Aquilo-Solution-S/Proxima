//! Query starts at `memory_head`. `HeadsOnly` = current `t` per handle.
//! `IncludeSuperseded` = every hot `t` of those handles.

use std::collections::HashMap;
use std::fmt::Write as _;

use futures_util::future::try_join_all;
use proxima_core::verbs::query::{
    EntityKind, QueryCursor, QueryRequest, QueryResponse, SupersessionStatus,
};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{MemoryId, SchemaId, SchemaVersion, SidecarPayload, StorageError};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_err;
use crate::sidecars::{PgSidecarKey, PgSidecarReadCtx, PgSidecarRegistryFrozen};

use super::edges::query_edges;
use super::goals::query_goals;
use super::rows::{MemoryRowDb, memory_row_from_db, read_seq_high_water};

pub(crate) async fn query_memories(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &QueryRequest,
    _schemas: &[SchemaInfo],
) -> Result<QueryResponse, StorageError> {
    let owner_ids: Vec<Uuid> = req
        .read_owners
        .iter()
        .copied()
        .map(proxima_core::OwnerRef::stored_owner_id)
        .collect();
    let schema_id_filter = req.schema_id.as_ref().map(|s| s.as_str().to_string());
    if matches!(req.entity_kind, Some(EntityKind::Goal)) {
        let (goals, next_cursor) =
            query_goals(pool, req, &owner_ids, schema_id_filter.as_deref()).await?;
        return Ok(QueryResponse {
            memories: Vec::new(),
            goals,
            edges: Vec::new(),
            next_cursor,
            seq_high_water: read_seq_high_water(pool, &owner_ids).await?,
        });
    }

    let cursor_t = match &req.page.after {
        Some(QueryCursor::Memory { memory_id, .. }) => Some(memory_id.into_inner()),
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

    let memory_ids: Vec<Uuid> = req.memory_ids.iter().map(|id| id.into_inner()).collect();
    let kind_filter = match req.entity_kind {
        Some(EntityKind::Fact) => Some("fact"),
        Some(EntityKind::Abstraction) => Some("abstraction"),
        Some(EntityKind::Perspective) => Some("perspective"),
        Some(EntityKind::Goal) | None => None,
    };
    let sql = memory_page_sql(
        matches!(req.supersession, SupersessionStatus::HeadsOnly),
        schema_id_filter.is_some(),
        kind_filter.is_some(),
        !memory_ids.is_empty(),
        cursor_t.is_some(),
        fetch_limit,
    );

    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query_as::<_, MemoryRowDb>(sqlx::AssertSqlSafe(sql)).bind(&owner_ids);
    if let Some(sid) = &schema_id_filter {
        q = q.bind(sid.clone());
    }
    if let Some(kind) = kind_filter {
        q = q.bind(kind);
    }
    if !memory_ids.is_empty() {
        q = q.bind(memory_ids);
    }
    if let Some(t) = cursor_t {
        q = q.bind(t);
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
            query_goals(pool, req, &owner_ids, schema_id_filter.as_deref()).await?
        } else {
            (Vec::new(), None)
        };
    let visible_goal_ids: Vec<Uuid> = goals.iter().map(|row| row.id.into_inner()).collect();
    let edges = query_edges(req, &memories, &visible_goal_ids);
    let seq_high_water = read_seq_high_water(pool, &owner_ids).await?;

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
        let kind = match row.kind.as_str() {
            "fact" | "Fact" => PayloadKind::Fact,
            "abstraction" | "Abstraction" => PayloadKind::Abstraction,
            "perspective" | "Perspective" => PayloadKind::Perspective,
            _ => continue,
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

#[allow(clippy::fn_params_excessive_bools)]
fn memory_page_sql(
    heads_only: bool,
    has_schema: bool,
    has_kind: bool,
    has_ids: bool,
    has_cursor: bool,
    fetch_limit: u64,
) -> String {
    let from = if heads_only {
        "FROM proxima_core.memory_head h \
         JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t"
    } else {
        "FROM proxima_core.memory m \
         JOIN proxima_core.memory_head h ON h.handle = m.handle"
    };
    let mut sql = format!(
        "SELECT m.t AS memory_id, m.handle, \
                COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01') AS created_at, \
                o.kind::text::proxima_core.owner_kind AS owner_kind, \
                m.owner_id, h.schema_id, 1::int4 AS schema_version, \
                m.kind::text AS kind, m.origins, m.refs \
         {from} \
         JOIN proxima_core.owners o ON o.owner_id = m.owner_id \
         WHERE m.owner_id = ANY($1::uuid[])"
    );
    let mut next = 2_u32;
    if has_schema {
        let _ = write!(sql, " AND h.schema_id = ${next}");
        next += 1;
    }
    if has_kind {
        let _ = write!(sql, " AND m.kind::text = ${next}");
        next += 1;
    }
    if has_ids {
        let _ = write!(sql, " AND m.t = ANY(${next}::uuid[])");
        next += 1;
    }
    if has_cursor {
        let _ = write!(sql, " AND m.t < ${next}");
    }
    let _ = write!(sql, " ORDER BY m.t DESC LIMIT {fetch_limit}");
    sql
}

/// [`memory_page_sql`] with the request-derived inputs recomputed exactly
/// as [`query_memories`] derives them.
///
/// # Errors
///
/// Never fails on the timeseries path (stateful-NK filters are not used).
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn memory_page_sql_for_tests(req: &QueryRequest) -> Result<String, StorageError> {
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
        matches!(req.supersession, SupersessionStatus::HeadsOnly),
        req.schema_id.is_some(),
        req.entity_kind.is_some() && !matches!(req.entity_kind, Some(EntityKind::Goal)),
        !req.memory_ids.is_empty(),
        matches!(&req.page.after, Some(QueryCursor::Memory { .. })),
        fetch_limit,
    ))
}
