//! Query starts at `memory_head`. `HeadsOnly` = current `t` per handle.
//! `IncludeSuperseded` = every hot `t` of those handles.

use std::collections::HashMap;
use std::fmt::Write as _;

use futures_util::future::try_join_all;
use proxima_core::read_models::MemorySchemaSpec;
use proxima_core::verbs::query::{
    EntityKind, MemoryRow, QueryCursor, QueryRequest, QueryResponse, SupersessionStatus,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{MemoryId, SchemaId, SidecarPayload, StorageError};
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
    schemas: &[MemorySchemaSpec],
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

    let single_memory_stream = matches!(
        req.entity_kind,
        Some(EntityKind::Fact | EntityKind::Abstraction | EntityKind::Perspective)
    );
    let mut rows = fetch_memory_page(
        pool,
        req,
        &owner_ids,
        schema_id_filter.as_deref(),
        single_memory_stream,
    )
    .await?;
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

    let mut memories = project_memory_rows(pool, sidecars, req, schemas, rows).await?;
    let (goals, next_goal_cursor) =
        if req.entity_kind.is_none() || matches!(req.entity_kind, Some(EntityKind::Goal)) {
            query_goals(pool, req, &owner_ids, schema_id_filter.as_deref()).await?
        } else {
            (Vec::new(), None)
        };
    let visible_goal_ids: Vec<Uuid> = goals.iter().map(|row| row.id.into_inner()).collect();
    demote_invisible_goal_refs(&mut memories, &visible_goal_ids);
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

/// Read one page of memory rows for the request's filters. `single_memory_stream`
/// asks for one row past the limit so the caller can tell a full page from an
/// exhausted one.
async fn fetch_memory_page(
    pool: &PgPool,
    req: &QueryRequest,
    owner_ids: &[Uuid],
    schema_id_filter: Option<&str>,
    single_memory_stream: bool,
) -> Result<Vec<MemoryRowDb>, StorageError> {
    let cursor_t = match &req.page.after {
        Some(QueryCursor::Memory { memory_id, .. }) => Some(memory_id.into_inner()),
        _ => None,
    };
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
    let mut q = sqlx::query_as::<_, MemoryRowDb>(sqlx::AssertSqlSafe(sql)).bind(owner_ids);
    if let Some(sid) = schema_id_filter {
        q = q.bind(sid.to_owned());
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

    q.fetch_all(pool).await.map_err(map_err)
}

/// Resolve each row's schema, verify its sidecar stamp, and project the page
/// into read-model rows.
async fn project_memory_rows(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &QueryRequest,
    schemas: &[MemorySchemaSpec],
    rows: Vec<MemoryRowDb>,
) -> Result<Vec<MemoryRow>, StorageError> {
    let mut schema_versions = HashMap::new();
    for row in &rows {
        let kind = parse_memory_kind(&row.kind)?;
        let spec = proxima_core::resolve_memory_schema(
            schemas,
            kind,
            &SchemaId::new(row.schema_id.clone()),
        )?;
        validate_row_stamp(
            sidecars,
            spec,
            &row.sidecar_tables,
            MemoryId::new(row.memory_id),
        )?;
        schema_versions.insert(MemoryId::new(row.memory_id), spec.schema_version);
    }
    // `include_payloads` controls projection, not integrity. A required
    // primary sidecar that disappeared is corruption even when the caller
    // asks only for identity fields, so every query verifies its presence.
    let mut payloads = load_row_payloads_batch(pool, sidecars, schemas, &rows).await?;
    let mut memories = Vec::with_capacity(rows.len());
    for row in rows {
        let id = MemoryId::new(row.memory_id);
        let loaded_payload = payloads.remove(&id);
        let schema_version = schema_versions
            .remove(&id)
            .ok_or_else(|| StorageError::Internal("query schema resolution lost row".into()))?;
        if proxima_core::resolve_memory_schema(
            schemas,
            parse_memory_kind(&row.kind)?,
            &SchemaId::new(row.schema_id.clone()),
        )?
        .sidecar_table
        .as_deref()
        .is_some_and(|table| {
            loaded_payload.is_none() && !sidecars.is_owner_pinned_memory_sidecar_table(table)
        }) {
            return Err(StorageError::ConstraintViolation(format!(
                "required sidecar payload missing for memory {id:?}"
            )));
        }
        let payload = req.include_payloads.then_some(loaded_payload).flatten();
        memories.push(memory_row_from_db(row, payload, schema_version)?);
    }
    Ok(memories)
}

/// A Goal reference the caller may not read is not a Goal edge for this
/// response; it stays a plain reference rather than disappearing.
fn demote_invisible_goal_refs(memories: &mut [MemoryRow], visible_goal_ids: &[Uuid]) {
    let visible_goal_set: std::collections::HashSet<Uuid> =
        visible_goal_ids.iter().copied().collect();
    for memory in memories {
        let mut visible = Vec::with_capacity(memory.goal_refs.len());
        for goal in memory.goal_refs.drain(..) {
            if visible_goal_set.contains(&goal.into_inner()) {
                visible.push(goal);
            } else {
                memory.refs.push(MemoryId::new(goal.into_inner()));
            }
        }
        memory.goal_refs = visible;
    }
}

async fn load_row_payloads_batch(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    schemas: &[MemorySchemaSpec],
    rows: &[MemoryRowDb],
) -> Result<HashMap<MemoryId, SidecarPayload>, StorageError> {
    let mut ids_by_key = HashMap::<PgSidecarKey, Vec<MemoryId>>::new();
    for row in rows {
        let entity_kind = parse_memory_kind(&row.kind)?;
        let spec = proxima_core::resolve_memory_schema(
            schemas,
            entity_kind,
            &SchemaId::new(row.schema_id.clone()),
        )?;
        let kind = payload_kind_for(entity_kind).expect("memory kind has payload kind");
        validate_row_stamp(
            sidecars,
            spec,
            &row.sidecar_tables,
            MemoryId::new(row.memory_id),
        )?;
        let Some(schema_sidecar) = spec.sidecar_table.as_ref() else {
            continue;
        };
        let _ = schema_sidecar;
        let key = PgSidecarKey::new(kind, spec.schema_id.clone(), spec.schema_version);
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
        "FROM proxima_core.memory m"
    };
    let owner_pred = if heads_only {
        "h.owner_id = ANY($1::uuid[])"
    } else {
        "m.owner_id = ANY($1::uuid[])"
    };
    let mut sql = format!(
        "SELECT m.t AS memory_id, m.handle, \
                COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01') AS created_at, \
                o.kind::text::proxima_core.owner_kind AS owner_kind, \
                m.owner_id, m.schema_id, m.sidecar_tables, \
                m.kind::text AS kind, m.origins, m.refs, m.goal_refs \
         {from} \
         JOIN proxima_core.owners o ON o.owner_id = m.owner_id \
         WHERE {owner_pred}"
    );
    let mut next = 2_u32;
    if has_schema {
        let schema_col = if heads_only {
            "h.schema_id"
        } else {
            "m.schema_id"
        };
        let _ = write!(sql, " AND {schema_col} = ${next}");
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

fn parse_memory_kind(kind: &str) -> Result<EntityKind, StorageError> {
    match kind {
        "fact" | "Fact" => Ok(EntityKind::Fact),
        "abstraction" | "Abstraction" => Ok(EntityKind::Abstraction),
        "perspective" | "Perspective" => Ok(EntityKind::Perspective),
        other => Err(StorageError::ConstraintViolation(format!(
            "invalid memory kind {other}"
        ))),
    }
}

fn payload_kind_for(kind: EntityKind) -> Option<PayloadKind> {
    match kind {
        EntityKind::Fact => Some(PayloadKind::Fact),
        EntityKind::Abstraction => Some(PayloadKind::Abstraction),
        EntityKind::Perspective => Some(PayloadKind::Perspective),
        EntityKind::Goal => None,
    }
}

fn validate_row_stamp(
    sidecars: &PgSidecarRegistryFrozen,
    spec: &MemorySchemaSpec,
    stamped_tables: &[String],
    memory_id: MemoryId,
) -> Result<(), StorageError> {
    let Some(table) = spec.sidecar_table.as_deref() else {
        return Ok(());
    };
    let kind = payload_kind_for(spec.kind).expect("memory kind has payload kind");
    if sidecars.table_for_schema(kind, &spec.schema_id, spec.schema_version) != Some(table)
        || !stamped_tables.iter().any(|stamped| stamped == table)
    {
        return Err(StorageError::ConstraintViolation(format!(
            "memory {memory_id:?} has invalid sidecar stamp for {table}"
        )));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{MemorySchemaSpec, validate_row_stamp};
    use crate::sidecars::{PgSidecarRegistryFrozen, core_pg_sidecars};
    use proxima_core::{AgentNoteV1, EntityKind, FactPayload, MemoryId, SchemaId, SchemaVersion};

    #[test]
    fn required_primary_stamp_is_checked_against_the_frozen_sidecar_registry() {
        let spec = MemorySchemaSpec {
            kind: EntityKind::Fact,
            schema_id: SchemaId::new("test/fact".to_owned()),
            schema_version: SchemaVersion::new(2),
            sidecar_table: Some("test.fact_v2".to_owned()),
        };
        let err = validate_row_stamp(
            &PgSidecarRegistryFrozen::default(),
            &spec,
            &[],
            MemoryId::new(uuid::Uuid::now_v7()),
        )
        .expect_err("unregistered or unstamped primary must fail closed");
        assert!(err.to_string().contains("invalid sidecar stamp"));
    }

    #[test]
    fn sidecarless_registered_memory_needs_no_stamp() {
        let spec = MemorySchemaSpec {
            kind: EntityKind::Fact,
            schema_id: SchemaId::new("test/fact".to_owned()),
            schema_version: SchemaVersion::new(2),
            sidecar_table: None,
        };
        validate_row_stamp(
            &PgSidecarRegistryFrozen::default(),
            &spec,
            &["extra.table".to_owned()],
            MemoryId::new(uuid::Uuid::now_v7()),
        )
        .expect("sidecarless memory permits extra declared stamps");
    }

    #[test]
    fn primary_stamp_must_be_present_but_declared_extensions_do_not_interfere() {
        let registry = core_pg_sidecars();
        let primary = AgentNoteV1::sidecar_table().expect("AgentNote has a primary sidecar");
        let spec = MemorySchemaSpec {
            kind: EntityKind::Fact,
            schema_id: AgentNoteV1::schema_id(),
            schema_version: SchemaVersion::new(AgentNoteV1::SCHEMA_VERSION),
            sidecar_table: Some(primary.to_owned()),
        };
        let memory_id = MemoryId::new(uuid::Uuid::now_v7());

        validate_row_stamp(&registry, &spec, &[primary.to_owned()], memory_id)
            .expect("the registered primary stamp agrees");
        validate_row_stamp(
            &registry,
            &spec,
            &["proxima_core.write_act_v1".to_owned(), primary.to_owned()],
            memory_id,
        )
        .expect("an additional declared extension does not select the schema");
        let err = validate_row_stamp(
            &registry,
            &spec,
            &["proxima_core.write_act_v1".to_owned()],
            memory_id,
        )
        .expect_err("a different declared table cannot stand in for the primary");
        assert!(err.to_string().contains("invalid sidecar stamp"));
    }

    #[test]
    fn non_head_query_does_not_join_head_for_schema() {
        let src = include_str!("memories.rs");
        let join = format!(
            "{}{}",
            "JOIN proxima_core.memory_head h ON h.handle = ", "m.handle\""
        );
        assert!(
            !src.contains(&join),
            "IncludeSuperseded reads m.schema_id; head join is HeadsOnly only"
        );
        assert!(
            src.contains("m.schema_id"),
            "query page selects memory.schema_id"
        );
    }

    #[test]
    fn heads_only_schema_predicates_use_head_columns() {
        let sql = super::memory_page_sql(true, true, false, false, false, 10);
        assert!(
            sql.contains("h.owner_id = ANY($1::uuid[])"),
            "HeadsOnly owner filter must hit memory_head_owner_schema_idx: {sql}"
        );
        assert!(
            sql.contains("h.schema_id = $2"),
            "HeadsOnly schema filter must hit memory_head_owner_schema_idx: {sql}"
        );
        assert!(
            !sql.contains("m.owner_id = ANY"),
            "HeadsOnly must not predicate m.owner_id: {sql}"
        );
        assert!(
            !sql.contains("AND m.schema_id"),
            "HeadsOnly must not predicate m.schema_id: {sql}"
        );
    }

    #[test]
    fn include_superseded_schema_predicates_use_memory_columns() {
        let sql = super::memory_page_sql(false, true, false, false, false, 10);
        assert!(
            sql.contains("m.owner_id = ANY($1::uuid[])"),
            "IncludeSuperseded owner filter stays on memory: {sql}"
        );
        assert!(
            sql.contains("AND m.schema_id = $2"),
            "IncludeSuperseded schema filter stays on memory: {sql}"
        );
        assert!(
            !sql.contains("h.owner_id"),
            "IncludeSuperseded has no head join: {sql}"
        );
        assert!(
            !sql.contains("h.schema_id"),
            "IncludeSuperseded has no head schema pred: {sql}"
        );
    }
}
