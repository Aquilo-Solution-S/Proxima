//! `core_search_memories` — sidecar-first content search.
//!
//! Same split as `proxima-code_search_chunks` (the reference):
//! 1. **content** — one GIN query per sidecar (`@@` only); `LIKE`
//!    runs only when that sidecar's GIN arm is empty
//! 2. **admit** — owner + optional current-head on the hit `t`s;
//!    `schema_id` is on `memory`
//! 3. **pins** — engine neighbor load, only if the caller asked
//!
//! Unscoped search (no tags) scans only `proxima_core.*` sidecars.
//! A tag filter is the documented flavor scope
//! (`docs/09-developing-flavors.md`): those queries also scan flavor
//! sidecars that declare a `tag_column`. A specialized flavor tool
//! (e.g. `proxima-code_search_chunks`) still owns extra filters;
//! unscoped core search is not a mega-index.

use std::collections::BTreeMap;

use futures_util::future::try_join_all;
use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::verbs::query::{
    DEFAULT_HYBRID_SEMANTIC_WEIGHT, EntityKind, MAX_SEARCH_PAGE_LIMIT, MemorySearchPage,
    MemorySearchRequest, MemorySearchResult, SearchCursor, SearchMode, SearchOrder,
    SupersessionStatus, TagMatch, like_pattern,
};
use proxima_core::verbs::schema::{MemorySearchProjection, PayloadKind};
use proxima_core::{MemoryId, OwnerRef, SchemaId, StorageError};
use sqlx::PgPool;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::pgvector::set_hnsw_search_sql;
use crate::tuning::PgTuning;
use crate::verbs::query::projection_sql::projection_search_text;

const CORE_SIDECAR_PREFIX: &str = "proxima_core.";
const SIDECAR_OVERFETCH_FACTOR: u32 = 20;
const SIDECAR_OVERFETCH_CAP: u32 = 1_000;

const SEMANTIC_SEARCH_SQL: &str = "SELECT emb.entity_id AS t,
                GREATEST(0.0, (1 - (emb.vec <=> $3::vector)))::real AS similarity_score
           FROM proxima_core.embeddings emb
           JOIN proxima_core.embedding_heads head
             ON head.entity_id = emb.entity_id
            AND head.model_id = emb.model_id
            AND head.embedding_version = emb.embedding_version
          WHERE emb.owner_id = ANY($1::uuid[])
            AND emb.model_id = $2
            AND ($5::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(emb.entity_id), TIMESTAMPTZ '1970-01-01') >= $5)
            AND ($6::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(emb.entity_id), TIMESTAMPTZ '1970-01-01') <= $6)
          ORDER BY emb.vec <=> $3::vector
          LIMIT $4";

#[derive(Debug, Clone)]
struct Hit {
    t: uuid::Uuid,
    lexical_score: f32,
    similarity_score: f32,
    snippet: String,
}

