use std::collections::BTreeMap;
use std::fmt::Write as _;

use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::verbs::query::{
    EntityKind, MemorySearchRequest, MemorySearchResult, SearchMode, SearchOrder,
    SupersessionStatus, TagMatch,
};
use proxima_core::verbs::schema::{
    MemorySearchProjection, MemorySearchProjectionField, PayloadKind,
};
use proxima_core::{
    MemoryId, OwnerPrincipalKind, PersonalityInstanceId, Principal, SchemaId,
    SearchProjectionColumnKind, StorageError, WakeChainDepth,
};
use sqlx::PgPool;

use crate::error::internal;
use crate::pg_ident::PgIdent;

#[derive(Debug)]
struct SearchRow {
    memory_id: uuid::Uuid,
    kind: EntityKind,
    schema_id: String,
    authoring_personality_instance_id: Option<PersonalityInstanceId>,
    created_at: time::OffsetDateTime,
    snippet: String,
    lexical_score: f32,
    similarity_score: f32,
    wake_chain_depth: i16,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for SearchRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row as _;

        Ok(Self {
            memory_id: row.try_get("memory_id")?,
            kind: row.try_get("kind")?,
            schema_id: row.try_get("schema_id")?,
            authoring_personality_instance_id: decode_personality(
                row.try_get("authoring_personality_instance_id")?,
            ),
            created_at: row.try_get("created_at")?,
            snippet: row.try_get("snippet")?,
            lexical_score: row.try_get("lexical_score")?,
            similarity_score: row.try_get("similarity_score")?,
            wake_chain_depth: row.try_get("wake_chain_depth")?,
        })
    }
}

#[derive(Debug, Clone)]
struct Candidate {
    memory_id: uuid::Uuid,
    kind: EntityKind,
    schema_id: SchemaId,
    authoring_personality_instance_id: Option<PersonalityInstanceId>,
    created_at: time::OffsetDateTime,
    snippet: String,
    lexical_score: f32,
    similarity_score: f32,
    wake_chain_depth: WakeChainDepth,
}

