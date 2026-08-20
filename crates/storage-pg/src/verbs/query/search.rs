//! `core_search_memories` — projection-first content search.
//!
//! Same split as `proxima-code_search_chunks` (the reference):
//! 1. **content** — one GIN query per projected schema against its
//!    flavor's projection table (`@@` only); `LIKE` runs against the
//!    owning sidecar only when that schema's GIN arm is empty
//! 2. **admit** — owner + optional current-head on the hit `t`s;
//!    `schema_id` is on `memory`. The projection's `owner_id` is an
//!    index accelerator; the scan still joins `memory` and filters
//!    `owner_id` there, so authorization never rests on the copy.
//! 3. **pins** — engine neighbor load, only if the caller asked
//!
//! The ranked arm reads the projection ALONE — vector, tags and language
//! are all on it — and joins the owning sidecar for the surviving top-k
//! rows only, to render the snippet (R6: nothing is materialized twice).
//!
//! Unscoped search (no tags) scans only flavor #0's schemas.
//! A tag filter is the documented flavor scope
//! (`docs/09-developing-flavors.md`): those queries also scan flavor
//! schemas that declare a `tag_column`. A specialized flavor tool
//! (e.g. `proxima-code_search_chunks`) still owns extra filters;
//! unscoped core search is not a mega-index.

use std::collections::BTreeMap;

use futures_util::future::try_join_all;
use proxima_core::flavor::LanguagePolicy;
use proxima_core::flavor::{BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING, Band};
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
use crate::projection::{sidecar_text_sql, snippet_sql};
use crate::tuning::PgTuning;

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
    let mut by_schema = BTreeMap::<&str, &MemorySearchProjection>::new();
    for projection in projections {
        // Unscoped search stays on flavor #0's sidecars. A tag filter is
        // how a flavor scopes `core_search_memories` (docs/09); those
        // queries must reach the flavor sidecar that declared `tag_column`.
        //
        // "Core" is the ordinal, asked of the contract. It used to be
        // `starts_with("proxima_core.")` — a schema name standing in for a
        // flavor identity, true by accident and satisfiable by any flavor
        // that picked the same schema.
        if !proxima_core::FLAVOR_0.declares_sidecar_table(&projection.sidecar_table)
            && req.tags.is_empty()
        {
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
        // Keyed by schema, not by sidecar table: the projection row is
        // keyed `(memory_id, schema_id)` and the scan filters `schema_id`,
        // so a table shared by two schemas must be scanned for both. It
        // used to be keyed by table because the scan WAS the table.
        by_schema
            .entry(projection.schema_id.as_str())
            .or_insert(projection);
    }
    by_schema.into_values().collect()
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
    let owner_ids: Vec<uuid::Uuid> = req
        .read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
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
        .bind(&owner_ids)
        .bind(projection.schema_id.as_str())
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

/// A band as SQL renders it: the floor, and the width a normalized rank is
/// scaled by to fill the window.
///
/// Rendered at two decimals rather than through `f32`'s own `Display`,
/// because `0.45f32 - 0.25f32` is `0.19999999`, which is a different NUMBER
/// from the `0.2` this builder used to emit. Two decimals is the precision
/// the bands are declared at. The spelling does change — `0.5` becomes
/// `0.50` — but `0.5` and `0.50` are the same `numeric` to Postgres, so no
/// score moves.
fn band_parts(band: Band) -> (String, String) {
    (
        format!("{:.2}", band.floor),
        format!("{:.2}", band.ceiling - band.floor),
    )
}

fn lexical_sidecar_sql(
    projection: &MemorySearchProjection,
    req: &MemorySearchRequest,
    rescue: bool,
    like_only: bool,
) -> Result<String, StorageError> {
    if like_only {
        return substring_sidecar_sql(projection, req);
    }
    ranked_projection_sql(projection, req, rescue)
}

/// The substring arm. Unchanged in shape: the projection stores a tsvector,
/// not text, so `LIKE` has nowhere to run but the owning sidecar.
fn substring_sidecar_sql(
    projection: &MemorySearchProjection,
    req: &MemorySearchRequest,
) -> Result<String, StorageError> {
    let table = PgIdent::table(&projection.sidecar_table)?;
    let search_text = sidecar_text_sql(projection)?;
    let snippet = snippet_sql(projection)?;
    let tag_pred = match projection.tag_column.as_deref() {
        Some(column) if !req.tags.is_empty() => {
            let column = PgIdent::column(column)?;
            format!(
                " AND c.{} {op} $4::text[]",
                column.as_str(),
                op = tag_operator(req.tag_match)
            )
        }
        _ => String::new(),
    };
    let order_by = match req.order {
        SearchOrder::Relevance => "lexical_score DESC, c.t DESC",
        SearchOrder::Recency => "c.t DESC",
    };
    // A flat band: the substring arm ranks nothing, it only admits.
    let score = format!("{:.2}", BAND_SUBSTRING.floor);
    // SQL-POLICY: PgIdent
    Ok(format!(
        "{q_cte}
         SELECT c.t,
                {score}::real AS lexical_score,
                {snippet} AS snippet
           FROM {table} c
           JOIN proxima_core.memory m ON m.t = c.t, q
          WHERE m.owner_id = ANY($8::uuid[])
            AND m.schema_id = $9
            AND (lower({search_text}) LIKE $2 ESCAPE '\\')
            {tag_pred}
            AND ($5::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(c.t), TIMESTAMPTZ '1970-01-01') >= $5)
            AND ($6::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(c.t), TIMESTAMPTZ '1970-01-01') <= $6)
            AND ($7::uuid IS NULL OR c.t < $7)
          ORDER BY {order_by}
          LIMIT $3",
        q_cte = query_side_cte(multilingual(projection)),
        score = score,
        table = table.as_str(),
    ))
}