#[derive(Debug, sqlx::FromRow)]
struct SidecarScanRow {
    t: uuid::Uuid,
    lexical_score: f32,
    snippet: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct AdmitRow {
    t: uuid::Uuid,
    kind: String,
    schema_id: String,
    created_at: time::OffsetDateTime,
}

#[derive(Debug, sqlx::FromRow)]
struct EmbeddingScanRow {
    t: uuid::Uuid,
    similarity_score: f32,
}

pub(crate) async fn search_memories(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    tuning: &PgTuning,
) -> Result<MemorySearchPage, StorageError> {
    if matches!(req.kind, Some(EntityKind::Goal)) || req.limit == 0 {
        return Ok(MemorySearchPage {
            results: Vec::new(),
            has_more: false,
        });
    }
    if let Some(after) = req.after
        && after.order() != req.order
    {
        return Err(StorageError::ConstraintViolation(
            "search cursor order does not match request order".into(),
        ));
    }

    let limit = req.limit.min(MAX_SEARCH_PAGE_LIMIT);
    let overfetch = sidecar_overfetch(limit);
    let core_projections = core_search_projections(req, projections);

    let mut hits: BTreeMap<uuid::Uuid, Hit> = BTreeMap::new();
    match req.mode {
        SearchMode::Lexical => {
            merge_hits(
                &mut hits,
                scan_sidecars(pool, req, &core_projections, overfetch, true).await?,
            );
        }
        SearchMode::Semantic => {
            merge_hits(
                &mut hits,
                scan_embeddings(pool, req, tuning, overfetch).await?,
            );
        }
        SearchMode::Hybrid => {
            if req.query_embedding.is_some() && req.embedding_model_id.is_some() {
                let (lexical, semantic) = tokio::try_join!(
                    scan_sidecars(pool, req, &core_projections, overfetch, false),
                    scan_embeddings(pool, req, tuning, overfetch),
                )?;
                merge_hits(&mut hits, lexical);
                merge_hits(&mut hits, semantic);
            } else {
                merge_hits(
                    &mut hits,
                    scan_sidecars(pool, req, &core_projections, overfetch, true).await?,
                );
            }
        }
    }

    let admitted = admit_hits(pool, req, &hits).await?;
    Ok(page_hits(req, limit, admitted))
}

fn sidecar_overfetch(limit: u32) -> u32 {
    limit
        .saturating_mul(SIDECAR_OVERFETCH_FACTOR)
        .max(limit)
        .min(SIDECAR_OVERFETCH_CAP)
}

fn core_search_projections<'a>(
    req: &MemorySearchRequest,
    projections: &'a [MemorySearchProjection],
) -> Vec<&'a MemorySearchProjection> {
    let mut by_table = BTreeMap::<&str, &MemorySearchProjection>::new();
    for projection in projections {
        // Unscoped search stays on core sidecars. A tag filter is how a
        // flavor scopes `core_search_memories` (docs/09); those queries
        // must reach the flavor sidecar that declared `tag_column`.
        if !projection.sidecar_table.starts_with(CORE_SIDECAR_PREFIX) && req.tags.is_empty() {
            continue;
        }
        if !payload_kind_matches(req.kind, projection.kind) {
            continue;
        }
        if let Some(schema_id) = &req.schema_id
            && projection.schema_id != *schema_id
        {
            continue;
        }
        if !req.tags.is_empty() && projection.tag_column.is_none() {
            continue;
        }
        by_table
            .entry(projection.sidecar_table.as_str())
            .or_insert(projection);
    }
    by_table.into_values().collect()
}

fn payload_kind_matches(requested: Option<EntityKind>, kind: PayloadKind) -> bool {
    matches!(
        (requested, kind),
        (
            None,
            PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
        ) | (Some(EntityKind::Fact), PayloadKind::Fact)
            | (Some(EntityKind::Abstraction), PayloadKind::Abstraction)
            | (Some(EntityKind::Perspective), PayloadKind::Perspective)
    )
}

fn merge_hits(into: &mut BTreeMap<uuid::Uuid, Hit>, rows: Vec<Hit>) {
    for hit in rows {
        into.entry(hit.t)
            .and_modify(|existing| {
                existing.lexical_score = existing.lexical_score.max(hit.lexical_score);
                existing.similarity_score = existing.similarity_score.max(hit.similarity_score);
                if existing.snippet.is_empty() && !hit.snippet.is_empty() {
                    existing.snippet.clone_from(&hit.snippet);
                }
            })
            .or_insert(hit);
    }
}

async fn scan_sidecars(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projections: &[&MemorySearchProjection],
    overfetch: u32,
    rescue: bool,
) -> Result<Vec<Hit>, StorageError> {
    if projections.is_empty() {
        return Ok(Vec::new());
    }
    let like_pattern = like_pattern(&req.query);
    let jobs = projections.iter().copied().map(|projection| {
        let like_pattern = like_pattern.clone();
        async move {
            let gin = scan_one_sidecar(
                pool,
                req,
                projection,
                &like_pattern,
                overfetch,
                rescue,
                false,
            )
            .await?;
            if gin.is_empty() {
                scan_one_sidecar(
                    pool,
                    req,
                    projection,
                    &like_pattern,
                    overfetch,
                    rescue,
                    true,
                )
                .await
            } else {
                Ok(gin)
            }
        }
    });
    let batches = try_join_all(jobs).await?;
    Ok(batches.into_iter().flatten().collect())
}

#[allow(clippy::too_many_lines, clippy::fn_params_excessive_bools)]
async fn scan_one_sidecar(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projection: &MemorySearchProjection,
    like_pattern: &str,
    overfetch: u32,
    rescue: bool,
    like_only: bool,
) -> Result<Vec<Hit>, StorageError> {
    let recency_t = match req.after {
        Some(SearchCursor::Recency { memory_id, .. }) => Some(memory_id.into_inner()),
        _ => None,
    };
    let sql = lexical_sidecar_sql(projection, req, rescue, like_only)?;

    // SQL-POLICY: PgIdent
    let rows: Vec<SidecarScanRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(&req.query)
        .bind(like_pattern)
        .bind(i64::from(overfetch))
        .bind((!req.tags.is_empty()).then_some(req.tags.as_slice()))
        .bind(req.since)
        .bind(req.until)
        .bind(recency_t)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|row| Hit {
            t: row.t,
            lexical_score: row.lexical_score.max(0.0),
            similarity_score: 0.0,
            snippet: row.snippet.unwrap_or_default(),
        })
        .collect())
}

