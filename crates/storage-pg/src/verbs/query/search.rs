use std::collections::BTreeMap;
use std::fmt::Write as _;

use proxima_core::verbs::query::{EntityKind, MemorySearchRequest, MemorySearchResult, SearchMode};
use proxima_core::verbs::schema::{
    MemorySearchProjection, MemorySearchProjectionField, PayloadKind,
};
use proxima_core::{
    MemoryId, OwnerPrincipalKind, Principal, SchemaId, SearchProjectionColumnKind, StorageError,
    WakeChainDepth,
};
use sqlx::PgPool;

use crate::pg_ident::PgIdent;

#[derive(Debug, sqlx::FromRow)]
struct SearchRow {
    memory_id: uuid::Uuid,
    kind: EntityKind,
    schema_id: String,
    snippet: String,
    lexical_score: f32,
    similarity_score: f32,
    wake_chain_depth: i16,
}

#[derive(Debug, Clone)]
struct Candidate {
    memory_id: uuid::Uuid,
    kind: EntityKind,
    schema_id: SchemaId,
    snippet: String,
    lexical_score: f32,
    similarity_score: f32,
    wake_chain_depth: WakeChainDepth,
}

pub(crate) async fn search_memories(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
) -> Result<Vec<MemorySearchResult>, StorageError> {
    if matches!(req.kind, Some(EntityKind::Goal)) || req.limit == 0 {
        return Ok(Vec::new());
    }

    let limit = req.limit.min(50);
    let mut candidates = BTreeMap::<uuid::Uuid, Candidate>::new();

    if matches!(req.mode, SearchMode::Lexical | SearchMode::Hybrid) {
        for row in run_lexical(pool, req, projections, limit.saturating_mul(4)).await? {
            merge_row(&mut candidates, row);
        }
    }

    if matches!(req.mode, SearchMode::Semantic | SearchMode::Hybrid) {
        for row in run_semantic(pool, req, projections, limit.saturating_mul(4)).await? {
            merge_row(&mut candidates, row);
        }
    }

    let mut results: Vec<MemorySearchResult> = candidates
        .into_values()
        .map(|candidate| {
            let score = match req.mode {
                SearchMode::Lexical => candidate.lexical_score,
                SearchMode::Semantic => candidate.similarity_score,
                SearchMode::Hybrid => {
                    (0.6 * candidate.similarity_score) + (0.4 * candidate.lexical_score)
                }
            };
            MemorySearchResult {
                memory_id: MemoryId::new(candidate.memory_id),
                kind: candidate.kind,
                schema_id: candidate.schema_id,
                snippet: candidate.snippet,
                score,
                lexical_score: candidate.lexical_score,
                similarity_score: candidate.similarity_score,
                wake_chain_depth: candidate.wake_chain_depth,
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.memory_id.into_inner().cmp(&a.memory_id.into_inner()))
    });
    results.truncate(usize::try_from(limit).unwrap_or(50));
    Ok(results)
}

fn merge_row(candidates: &mut BTreeMap<uuid::Uuid, Candidate>, row: SearchRow) {
    let entry = candidates
        .entry(row.memory_id)
        .or_insert_with(|| Candidate {
            memory_id: row.memory_id,
            kind: row.kind,
            schema_id: SchemaId::new(row.schema_id.clone()),
            snippet: row.snippet.clone(),
            lexical_score: 0.0,
            similarity_score: 0.0,
            wake_chain_depth: WakeChainDepth::new(u16::try_from(row.wake_chain_depth).unwrap_or(0)),
        });
    entry.lexical_score = entry.lexical_score.max(row.lexical_score.max(0.0));
    entry.similarity_score = entry
        .similarity_score
        .max(row.similarity_score.clamp(0.0, 1.0));
    if entry.snippet.is_empty() && !row.snippet.is_empty() {
        entry.snippet = row.snippet;
    }
}