/// The ranked arm: `<flavor>.projection` alone, then the sidecar for top-k.
fn ranked_projection_sql(
    projection: &MemorySearchProjection,
    req: &MemorySearchRequest,
    rescue: bool,
) -> Result<String, StorageError> {
    let table = PgIdent::table(&projection.projection_table)?;
    let sidecar = PgIdent::table(&projection.sidecar_table)?;
    let snippet = snippet_sql(projection)?;
    let tsv = "p.search_tsv";
    let tag_pred = if projection.tag_column.is_some() && !req.tags.is_empty() {
        format!(
            " AND p.tag {op} $4::text[]",
            op = tag_operator(req.tag_match)
        )
    } else {
        String::new()
    };
    let multilingual = multilingual(projection);
    let rank_tsq = rank_tsquery_expr(multilingual);
    let weights = rank_weight_array(projection);
    // The three score windows are named in the contract (`BAND_EXACT`,
    // `BAND_RESCUE`, `BAND_SUBSTRING`) and rendered from it here. Raw
    // `ts_rank` is not comparable across corpora; a band is, which is what
    // makes a cross-flavor merge meaningful. Naming them moved no score:
    // the floor and width below are the numbers this SQL already carried,
    // re-spelled at two decimals (`0.5` -> `0.50`).
    let (rescue_floor, rescue_width) = band_parts(BAND_RESCUE);
    let rescue_score = if rescue {
        format!(
            ", CASE WHEN q.any_tsq IS NOT NULL AND {tsv} @@ q.any_tsq
                    THEN {rescue_floor} + LEAST(COALESCE(ts_rank({weights}{tsv}, q.any_tsq, 1|32), 0.0) * 100.0, 1.0) * {rescue_width}
                    ELSE 0.0 END"
        )
    } else {
        String::new()
    };
    let rescue_where = if rescue {
        format!(" OR (q.any_tsq IS NOT NULL AND {tsv} @@ q.any_tsq)")
    } else {
        String::new()
    };
    let (exact_floor, exact_width) = band_parts(BAND_EXACT);
    let score_expr = format!(
        "GREATEST(
                    CASE WHEN {tsv} @@ q.tsq
                         THEN {exact_floor} + LEAST(COALESCE(ts_rank_cd({weights}{tsv}, {rank_tsq}, 32), 0.0), 1.0) * {exact_width}
                         ELSE 0.0 END{rescue_score},
                    0.0
                )::real"
    );
    let ranked_order = match req.order {
        SearchOrder::Relevance => "lexical_score DESC, p.memory_id DESC",
        SearchOrder::Recency => "p.memory_id DESC",
    };
    let outer_order = match req.order {
        SearchOrder::Relevance => "r.lexical_score DESC, r.t DESC",
        SearchOrder::Recency => "r.t DESC",
    };
    // SQL-POLICY: PgIdent
    Ok(format!(
        "{q_cte},
         ranked AS (
         SELECT p.memory_id AS t,
                {score_expr} AS lexical_score
           FROM {table} p
           JOIN proxima_core.memory m ON m.t = p.memory_id, q
          WHERE m.owner_id = ANY($8::uuid[])
            AND p.schema_id = $9
            AND ({tsv} @@ q.tsq{rescue_where})
            {tag_pred}
            AND length($2::text) >= 0
            AND ($5::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(p.memory_id), TIMESTAMPTZ '1970-01-01') >= $5)
            AND ($6::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(p.memory_id), TIMESTAMPTZ '1970-01-01') <= $6)
            AND ($7::uuid IS NULL OR p.memory_id < $7)
          ORDER BY {ranked_order}
          LIMIT $3
         )
         SELECT r.t,
                r.lexical_score,
                {snippet} AS snippet
           FROM ranked r
           JOIN {sidecar} c ON c.t = r.t
          ORDER BY {outer_order}",
        q_cte = query_side_cte(multilingual),
        table = table.as_str(),
        sidecar = sidecar.as_str(),
    ))
}

fn tag_operator(tag_match: TagMatch) -> &'static str {
    match tag_match {
        TagMatch::Any => "&&",
        TagMatch::All => "@>",
    }
}