#[derive(Debug, Clone, Copy)]
struct CandidateFilterParams {
    schema_filter: Option<usize>,
    reader: Option<usize>,
    since: Option<usize>,
    until: Option<usize>,
    tags: Option<usize>,
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
                authoring_personality_instance_id: candidate.authoring_personality_instance_id,
                created_at: candidate.created_at,
                snippet: candidate.snippet,
                score,
                lexical_score: candidate.lexical_score,
                similarity_score: candidate.similarity_score,
                wake_chain_depth: candidate.wake_chain_depth,
            }
        })
        .collect();

    match req.order {
        SearchOrder::Relevance => results.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| b.memory_id.into_inner().cmp(&a.memory_id.into_inner()))
        }),
        SearchOrder::Recency => results.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.memory_id.into_inner().cmp(&a.memory_id.into_inner()))
        }),
    }
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
            authoring_personality_instance_id: row.authoring_personality_instance_id,
            created_at: row.created_at,
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
    let order_by = branch_order_by(req, "lexical_score");

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
          SELECT c.memory_id, c.kind, c.schema_id, c.authoring_personality_instance_id,
                 c.created_at,
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
          ORDER BY {order_by}
          LIMIT {}",
        u64::from(limit),
        order_by = order_by
    )
    .expect("write to String is infallible");

    let mut q = bind_common(sqlx::query_as::<_, SearchRow>(&sql), req);
    for projection in &projections {
        q = q.bind(projection.schema_id.as_str().to_string());
        q = q.bind(projection.schema_version.into_inner().cast_signed());
    }
    q = bind_filter_params(q, req);
    q = q.bind(req.query.clone());
    q.fetch_all(pool).await.map_err(internal)
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
    if query_embedding.len() != EMBEDDING_DIM {
        return Err(StorageError::ConstraintViolation(
            "semantic search embedding length must be 1024".into(),
        ));
    }

    let projections = memory_search_projections(req, projections);
    let mut next_param = 3;
    let mut sql = common_candidates_sql(req, &projections, &mut next_param)?;
    let vec_param = next_param;
    let model_param = next_param + 1;
    let order_by = branch_order_by(req, "similarity_score");

    write!(
        sql,
        " SELECT c.memory_id, c.kind, c.schema_id, c.authoring_personality_instance_id,
                 c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 CASE
                     WHEN (1 - (emb.vec <=> ${vec_param}::vector)) = 'NaN'::float8 THEN 0.0
                     ELSE GREATEST(0.0, (1 - (emb.vec <=> ${vec_param}::vector)))
                 END::real AS similarity_score,
                 c.wake_chain_depth
          FROM candidates c
          JOIN proxima_core.embeddings emb
            ON emb.entity_kind = c.kind
           AND emb.entity_id = c.memory_id
           AND emb.owner_principal_kind = c.owner_principal_kind
           AND emb.owner_principal_id = c.owner_principal_id
           AND emb.embedding_version = 1
           AND emb.model_id = ${model_param}
          ORDER BY {order_by}
          LIMIT {}",
        u64::from(limit),
        order_by = order_by
    )
    .expect("write to String is infallible");

    let mut q = bind_common(sqlx::query_as::<_, SearchRow>(&sql), req);
    for projection in &projections {
        q = q.bind(projection.schema_id.as_str().to_string());
        q = q.bind(projection.schema_version.into_inner().cast_signed());
    }
    q = bind_filter_params(q, req);
    q = q.bind(crate::pgvector::literal(query_embedding));
    q = q.bind(model_id.clone());
    q.fetch_all(pool).await.map_err(internal)
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
    let since_param = req.since.map(|_| {
        let param = *next_param;
        *next_param += 1;
        param
    });
    let until_param = req.until.map(|_| {
        let param = *next_param;
        *next_param += 1;
        param
    });
    let tags_param = (!req.tags.is_empty()).then(|| {
        let param = *next_param;
        *next_param += 1;
        param
    });
    let filters = CandidateFilterParams {
        schema_filter: schema_filter_param,
        reader: reader_param,
        since: since_param,
        until: until_param,
        tags: tags_param,
    };

    let mut sql = String::from("WITH candidates AS (");
    push_candidate_branch_prefix(&mut sql);
    sql.push_str(
        "NULL::text[] AS tags, COALESCE(m.text, '') AS search_text \
         FROM proxima_core.memories m \
         LEFT JOIN proxima_core.entity_owner home_owner \
           ON home_owner.entity_id = m.memory_id \
          AND home_owner.is_home",
    );
    push_base_memory_filters(&mut sql, req, filters);
    sql.push_str(" AND NULLIF(m.text, '') IS NOT NULL");

    for (idx, projection) in projections.iter().enumerate() {
        let table = PgIdent::table(&projection.sidecar_table)?;
        let projection_expr = projection_search_expr(&projection.fields)?;
        let tag_expr = projection_tag_expr(projection)?;
        let schema_param = sidecar_first_param + (idx * 2);
        let version_param = schema_param + 1;
        sql.push_str(" UNION ALL ");
        push_candidate_branch_prefix(&mut sql);
        write!(
            sql,
            "{tag_expr} AS tags,
             NULLIF(concat_ws(' ', {projection_expr}), '') AS search_text
             FROM proxima_core.memories m
             LEFT JOIN proxima_core.entity_owner home_owner
               ON home_owner.entity_id = m.memory_id
              AND home_owner.is_home
             JOIN {table} s ON s.memory_id = m.memory_id",
            tag_expr = tag_expr.as_str(),
            projection_expr = projection_expr,
            table = table.as_str()
        )
        .expect("write to String is infallible");
        push_sidecar_memory_filters(
            &mut sql,
            req,
            projection.kind,
            schema_param,
            version_param,
            filters,
            &tag_expr,
        );
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

fn projection_tag_expr(projection: &MemorySearchProjection) -> Result<String, StorageError> {
    let Some(tag_column) = &projection.tag_column else {
        return Ok("NULL::text[]".to_string());
    };
    let column = PgIdent::column(tag_column)?;
    Ok(format!("s.{}", column.as_str()))
}

fn push_candidate_branch_prefix(sql: &mut String) {
    sql.push_str(
        "SELECT m.memory_id, home_owner.owner_principal_kind, home_owner.owner_principal_id, \
         COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, \
         m.schema_id, m.personality_instance_id AS authoring_personality_instance_id, \
         m.wake_chain_depth, m.created_at, ",
    );
}

fn decode_personality(instance_id: Option<uuid::Uuid>) -> Option<PersonalityInstanceId> {
    instance_id
        .filter(|id| !id.is_nil())
        .map(PersonalityInstanceId::new)
}

fn push_base_memory_filters(
    sql: &mut String,
    req: &MemorySearchRequest,
    filters: CandidateFilterParams,
) {
    sql.push_str(
        " WHERE EXISTS (
              SELECT 1
                FROM proxima_core.entity_owner eo
                JOIN unnest($1::proxima_core.owner_principal_kind[], $2::uuid[]) AS s(kind, id)
                  ON eo.owner_principal_kind = s.kind
                 AND eo.owner_principal_id = s.id
               WHERE eo.entity_id = m.memory_id
           )
           AND m.tombstoned_at IS NULL",
    );
    match req.kind {
        None => {}
        Some(EntityKind::Fact) => sql.push_str(" AND m.kind IS NULL"),
        Some(EntityKind::Abstraction) => sql.push_str(" AND m.kind = 'Abstraction'"),
        Some(EntityKind::Perspective) => sql.push_str(" AND m.kind = 'Perspective'"),
        Some(EntityKind::Goal) => sql.push_str(" AND false"),
    }
    if let Some(param) = filters.schema_filter {
        write!(sql, " AND m.schema_id = ${param}").expect("write to String is infallible");
    }
    push_time_filters(sql, filters);
    push_tag_filter(sql, req, filters.tags, "NULL::text[]");
    push_search_head_filter(sql, req);
    push_reader_visibility_filter(sql, filters.reader);
}