async fn run_lexical(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    limit: u32,
) -> Result<Vec<SearchRow>, StorageError> {
    let projections = memory_search_projections(req, projections);
    let mut next_param = 3;
    let mut sql = common_candidates_sql(req, &projections, &mut next_param)?;
    let query_param = next_param;

    write!(
        sql,
        " , indexed AS (
              SELECT c.*,
                     regexp_replace(
                         regexp_replace(c.search_text, '[[:punct:]]+', ' ', 'g'),
                         '\\m[[:alnum:]]{{255}}[[:alnum:]]+\\M',
                         ' ',
                         'g'
                     ) AS index_text
                FROM candidates c
          )
          SELECT c.memory_id, c.kind, c.schema_id,
                 left(c.search_text, 480) AS snippet,
                 GREATEST(
                     LEAST(ts_rank_cd(to_tsvector('simple', c.index_text), q.tsq) * 10.0, 1.0),
                     CASE WHEN lower(c.search_text) LIKE '%' || lower(${query_param}) || '%'
                          THEN 0.25 ELSE 0.0 END
                 )::real AS lexical_score,
                 0.0::real AS similarity_score,
                 c.wake_chain_depth
          FROM indexed c,
               (SELECT websearch_to_tsquery(
                   'simple',
                   regexp_replace(
                       regexp_replace(${query_param}, '[[:punct:]]+', ' ', 'g'),
                       '\\m[[:alnum:]]{{255}}[[:alnum:]]+\\M',
                       ' ',
                       'g'
                   )
               ) AS tsq) q
          WHERE c.search_text <> ''
            AND (
                to_tsvector('simple', c.index_text) @@ q.tsq
                OR lower(c.search_text) LIKE '%' || lower(${query_param}) || '%'
            )
          ORDER BY lexical_score DESC, c.memory_id DESC
          LIMIT {}",
        u64::from(limit)
    )
    .expect("write to String is infallible");

    let mut q = bind_common(sqlx::query_as::<_, SearchRow>(&sql), req);
    for projection in &projections {
        q = q.bind(projection.schema_id.as_str().to_string());
        q = q.bind(projection.schema_version.into_inner().cast_signed());
    }
    if let Some(schema_id) = &req.schema_id {
        q = q.bind(schema_id.as_str().to_string());
    }
    if let Some(reader) = req.reader_personality_instance_id {
        q = q.bind(reader.into_inner());
    }
    q = q.bind(req.query.clone());
    q.fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))
}

async fn run_semantic(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    limit: u32,
) -> Result<Vec<SearchRow>, StorageError> {
    let Some(query_embedding) = req.query_embedding.as_ref() else {
        return Err(StorageError::ConstraintViolation(
            "semantic search requires query_embedding".into(),
        ));
    };
    let Some(model_id) = req.embedding_model_id.as_ref() else {
        return Err(StorageError::ConstraintViolation(
            "semantic search requires embedding_model_id".into(),
        ));
    };
    let Some(dim) = req.embedding_dim else {
        return Err(StorageError::ConstraintViolation(
            "semantic search requires embedding_dim".into(),
        ));
    };
    if query_embedding.len() != dim {
        return Err(StorageError::ConstraintViolation(
            "semantic search embedding length must match embedding_dim".into(),
        ));
    }

    let projections = memory_search_projections(req, projections);
    let mut next_param = 3;
    let mut sql = common_candidates_sql(req, &projections, &mut next_param)?;
    let vec_param = next_param;
    next_param += 1;
    let model_param = next_param;
    next_param += 1;
    let dim_param = next_param;

    write!(
        sql,
        " SELECT c.memory_id, c.kind, c.schema_id,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 GREATEST(0.0, sim.similarity)::real AS similarity_score,
                 c.wake_chain_depth
          FROM candidates c
          JOIN proxima_core.embeddings e
            ON e.entity_kind = c.kind
           AND e.entity_id = c.memory_id
           AND e.owner_principal_kind = c.owner_principal_kind
           AND e.owner_principal_id = c.owner_principal_id
           AND e.embedding_version = 1
           AND e.model_id = ${model_param}
           AND e.dim = ${dim_param}
          CROSS JOIN LATERAL (
              SELECT COALESCE(
                  SUM(pair.ev * pair.qv)
                  / NULLIF(
                      sqrt(SUM(pair.ev * pair.ev)) * sqrt(SUM(pair.qv * pair.qv)),
                      0.0
                  ),
                  0.0
              )::real AS similarity
              FROM unnest(e.vec, ${vec_param}::real[]) AS pair(ev, qv)
          ) sim
          ORDER BY similarity_score DESC, c.memory_id DESC
          LIMIT {}",
        u64::from(limit)
    )
    .expect("write to String is infallible");

    let mut q = bind_common(sqlx::query_as::<_, SearchRow>(&sql), req);
    for projection in &projections {
        q = q.bind(projection.schema_id.as_str().to_string());
        q = q.bind(projection.schema_version.into_inner().cast_signed());
    }
    if let Some(schema_id) = &req.schema_id {
        q = q.bind(schema_id.as_str().to_string());
    }
    if let Some(reader) = req.reader_personality_instance_id {
        q = q.bind(reader.into_inner());
    }
    q = q.bind(query_embedding.clone());
    q = q.bind(model_id.clone());
    q = q.bind(i32::try_from(dim).unwrap_or(i32::MAX));
    q.fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))
}