fn lexical_sidecar_sql(
    projection: &MemorySearchProjection,
    req: &MemorySearchRequest,
    rescue: bool,
    like_only: bool,
) -> Result<String, StorageError> {
    let table = PgIdent::table(&projection.sidecar_table)?;
    let search_text = projection_search_text(&projection.fields)?;
    let tsv_expr = projection_tsv_expr(projection, &search_text)?;
    let tag_pred = match projection.tag_column.as_deref() {
        Some(column) if !req.tags.is_empty() => {
            let column = PgIdent::column(column)?;
            let op = match req.tag_match {
                TagMatch::Any => "&&",
                TagMatch::All => "@>",
            };
            format!(" AND c.{} {op} $4::text[]", column.as_str())
        }
        _ => String::new(),
    };
    let multilingual = projection.language_column.is_some();
    let rank_tsq = rank_tsquery_expr(projection.language_column.as_deref())?;
    let rescue_score = if rescue && !like_only {
        format!(
            ", CASE WHEN q.any_tsq IS NOT NULL AND {tsv_expr} @@ q.any_tsq
                    THEN 0.25 + LEAST(COALESCE(ts_rank({tsv_expr}, q.any_tsq, 1|32), 0.0) * 100.0, 1.0) * 0.2
                    ELSE 0.0 END"
        )
    } else {
        String::new()
    };
    let rescue_where = if rescue && !like_only {
        format!(" OR (q.any_tsq IS NOT NULL AND {tsv_expr} @@ q.any_tsq)")
    } else {
        String::new()
    };
    let order_by = match req.order {
        SearchOrder::Relevance => "lexical_score DESC, c.t DESC",
        SearchOrder::Recency => "c.t DESC",
    };
    let match_pred = if like_only {
        format!("lower({search_text}) LIKE $2 ESCAPE '\\'")
    } else {
        format!("{tsv_expr} @@ q.tsq{rescue_where}")
    };
    let score_expr = if like_only {
        "0.25::real".to_string()
    } else {
        format!(
            "GREATEST(
                    CASE WHEN {tsv_expr} @@ q.tsq
                         THEN 0.5 + LEAST(COALESCE(ts_rank_cd({tsv_expr}, {rank_tsq}, 32), 0.0), 1.0) * 0.5
                         ELSE 0.0 END{rescue_score},
                    0.0
                )::real"
        )
    };
    Ok(format!(
        "{q_cte}
         SELECT c.t,
                {score_expr} AS lexical_score,
                left({search_text}, 480) AS snippet
           FROM {table} c, q
          WHERE ({match_pred})
            {tag_pred}
            AND length($2::text) >= 0
            AND ($5::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(c.t), TIMESTAMPTZ '1970-01-01') >= $5)
            AND ($6::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(c.t), TIMESTAMPTZ '1970-01-01') <= $6)
            AND ($7::uuid IS NULL OR c.t < $7)
          ORDER BY {order_by}
          LIMIT $3",
        q_cte = query_side_cte(multilingual),
        table = table.as_str(),
        search_text = search_text,
        tag_pred = tag_pred,
        order_by = order_by,
        match_pred = match_pred,
        score_expr = score_expr,
    ))
}