fn push_sidecar_memory_filters(
    sql: &mut String,
    req: &MemorySearchRequest,
    kind: PayloadKind,
    schema_param: usize,
    version_param: usize,
    filters: CandidateFilterParams,
    tag_expr: &str,
) {
    write!(
        sql,
        " WHERE EXISTS (
              SELECT 1
                FROM proxima_core.entity_owner eo
                JOIN unnest($1::proxima_core.owner_principal_kind[], $2::uuid[]) AS s(kind, id)
                  ON eo.owner_principal_kind = s.kind
                 AND eo.owner_principal_id = s.id
               WHERE eo.entity_id = m.memory_id
           )
           AND m.tombstoned_at IS NULL
           AND m.schema_id = ${schema_param}
           AND m.schema_version = ${version_param}"
    )
    .expect("write to String is infallible");
    push_payload_kind_filter(sql, kind);
    push_time_filters(sql, filters);
    push_tag_filter(sql, req, filters.tags, tag_expr);
    push_search_head_filter(sql, req);
    push_reader_visibility_filter(sql, filters.reader);
}

fn push_payload_kind_filter(sql: &mut String, kind: PayloadKind) {
    match kind {
        PayloadKind::Fact => sql.push_str(" AND m.kind IS NULL"),
        PayloadKind::Abstraction => sql.push_str(" AND m.kind = 'Abstraction'"),
        PayloadKind::Perspective => sql.push_str(" AND m.kind = 'Perspective'"),
        PayloadKind::Goal
        | PayloadKind::Edge
        | PayloadKind::CitedObject
        | PayloadKind::CitationMapping => sql.push_str(" AND false"),
    }
}