/// Whether the query side must OR one `websearch_to_tsquery` per registered
/// configuration. Only a `PerRow` policy leaves the row's language unknown
/// to the query; a pinned one is known at build time.
fn multilingual(projection: &MemorySearchProjection) -> bool {
    matches!(projection.language, LanguagePolicy::PerRow { .. })
}

/// `ts_rank`'s weight array, or the empty string when the unit declares one
/// uniform level. Passing no array is not the same as passing the default
/// one only in spelling: with a single level every lexeme is class `D` and
/// the default `{0.1,0.2,0.4,1.0}` would scale every score by 0.1.
fn rank_weight_array(projection: &MemorySearchProjection) -> String {
    let Some(weights) = projection.rank_weights else {
        return String::new();
    };
    let rendered = weights
        .iter()
        .map(|weight| format!("{weight}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("'{{{rendered}}}'::float4[], ")
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

/// Match-side tsquery. When the schema declares `LanguagePolicy::PerRow`
/// (agent memory tables), OR one `websearch_to_tsquery` per
/// `lexical_languages` row — the query cannot know its language. Schemas
/// that pin one config stay on `lexical_config()`.
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

fn rank_tsquery_expr(multilingual: bool) -> &'static str {
    if multilingual {
        "websearch_to_tsquery(p.lexical_language, \
         proxima_core.lexical_query_text(p.lexical_language, q.scrubbed))"
    } else {
        "q.tsq"
    }
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

    /// Parity pin for the unscoped-search rewire.
    ///
    /// The filter in `core_search_projections` was
    /// `sidecar_table.starts_with("proxima_core.")` and is now
    /// `FLAVOR_0.declares_sidecar_table(..)`. The literal below is the set
    /// the prefix test selected out of the shipped registry, held HERE so
    /// production carries no second copy of it: if the contract ever stops
    /// declaring one of these, unscoped search silently narrows and this
    /// test says which one went.
    #[test]
    fn the_contract_selects_the_sidecars_the_name_prefix_used_to() {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let mut by_contract = registry
            .search_projections()
            .iter()
            .map(|projection| projection.sidecar_table.as_str())
            .filter(|table| proxima_core::FLAVOR_0.declares_sidecar_table(table))
            .collect::<Vec<_>>();
        by_contract.sort_unstable();
        by_contract.dedup();
        assert_eq!(
            by_contract,
            vec![
                "proxima_core.agent_derivation_v1",
                "proxima_core.agent_note_v1",
                "proxima_core.interpretation_v1",
                "proxima_core.utterance_v1",
            ],
        );
        let mut by_prefix = registry
            .search_projections()
            .iter()
            .map(|projection| projection.sidecar_table.as_str())
            .filter(|table| table.starts_with("proxima_core."))
            .collect::<Vec<_>>();
        by_prefix.sort_unstable();
        by_prefix.dedup();
        assert_eq!(
            by_contract, by_prefix,
            "the ordinal and the schema-name prefix must still agree on core's own sidecars"
        );
    }

    #[test]
    fn sidecar_scan_filters_owner_before_limit() {
        let src = include_str!("search.rs");
        assert!(
            src.contains("JOIN proxima_core.memory m ON m.t = c.t"),
            "sidecar GIN is ownerless; scan must join memory"
        );
        assert!(
            src.contains("m.owner_id = ANY($8::uuid[])"),
            "owner filter must sit on the sidecar scan, not only admit"
        );
    }

    /// Parity pin for the band rewire.
    ///
    /// The three score windows were inline float literals in this builder.
    /// They are now `BAND_EXACT`, `BAND_RESCUE` and `BAND_SUBSTRING` in the
    /// flavor contract, rendered as `floor + LEAST(rank, 1.0) * width`.
    ///
    /// The emitted TEXT is not byte-identical to what shipped: the builder
    /// wrote `0.5` and `0.2`, and `{:.2}` renders `0.50` and `0.20`. The
    /// VALUE is. Both spellings parse to the same `numeric`, and the width
    /// is rendered rather than printed with `f32`'s `Display` precisely
    /// because `0.45f32 - 0.25f32` is `0.19999999` — which would have moved
    /// a score. This pins the values; naming a band must not move one.
    #[test]
    fn the_named_bands_render_the_arithmetic_the_sql_already_had() {
        use super::{BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING, band_parts};

        assert_eq!(
            band_parts(BAND_EXACT),
            ("0.50".to_owned(), "0.50".to_owned())
        );
        assert_eq!(
            band_parts(BAND_RESCUE),
            ("0.25".to_owned(), "0.20".to_owned())
        );
        assert_eq!(
            band_parts(BAND_SUBSTRING),
            ("0.25".to_owned(), "0.00".to_owned()),
            "the substring arm admits, it does not rank: zero width"
        );
        assert_eq!(format!("{:.2}::real", BAND_SUBSTRING.floor), "0.25::real");
    }
}