fn common_candidates_sql(
    req: &MemorySearchRequest,
    projections: &[&MemorySearchProjection],
    next_param: &mut usize,
) -> Result<String, StorageError> {
    let sidecar_first_param = *next_param;
    *next_param += projections.len() * 2;
    let schema_filter_param = req.schema_id.as_ref().map(|_| {
        let param = *next_param;
        *next_param += 1;
        param
    });
    let reader_param = req.reader_personality_instance_id.map(|_| {
        let param = *next_param;
        *next_param += 1;
        param
    });

    let mut sql = String::from("WITH candidates AS (");
    push_candidate_branch_prefix(&mut sql);
    sql.push_str("COALESCE(m.text, '') AS search_text FROM proxima_core.memories m");
    push_base_memory_filters(&mut sql, req, schema_filter_param, reader_param);
    sql.push_str(" AND NULLIF(m.text, '') IS NOT NULL");

    for (idx, projection) in projections.iter().enumerate() {
        let table = PgIdent::table(&projection.sidecar_table)?;
        let projection_expr = projection_search_expr(&projection.fields)?;
        let schema_param = sidecar_first_param + (idx * 2);
        let version_param = schema_param + 1;
        sql.push_str(" UNION ALL ");
        push_candidate_branch_prefix(&mut sql);
        write!(
            sql,
            "NULLIF(concat_ws(' ', {projection_expr}), '') AS search_text
             FROM proxima_core.memories m
             JOIN {table} s ON s.memory_id = m.memory_id",
            projection_expr = projection_expr,
            table = table.as_str()
        )
        .expect("write to String is infallible");
        push_sidecar_memory_filters(&mut sql, schema_param, version_param, reader_param);
    }

    sql.push(')');
    Ok(sql)
}

fn projection_search_expr(fields: &[MemorySearchProjectionField]) -> Result<String, StorageError> {
    let mut expressions = Vec::with_capacity(fields.len());
    for field in fields {
        let column = PgIdent::column(&field.column)?;
        let expression = match field.kind {
            SearchProjectionColumnKind::Text => {
                format!("NULLIF(s.{}::text, '')", column.as_str())
            }
            SearchProjectionColumnKind::TextArray => {
                format!("NULLIF(array_to_string(s.{}, ' '), '')", column.as_str())
            }
        };
        expressions.push(expression);
    }
    Ok(expressions.join(", "))
}

fn push_candidate_branch_prefix(sql: &mut String) {
    sql.push_str(
        "SELECT m.memory_id, m.owner_principal_kind, m.owner_principal_id, \
         COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, \
         m.schema_id, m.wake_chain_depth, ",
    );
}