fn push_time_filters(sql: &mut String, filters: CandidateFilterParams) {
    if let Some(param) = filters.since {
        write!(sql, " AND m.created_at >= ${param}").expect("write to String is infallible");
    }
    if let Some(param) = filters.until {
        write!(sql, " AND m.created_at <= ${param}").expect("write to String is infallible");
    }
}

fn push_search_head_filter(sql: &mut String, req: &MemorySearchRequest) {
    if matches!(req.supersession, SupersessionStatus::IncludeSuperseded) {
        return;
    }
    sql.push_str(
        " AND ( \
            (m.kind IS NULL AND ( \
                m.fact_entity_id IS NULL \
                OR EXISTS ( \
                    SELECT 1 FROM proxima_core.fact_entities fe \
                     WHERE fe.fact_entity_id = m.fact_entity_id \
                       AND fe.current_memory_id = m.memory_id \
                ) \
            )) \
            OR (m.kind IS NOT NULL AND NOT EXISTS ( \
                SELECT 1 FROM proxima_core.memories m2 \
                 WHERE m2.supersedes = m.memory_id \
                   AND m2.tombstoned_at IS NULL \
            )) \
        )",
    );
}

fn push_tag_filter(
    sql: &mut String,
    req: &MemorySearchRequest,
    tag_param: Option<usize>,
    tag_expr: &str,
) {
    let Some(param) = tag_param else {
        return;
    };
    let op = match req.tag_match {
        TagMatch::Any => "&&",
        TagMatch::All => "@>",
    };
    write!(sql, " AND {tag_expr} {op} ${param}::text[]").expect("write to String is infallible");
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
	                     WHERE r.owner_principal_kind = home_owner.owner_principal_kind
	                       AND r.owner_principal_id = home_owner.owner_principal_id
	                       AND r.reader_personality_instance_id = ${param}
	                       AND r.readable_personality_instance_id = m.personality_instance_id
	                )
            )"
        )
        .expect("write to String is infallible");
    }
}

fn branch_order_by(req: &MemorySearchRequest, relevance_score_column: &str) -> String {
    match req.order {
        SearchOrder::Relevance => format!("{relevance_score_column} DESC, c.memory_id DESC"),
        SearchOrder::Recency => "c.created_at DESC, c.memory_id DESC".to_string(),
    }
}

fn bind_common<'q>(
    mut q: sqlx::query::QueryAs<'q, sqlx::Postgres, SearchRow, sqlx::postgres::PgArguments>,
    req: &'q MemorySearchRequest,
) -> sqlx::query::QueryAs<'q, sqlx::Postgres, SearchRow, sqlx::postgres::PgArguments> {
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(&req.read_owners);
    q = q.bind(read_owner_kinds);
    q = q.bind(read_owner_ids);
    q
}

fn read_owner_columns(read_owners: &[Principal]) -> (Vec<OwnerPrincipalKind>, Vec<uuid::Uuid>) {
    let kinds = read_owners
        .iter()
        .map(|principal| principal.columns().0)
        .collect();
    let ids = read_owners
        .iter()
        .map(|principal| principal.columns().1)
        .collect();
    (kinds, ids)
}

fn bind_filter_params<'q>(
    mut q: sqlx::query::QueryAs<'q, sqlx::Postgres, SearchRow, sqlx::postgres::PgArguments>,
    req: &'q MemorySearchRequest,
) -> sqlx::query::QueryAs<'q, sqlx::Postgres, SearchRow, sqlx::postgres::PgArguments> {
    if let Some(schema_id) = &req.schema_id {
        q = q.bind(schema_id.as_str().to_string());
    }
    if let Some(reader) = req.reader_personality_instance_id {
        q = q.bind(reader.into_inner());
    }
    if let Some(since) = req.since {
        q = q.bind(since);
    }
    if let Some(until) = req.until {
        q = q.bind(until);
    }
    if !req.tags.is_empty() {
        q = q.bind(req.tags.clone());
    }
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
