//! Query starts at `memory_head`. `HeadsOnly` = current `t` per handle.
//! `IncludeSuperseded` = every hot `t` of those handles.

use std::collections::HashMap;
use std::fmt::Write as _;

use proxima_core::read_models::MemorySchemaSpec;
use proxima_core::verbs::query::{
    EntityKind, MemoryRow, QueryCursor, QueryRequest, QueryResponse, SupersessionStatus,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{MemoryId, OwnerRef, SchemaId, SidecarPayload, StorageError};
use sqlx::PgConnection;
#[cfg(test)]
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_err;
use crate::sidecars::{PgSidecarKey, PgSidecarReadCtx, PgSidecarRegistryFrozen};

use super::edges::query_edges;
use super::goals::query_goals_on_connection;
use super::rows::{MemoryRowDb, memory_row_from_db, read_seq_high_water_on_connection};

#[cfg(test)]
async fn query_memories(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    read_owners: &[OwnerRef],
    req: &QueryRequest,
    schemas: &[MemorySchemaSpec],
) -> Result<QueryResponse, StorageError> {
    let mut connection = pool.acquire().await.map_err(crate::error::map_err)?;
    query_memories_on_connection(&mut connection, sidecars, read_owners, req, schemas).await
}

/// Transaction-backed query path. Every statement, including typed sidecar
/// hydration, uses the caller's one borrowed connection.
pub(crate) async fn query_memories_on_connection(
    connection: &mut PgConnection,
    sidecars: &PgSidecarRegistryFrozen,
    read_owners: &[OwnerRef],
    req: &QueryRequest,
    schemas: &[MemorySchemaSpec],
) -> Result<QueryResponse, StorageError> {
    let owner_ids: Vec<Uuid> = read_owners
        .iter()
        .copied()
        .map(proxima_core::OwnerRef::stored_owner_id)
        .collect();
    let seq_high_water = read_seq_high_water_on_connection(connection, &owner_ids).await?;
    let schema_id_filter = req.schema_id.as_ref().map(|s| s.as_str().to_string());
    if matches!(req.entity_kind, Some(EntityKind::Goal)) {
        let (goals, next_cursor) =
            query_goals_on_connection(connection, req, &owner_ids, schema_id_filter.as_deref())
                .await?;
        return Ok(QueryResponse {
            memories: Vec::new(),
            goals,
            edges: Vec::new(),
            next_cursor,
            seq_high_water,
        });
    }
    let single_memory_stream = is_single_memory_stream(req);
    let mut rows = fetch_memory_page_on_connection(connection, req, &owner_ids).await?;
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
    let mut memories =
        project_memory_rows_on_connection(connection, sidecars, req, schemas, rows).await?;
    let (goals, next_goal_cursor) = if req.entity_kind.is_none() {
        query_goals_on_connection(connection, req, &owner_ids, schema_id_filter.as_deref()).await?
    } else {
        (Vec::new(), None)
    };
    let visible_goal_ids: Vec<Uuid> = goals.iter().map(|row| row.id.into_inner()).collect();
    demote_invisible_goal_refs(&mut memories, &visible_goal_ids);
    let edges = query_edges(req, &memories, &visible_goal_ids);
    Ok(QueryResponse {
        memories,
        goals,
        edges,
        next_cursor: next_memory_cursor.or(next_goal_cursor),
        seq_high_water,
    })
}

/// Read one page of memory rows for the request's filters.
///
/// The filter list is built once and then read twice — once to number the
/// placeholders, once to bind them. That is the whole point: the SQL and the
/// arguments are projections of one value, so "the query has N placeholders"
/// and "N arguments were bound" cannot disagree. They used to be two lists
/// grown from the same four predicates, each evaluated twice.
async fn fetch_memory_page_on_connection(
    connection: &mut PgConnection,
    req: &QueryRequest,
    owner_ids: &[Uuid],
) -> Result<Vec<MemoryRowDb>, StorageError> {
    let filters = memory_filters(req);
    let sql = memory_page_sql(
        matches!(req.supersession, SupersessionStatus::HeadsOnly),
        &filters,
        page_fetch_limit(req),
    );
    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query_as::<_, MemoryRowDb>(sqlx::AssertSqlSafe(sql)).bind(owner_ids);
    for filter in filters {
        q = match filter {
            MemoryFilter::Schema(schema_id) => q.bind(schema_id),
            MemoryFilter::Kind(kind) => q.bind(kind),
            MemoryFilter::Ids(ids) => q.bind(ids),
            MemoryFilter::Cursor(t) => q.bind(t),
        };
    }
    q.fetch_all(&mut *connection).await.map_err(map_err)
}

/// An optional `WHERE` predicate together with the value it binds.
///
/// Keeping the two halves in one value is what removes the bookkeeping: a
/// filter contributes a predicate and an argument or neither, and its
/// placeholder number is simply its index in the list.
enum MemoryFilter {
    Schema(String),
    Kind(&'static str),
    Ids(Vec<Uuid>),
    Cursor(Uuid),
}

impl MemoryFilter {
    /// This filter's predicate at `placeholder`, which is its position in
    /// the list the caller will bind in the same order.
    ///
    /// `heads_only` picks the column, not the shape: with the head join in
    /// place the schema predicate has to hit `memory_head` for
    /// `memory_head_owner_schema_idx` to be usable.
    fn predicate(&self, placeholder: u32, heads_only: bool) -> String {
        match self {
            Self::Schema(_) => {
                let column = if heads_only {
                    "h.schema_id"
                } else {
                    "m.schema_id"
                };
                format!(" AND {column} = ${placeholder}")
            }
            Self::Kind(_) => format!(" AND m.kind::text = ${placeholder}"),
            Self::Ids(_) => format!(" AND m.t = ANY(${placeholder}::uuid[])"),
            Self::Cursor(_) => format!(" AND m.t < ${placeholder}"),
        }
    }
}

/// Every filter the request asks for, in bind order.
///
/// The single derivation of "what does this request filter on": the page
/// query and [`memory_page_sql_for_tests`] both read it, so the test cannot
/// assert against a shape production does not build.
fn memory_filters(req: &QueryRequest) -> Vec<MemoryFilter> {
    let mut filters = Vec::new();
    if let Some(schema_id) = req.schema_id.as_ref() {
        filters.push(MemoryFilter::Schema(schema_id.as_str().to_owned()));
    }
    match req.entity_kind {
        Some(EntityKind::Fact) => filters.push(MemoryFilter::Kind("fact")),
        Some(EntityKind::Abstraction) => filters.push(MemoryFilter::Kind("abstraction")),
        Some(EntityKind::Perspective) => filters.push(MemoryFilter::Kind("perspective")),
        Some(EntityKind::Goal) | None => {}
    }
    if !req.memory_ids.is_empty() {
        filters.push(MemoryFilter::Ids(
            req.memory_ids.iter().map(|id| id.into_inner()).collect(),
        ));
    }
    if let Some(QueryCursor::Memory { memory_id, .. }) = &req.page.after {
        filters.push(MemoryFilter::Cursor(memory_id.into_inner()));
    }
    filters
}

/// Whether this request reads one memory stream, and so can be told a full
/// page from an exhausted one by fetching a row past the limit.
fn is_single_memory_stream(req: &QueryRequest) -> bool {
    matches!(
        req.entity_kind,
        Some(EntityKind::Fact | EntityKind::Abstraction | EntityKind::Perspective)
    )
}

fn page_fetch_limit(req: &QueryRequest) -> u64 {
    if is_single_memory_stream(req) {
        u64::from(req.limit) + 1
    } else {
        u64::from(req.limit)
    }
}

/// Resolve each row's schema, verify its sidecar stamp, and project the page
/// into read-model rows.
async fn project_memory_rows_on_connection(
    connection: &mut PgConnection,
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
    let mut payloads =
        load_row_payloads_batch_on_connection(connection, sidecars, schemas, &rows).await?;
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

async fn load_row_payloads_batch_on_connection(
    connection: &mut PgConnection,
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
    let mut result = HashMap::new();
    for (key, ids) in ids_by_key {
        let rows = sidecars
            .load_memory_payloads_batch(PgSidecarReadCtx::from(&mut *connection), &key, &ids)
            .await?;
        result.extend(rows);
    }
    Ok(result)
}

fn memory_page_sql(heads_only: bool, filters: &[MemoryFilter], fetch_limit: u64) -> String {
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
    // `$1` is the owner array; the filters take the placeholders after it,
    // in the order the caller binds them.
    for (index, filter) in filters.iter().enumerate() {
        let placeholder = u32::try_from(index).unwrap_or(u32::MAX).saturating_add(2);
        // SQL-POLICY: fixed-fragment — every arm of `MemoryFilter::predicate`
        // is a literal; the only interpolations are a column picked between
        // two literals by `heads_only` and this `u32` placeholder index. No
        // caller-supplied text reaches the statement, only binds.
        sql.push_str(&filter.predicate(placeholder, heads_only));
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

#[cfg(test)]
mod transaction_visibility_tests {
    use super::{query_memories, query_memories_on_connection};
    use crate::sidecars::core_pg_sidecars;
    use crate::test_fixtures::fresh_pg;
    use proxima_core::read_models::MemorySchemaSpec;
    use proxima_core::verbs::query::QueryRequest;
    use proxima_core::{EntityKind, OwnerRef, SchemaId, SchemaVersion, UserId};

    #[tokio::test]
    async fn connection_query_sees_uncommitted_memory_and_sidecar_but_pool_does_not()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, _db) = fresh_pg("proxima_query_scope").await;
        let pool = pg.pool_for_tests();
        let owner_id = uuid::Uuid::now_v7();
        let handle = uuid::Uuid::now_v7();
        let memory_id = uuid::Uuid::now_v7();
        let note_id = uuid::Uuid::now_v7();
        let owner = OwnerRef::Personal(UserId::new(owner_id));
        let sidecars = core_pg_sidecars();
        let schemas = [MemorySchemaSpec {
            kind: EntityKind::Fact,
            schema_id: SchemaId::new("core/agent-note-v1".into()),
            schema_version: SchemaVersion::new(1),
            sidecar_table: Some("proxima_core.agent_note_v1".into()),
        }];
        let req = QueryRequest::readable();

        let mut tx = pool.begin().await?;
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t) VALUES ($1, 'fact', $2, $3, $4)")
            .bind(handle).bind("core/agent-note-v1").bind(owner_id).bind(memory_id)
            .execute(&mut *tx).await?;
        sqlx::query("INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, sidecar_tables) VALUES ($1, $2, 'fact', $3, $4, $5)")
            .bind(handle).bind(memory_id).bind(owner_id).bind("core/agent-note-v1")
            .bind(vec!["proxima_core.agent_note_v1"])
            .execute(&mut *tx).await?;
        sqlx::query("INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags) VALUES ($1, $2, $3, $4, $5)")
            .bind(memory_id).bind(note_id).bind("uncommitted").bind("visible on tx").bind(Vec::<String>::new())
            .execute(&mut *tx).await?;

        let scoped =
            query_memories_on_connection(&mut tx, &sidecars, &[owner], &req, &schemas).await?;
        assert_eq!(scoped.memories.len(), 1);
        assert_eq!(scoped.memories[0].id.into_inner(), memory_id);
        assert!(scoped.memories[0].payload.is_some());

        let independent = query_memories(pool, &sidecars, &[owner], &req, &schemas).await?;
        assert!(independent.memories.is_empty());
        tx.rollback().await?;
        let after = query_memories(pool, &sidecars, &[owner], &req, &schemas).await?;
        assert!(after.memories.is_empty());
        Ok(())
    }
}

/// The page SQL [`query_memories`] would run for `req`.
///
/// It calls the same [`memory_filters`] and [`page_fetch_limit`] the page
/// read calls, so this cannot describe a query production would not build.
///
/// # Errors
///
/// Never fails on the timeseries path (stateful-NK filters are not used).
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn memory_page_sql_for_tests(req: &QueryRequest) -> Result<String, StorageError> {
    Ok(memory_page_sql(
        matches!(req.supersession, SupersessionStatus::HeadsOnly),
        &memory_filters(req),
        page_fetch_limit(req),
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

    /// The property the filter list exists to guarantee: every filter
    /// contributes exactly one placeholder, numbered by its position after
    /// `$1`, and nothing else does.
    ///
    /// `fetch_memory_page` binds the owner array and then the same list in
    /// the same order, so this is also the statement that the argument count
    /// matches — the two used to be separate hand-kept lists.
    #[test]
    fn every_filter_contributes_exactly_one_numbered_placeholder() {
        use super::MemoryFilter;
        let all = || {
            vec![
                MemoryFilter::Schema("s".to_owned()),
                MemoryFilter::Kind("fact"),
                MemoryFilter::Ids(Vec::new()),
                MemoryFilter::Cursor(uuid::Uuid::nil()),
            ]
        };
        for heads_only in [true, false] {
            for take in 0..=4 {
                let filters: Vec<MemoryFilter> = all().into_iter().take(take).collect();
                let sql = super::memory_page_sql(heads_only, &filters, 10);
                for n in 1..=take + 1 {
                    assert!(
                        sql.contains(&format!("${n}")),
                        "placeholder ${n} missing with {take} filters: {sql}"
                    );
                }
                assert!(
                    !sql.contains(&format!("${}", take + 2)),
                    "placeholder ${} emitted with only {take} filters: {sql}",
                    take + 2
                );
            }
        }
    }

    #[test]
    fn heads_only_schema_predicates_use_head_columns() {
        let sql = super::memory_page_sql(true, &[super::MemoryFilter::Schema("s".to_owned())], 10);
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
        let sql = super::memory_page_sql(false, &[super::MemoryFilter::Schema("s".to_owned())], 10);
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