fn push_base_memory_filters(
    sql: &mut String,
    req: &MemorySearchRequest,
    schema_filter_param: Option<usize>,
    reader_param: Option<usize>,
) {
    sql.push_str(
        " WHERE m.owner_principal_kind = $1
           AND m.owner_principal_id = $2",
    );
    match req.kind {
        None => {}
        Some(EntityKind::Fact) => sql.push_str(" AND m.kind IS NULL"),
        Some(EntityKind::Abstraction) => sql.push_str(" AND m.kind = 'Abstraction'"),
        Some(EntityKind::Perspective) => sql.push_str(" AND m.kind = 'Perspective'"),
        Some(EntityKind::Goal) => sql.push_str(" AND false"),
    }
    if let Some(param) = schema_filter_param {
        write!(sql, " AND m.schema_id = ${param}").expect("write to String is infallible");
    }
    push_reader_visibility_filter(sql, reader_param);
}

fn push_sidecar_memory_filters(
    sql: &mut String,
    schema_param: usize,
    version_param: usize,
    reader_param: Option<usize>,
) {
    write!(
        sql,
        " WHERE m.owner_principal_kind = $1
           AND m.owner_principal_id = $2
           AND m.schema_id = ${schema_param}
           AND m.schema_version = ${version_param}"
    )
    .expect("write to String is infallible");
    push_reader_visibility_filter(sql, reader_param);
}

fn push_reader_visibility_filter(sql: &mut String, reader_param: Option<usize>) {
    if let Some(param) = reader_param {
        write!(
            sql,
            " AND (
                m.kind IS NULL
                OR m.personality_instance_id = ${param}
                OR EXISTS (
                    SELECT 1
                      FROM proxima_core.read_scope_matrix r
                     WHERE r.owner_principal_kind = m.owner_principal_kind
                       AND r.owner_principal_id = m.owner_principal_id
                       AND r.owner_org_id = m.owner_org_id
                       AND r.reader_personality_instance_id = ${param}
                       AND r.readable_personality_instance_id = m.personality_instance_id
                )
            )"
        )
        .expect("write to String is infallible");
    }
}

fn bind_common<'q>(
    mut q: sqlx::query::QueryAs<'q, sqlx::Postgres, SearchRow, sqlx::postgres::PgArguments>,
    req: &'q MemorySearchRequest,
) -> sqlx::query::QueryAs<'q, sqlx::Postgres, SearchRow, sqlx::postgres::PgArguments> {
    let (owner_kind, owner_principal_id) = match &req.owner.principal {
        Principal::User(user) => (OwnerPrincipalKind::User, user.into_inner()),
        Principal::Group(group) => (OwnerPrincipalKind::Group, group.into_inner()),
    };
    q = q.bind(owner_kind);
    q = q.bind(owner_principal_id);
    q
}

fn memory_search_projections<'a>(
    req: &MemorySearchRequest,
    projections: &'a [MemorySearchProjection],
) -> Vec<&'a MemorySearchProjection> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for projection in projections {
        if matches!(
            projection.kind,
            PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
        ) && req
            .kind
            .is_none_or(|kind| projection.kind == payload_kind_for_entity_kind(kind))
            && req
                .schema_id
                .as_ref()
                .is_none_or(|schema_id| projection.schema_id == *schema_id)
        {
            let key = (
                projection.kind,
                projection.schema_id.as_str().to_string(),
                projection.schema_version.into_inner(),
                projection.sidecar_table.clone(),
            );
            if seen.insert(key) {
                out.push(projection);
            }
        }
    }
    out
}

fn payload_kind_for_entity_kind(kind: EntityKind) -> PayloadKind {
    match kind {
        EntityKind::Fact => PayloadKind::Fact,
        EntityKind::Abstraction => PayloadKind::Abstraction,
        EntityKind::Perspective => PayloadKind::Perspective,
        EntityKind::Goal => PayloadKind::Goal,
    }
}