fn search_admit_sql(heads_only: bool) -> String {
    let from = if heads_only {
        "FROM proxima_core.memory_head h \
         JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t"
    } else {
        "FROM proxima_core.memory m"
    };
    format!(
        "SELECT m.t,
                m.kind::text,
                m.schema_id,
                COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01') AS created_at
           {from}
          WHERE m.t = ANY($1::uuid[])
            AND m.owner_id = ANY($2::uuid[])
            AND ($3::text IS NULL OR m.kind::text = $3)
            AND ($4::text IS NULL OR m.schema_id = $4)
            AND ($5::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01') >= $5)
            AND ($6::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01') <= $6)"
    )
}

fn projection_tsv_expr(
    projection: &MemorySearchProjection,
    search_text: &str,
) -> Result<String, StorageError> {
    if let Some(column) = &projection.tsv_column {
        let column = PgIdent::column(column)?;
        return Ok(format!("c.{}", column.as_str()));
    }
    if let Some(language) = &projection.language_column {
        let language = PgIdent::column(language)?;
        return Ok(format!(
            "proxima_core.lexical_tsv(c.{}, {search_text})",
            language.as_str()
        ));
    }
    Ok(format!(
        "proxima_core.lexical_tsv(proxima_core.lexical_config(), {search_text})"
    ))
}

/// Match-side tsquery. When the sidecar declares `language_column` (agent
/// memory tables), OR one `websearch_to_tsquery` per `lexical_languages`
/// row — the query cannot know its language. Flavor tables that pin one
/// config omit `language_column` and stay on `lexical_config()`.
fn query_side_cte(multilingual: bool) -> &'static str {
    if multilingual {
        "WITH q AS (
             SELECT s.q AS scrubbed,
                    COALESCE(
                        (SELECT proxima_core.tsquery_or_agg(
                                    websearch_to_tsquery(l.config,
                                        proxima_core.lexical_query_text(l.config, s.q))
                                    ORDER BY l.config)
                           FROM proxima_core.lexical_languages l),
                        websearch_to_tsquery(proxima_core.lexical_config(), s.q)
                    ) AS tsq,
                    COALESCE(
                        (SELECT proxima_core.tsquery_or_agg(
                                    NULLIF(
                                        replace(
                                            plainto_tsquery(l.config,
                                                proxima_core.lexical_query_text(l.config, s.q))::text,
                                            ' & ', ' | '),
                                        '')::tsquery
                                    ORDER BY l.config)
                           FROM proxima_core.lexical_languages l),
                        NULLIF(
                            replace(
                                plainto_tsquery(proxima_core.lexical_config(), s.q)::text,
                                ' & ', ' | '),
                            '')::tsquery
                    ) AS any_tsq
               FROM (SELECT proxima_core.lexical_scrub($1) AS q) s
         )"
    } else {
        "WITH q AS (
             SELECT proxima_core.lexical_scrub($1) AS scrubbed,
                    websearch_to_tsquery(proxima_core.lexical_config(),
                        proxima_core.lexical_scrub($1)) AS tsq,
                    NULLIF(
                        replace(
                            plainto_tsquery(proxima_core.lexical_config(),
                                proxima_core.lexical_scrub($1))::text,
                            ' & ', ' | '),
                        '')::tsquery AS any_tsq
         )"
    }
}

fn rank_tsquery_expr(language_column: Option<&str>) -> Result<String, StorageError> {
    let Some(column) = language_column else {
        return Ok("q.tsq".into());
    };
    let column = PgIdent::column(column)?;
    Ok(format!(
        "websearch_to_tsquery(c.{col}, proxima_core.lexical_query_text(c.{col}, q.scrubbed))",
        col = column.as_str()
    ))
}

async fn scan_embeddings(
    pool: &PgPool,
    req: &MemorySearchRequest,
    tuning: &PgTuning,
    overfetch: u32,
) -> Result<Vec<Hit>, StorageError> {
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
        return Err(StorageError::ConstraintViolation(format!(
            "semantic search embedding length must be {EMBEDDING_DIM}"
        )));
    }
    let owner_ids: Vec<uuid::Uuid> = req
        .read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();

    let mut tx = pool.begin().await.map_err(map_err)?;
    // SQL-POLICY: fixed-fragment
    sqlx::raw_sql(sqlx::AssertSqlSafe(set_hnsw_search_sql(tuning)))
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    let rows: Vec<EmbeddingScanRow> = sqlx::query_as(SEMANTIC_SEARCH_SQL)
        .bind(&owner_ids)
        .bind(model_id)
        .bind(crate::pgvector::literal(query_embedding))
        .bind(i64::from(overfetch))
        .bind(req.since)
        .bind(req.until)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|row| Hit {
            t: row.t,
            lexical_score: 0.0,
            similarity_score: row.similarity_score.clamp(0.0, 1.0),
            snippet: String::new(),
        })
        .collect())
}

async fn admit_hits(
    pool: &PgPool,
    req: &MemorySearchRequest,
    hits: &BTreeMap<uuid::Uuid, Hit>,
) -> Result<Vec<MemorySearchResult>, StorageError> {
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    let owner_ids: Vec<uuid::Uuid> = req
        .read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let hit_ts: Vec<uuid::Uuid> = hits.keys().copied().collect();
    let kind_filter = match req.kind {
        Some(EntityKind::Fact) => Some("fact"),
        Some(EntityKind::Abstraction) => Some("abstraction"),
        Some(EntityKind::Perspective) => Some("perspective"),
        Some(EntityKind::Goal) | None => None,
    };
    let schema_filter = req.schema_id.as_ref().map(SchemaId::as_str);
    let sql = search_admit_sql(matches!(req.supersession, SupersessionStatus::HeadsOnly));

    // SQL-POLICY: fixed-fragment
    let rows: Vec<AdmitRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(&hit_ts)
        .bind(&owner_ids)
        .bind(kind_filter)
        .bind(schema_filter)
        .bind(req.since)
        .bind(req.until)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;

    let semantic_weight = req
        .semantic_weight
        .unwrap_or(DEFAULT_HYBRID_SEMANTIC_WEIGHT)
        .clamp(0.0, 1.0);
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let hit = hits.get(&row.t)?;
            let kind = parse_kind(&row.kind)?;
            let score = match req.mode {
                SearchMode::Lexical => hit.lexical_score,
                SearchMode::Semantic => hit.similarity_score,
                SearchMode::Hybrid => {
                    (semantic_weight * hit.similarity_score)
                        + ((1.0 - semantic_weight) * hit.lexical_score)
                }
            };
            Some(MemorySearchResult {
                memory_id: MemoryId::new(row.t),
                kind,
                schema_id: SchemaId::new(row.schema_id),
                created_at: row.created_at,
                snippet: hit.snippet.clone(),
                score,
                lexical_score: hit.lexical_score,
                similarity_score: hit.similarity_score,
            })
        })
        .collect())
}

fn page_hits(
    req: &MemorySearchRequest,
    limit: u32,
    mut results: Vec<MemorySearchResult>,
) -> MemorySearchPage {
    if let Some(floor) = req.min_score {
        results.retain(|result| result.score >= floor);
    }
    if let Some(after) = req.after {
        results.retain(|result| ranks_after_cursor(result, after));
    }
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
    let page_len = usize::try_from(limit).unwrap_or(usize::MAX);
    let has_more = results.len() > page_len;
    results.truncate(page_len);
    MemorySearchPage { results, has_more }
}

fn ranks_after_cursor(result: &MemorySearchResult, after: SearchCursor) -> bool {
    match after {
        SearchCursor::Relevance {
            score_bits,
            memory_id,
            ..
        } => match result.score.total_cmp(&f32::from_bits(score_bits)) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Equal => result.memory_id.into_inner() < memory_id.into_inner(),
            std::cmp::Ordering::Greater => false,
        },
        SearchCursor::Recency {
            created_at,
            memory_id,
            ..
        } => {
            (result.created_at, result.memory_id.into_inner())
                < (created_at, memory_id.into_inner())
        }
    }
}

#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn lexical_sidecar_sql_for_tests(
    projection: &MemorySearchProjection,
    req: &MemorySearchRequest,
    rescue: bool,
    like_only: bool,
) -> Result<String, StorageError> {
    lexical_sidecar_sql(projection, req, rescue, like_only)
}

#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn semantic_search_sql_for_tests() -> &'static str {
    SEMANTIC_SEARCH_SQL
}

#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn search_admit_sql_for_tests(heads_only: bool) -> String {
    search_admit_sql(heads_only)
}

fn parse_kind(kind: &str) -> Option<EntityKind> {
    match kind {
        "fact" => Some(EntityKind::Fact),
        "abstraction" => Some(EntityKind::Abstraction),
        "perspective" => Some(EntityKind::Perspective),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn admit_reads_schema_from_memory() {
        let src = include_str!("search.rs");
        let join = format!(
            "{}{}",
            "JOIN proxima_core.memory_head h ON h.handle = ", "m.handle\""
        );
        assert!(
            !src.contains(&join),
            "W5: non-HeadsOnly admit does not join head for schema_id"
        );
        assert!(
            src.contains("m.schema_id"),
            "admit selects memory.schema_id"
        );
    }

    #[test]
    fn production_search_uses_exported_builders() {
        let src = include_str!("search.rs");
        let prod = src.split("mod tests").next().expect("production");
        assert!(
            prod.contains("lexical_sidecar_sql(projection, req, rescue, like_only)"),
            "GIN/LIKE scan must run the exported builder"
        );
        assert!(
            prod.contains("query_as(SEMANTIC_SEARCH_SQL)"),
            "semantic scan must run SEMANTIC_SEARCH_SQL"
        );
        let admit = format!("{}{}", "search_admit_sql(matches!(", "");
        assert!(prod.contains(&admit), "admit must run search_admit_sql");
    }
}
