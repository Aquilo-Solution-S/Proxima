use std::collections::BTreeMap;
use std::fmt::Write as _;

use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::verbs::query::{
    DEFAULT_HYBRID_SEMANTIC_WEIGHT, EntityKind, MAX_SEARCH_PAGE_LIMIT, MemorySearchPage,
    MemorySearchRequest, MemorySearchResult, SearchCursor, SearchMode, SearchOrder,
    SupersessionStatus, TagMatch,
};
use proxima_core::verbs::schema::{
    MemorySearchProjection, MemorySearchProjectionField, PayloadKind,
};
use proxima_core::{MemoryId, OwnerRef, SchemaId, SearchProjectionColumnKind, StorageError};
use sqlx::PgPool;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::pgvector::SET_HNSW_SEARCH_SQL;

use super::{entity_owner_union, read_owner_columns};

/// Text-search configuration for the lexical branch, read from the database
/// rather than written here.
///
/// The configuration stems both document and query tokens ("adopted" matches
/// "adopt") and drops stopwords, so a natural-language question's content
/// words drive the AND-semantics match instead of its function words ("what",
/// "my", …). With `simple` (the pre-v0.0.7 config), every question word had
/// to appear literally — on conversational corpora most queries matched
/// nothing. A query that is *all* stopwords yields an empty tsquery and falls
/// back to the substring `LIKE` arm below.
///
/// It used to be the literal `english` in this file, and separately the
/// literal `english` inside `proxima_core.lexical_tsv`, which every stored
/// `search_tsv` column is generated from. Those two must name the same
/// configuration: a german document vector answered by an english tsquery
/// does not match worse, it does not match. Emitting the function call keeps
/// one authority for both sides, so a deployment that switches the
/// configuration (`proxima_core.set_lexical_config`) moves the query side
/// with it and cannot half-switch.
///
/// `lexical_config()` is IMMUTABLE and returns a constant, so the planner
/// folds it; this costs nothing per query.
const TEXT_SEARCH_CONFIG: &str = "proxima_core.lexical_config()";

/// SQL that lowercases a bound query parameter and neutralises the `LIKE`
/// metacharacters inside it, for use between `'%' || … || '%'` with
/// `ESCAPE '\'`.
///
/// The substring arm used to concatenate the parameter in raw, so `%` and `_`
/// in a user's query were wildcards rather than characters. Searching for
/// `100%` matched every memory beginning `100` — 70 of 3,000 rows on an
/// indexed corpus where the literal string matched none — and a query of a
/// bare `%` matched the entire corpus at the substring band's score.
///
/// The backslash is escaped first, so an already-backslashed query cannot
/// smuggle an escape through the later replacements.
fn like_literal(query_param: usize) -> String {
    format!(
        "replace(replace(replace(lower(${query_param}), '\\\\', '\\\\\\\\'), '%', '\\\\%'), '_', '\\\\_')"
    )
}

// Lexical score bands. Websearch AND semantics require every content word
// to co-occur in one memory; on conversational corpora most multi-word
// questions match nothing, so an OR-rescue arm re-runs the same lexemes
// any-matched. Match *tier* must dominate cover-density rank — ts_rank_cd
// penalizes wide multi-term covers, so an unbanded strict match can rank
// below a saturated single-term rescue hit — hence disjoint bands:
// strict [0.5, 1.0] > rescue (0.25, 0.45] > substring LIKE 0.25.
//
// `ts_rank_cd` is normalised with flag 32 (divide by itself + 1) rather than
// multiplied by a constant. Nothing here assigns A/B/C/D lexeme weights, so
// every document is weight D, and cover density for weight D starts at 0.1 —
// which made the old `LEAST(ts_rank_cd(...) * 10.0, 1.0)` equal to 1.0 for
// every row that matched at all. Measured against an indexed corpus: 3,170
// of 3,170 matching rows saturated, in both arms. The rank term was a
// constant, so *within* a band nothing was ranked and the order was whatever
// the plan happened to emit — a lexical search over 5,142 matches returned
// six unrelated files all scoring exactly 0.45. Flag 32 bounds the value in
// [0, 1) and it varies (0.091..0.888 over those same rows), leaving `LEAST`
// as a guard rather than the whole computation.
//
// The *rescue* arm ranks with `ts_rank(v, q, 1|32)` and not with cover
// density, for the same reason the code flavor's rescue arm does. Cover
// density rewards a short span containing several query terms, which is
// exactly the shape of repetitive structured data. Measured over an
// indexed corpus of 4,935 chunks with a real bug report as the query,
// cover density returned a documentation page and eight chunks of one
// `schema.json` — several scoring identically to six decimal places —
// while the file the fix actually touched never appeared. Flag 1 adds
// division by the log of document length, which is what separates a
// precise short chunk from a long repetitive one:
//
//   corpus                    ts_rank_cd      ts_rank(1|32)
//   17 knip bug reports       1 of 17         5 of 17
//   7 prek bug reports        3 of 7          5 of 7
//   24 prose questions        12 of 24        17 of 24
//
// The strict arm keeps cover density: measured on the same three corpora,
// giving it length normalisation too changes nothing, because a
// multi-sentence query almost never AND-matches at all and the arm does
// not fire.

#[derive(Debug)]
struct SearchRow {
    memory_id: uuid::Uuid,
    kind: EntityKind,
    schema_id: String,
    created_at: time::OffsetDateTime,
    snippet: String,
    lexical_score: f32,
    similarity_score: f32,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for SearchRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row as _;

        Ok(Self {
            memory_id: row.try_get("memory_id")?,
            kind: row.try_get("kind")?,
            schema_id: row.try_get("schema_id")?,
            created_at: row.try_get("created_at")?,
            snippet: row.try_get("snippet")?,
            lexical_score: row.try_get("lexical_score")?,
            similarity_score: row.try_get("similarity_score")?,
        })
    }
}

#[derive(Debug, Clone)]
struct Candidate {
    memory_id: uuid::Uuid,
    kind: EntityKind,
    schema_id: SchemaId,
    created_at: time::OffsetDateTime,
    snippet: String,
    lexical_score: f32,
    similarity_score: f32,
}

#[derive(Debug, Clone, Copy)]
struct CandidateFilterParams {
    schema_filter: Option<usize>,
    since: Option<usize>,
    until: Option<usize>,
    tags: Option<usize>,
    /// First of two params (`created_at`, `memory_id`) for a pushed-down
    /// recency keyset. Relevance cursors filter post-fusion instead —
    /// the fused score does not exist per SQL branch.
    recency_cursor: Option<usize>,
}

const MIN_VECTOR_CANDIDATE_OVERFETCH: u64 = 512;
const VECTOR_CANDIDATE_OVERFETCH_PER_RESULT: u64 = 64;
pub(crate) async fn search_memories(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
) -> Result<MemorySearchPage, StorageError> {
    if matches!(req.kind, Some(EntityKind::Goal)) || req.limit == 0 {
        return Ok(MemorySearchPage {
            results: Vec::new(),
            has_more: false,
        });
    }
    // The engine validates this pairing; re-check for direct port callers.
    if let Some(after) = req.after
        && after.order() != req.order
    {
        return Err(StorageError::ConstraintViolation(
            "search cursor order does not match request order".into(),
        ));
    }

    let limit = req.limit.min(MAX_SEARCH_PAGE_LIMIT);
    // One extra fused row past the page proves has_more. A relevance
    // cursor additionally widens the window by the rows earlier pages
    // already emitted, so page N still reaches candidates ranked past
    // them; recency cursors filter in SQL and need no widening.
    let relevance_depth = match req.after {
        Some(after @ SearchCursor::Relevance { .. }) => after.seen(),
        _ => 0,
    };
    let fetch_target = limit.saturating_add(1).saturating_add(relevance_depth);
    let mut candidates = BTreeMap::<uuid::Uuid, Candidate>::new();
    let overfetch = fetch_target.saturating_mul(4);
    let candidate_overfetch = u64::from(fetch_target)
        .saturating_mul(VECTOR_CANDIDATE_OVERFETCH_PER_RESULT)
        .max(MIN_VECTOR_CANDIDATE_OVERFETCH);

    match req.mode {
        SearchMode::Lexical => {
            for row in run_lexical(pool, req, projections, overfetch).await? {
                merge_row(&mut candidates, row);
            }
        }
        SearchMode::Semantic => {
            for row in run_semantic(pool, req, projections, overfetch, candidate_overfetch).await? {
                merge_row(&mut candidates, row);
            }
        }
        SearchMode::Hybrid => {
            // The lexical and vector candidate queries are independent (disjoint
            // indexes) and merge order-independently, so run them concurrently
            // to halve wall-clock latency. Weights/ef_search are unchanged.
            let (lexical, semantic) = tokio::try_join!(
                run_lexical(pool, req, projections, overfetch),
                run_semantic(pool, req, projections, overfetch, candidate_overfetch),
            )?;
            for row in lexical {
                merge_row(&mut candidates, row);
            }
            for row in semantic {
                merge_row(&mut candidates, row);
            }
        }
    }

    let semantic_weight = req
        .semantic_weight
        .unwrap_or(DEFAULT_HYBRID_SEMANTIC_WEIGHT)
        .clamp(0.0, 1.0);
    let mut results: Vec<MemorySearchResult> = candidates
        .into_values()
        .map(|candidate| {
            let score = match req.mode {
                SearchMode::Lexical => candidate.lexical_score,
                SearchMode::Semantic => candidate.similarity_score,
                SearchMode::Hybrid => {
                    (semantic_weight * candidate.similarity_score)
                        + ((1.0 - semantic_weight) * candidate.lexical_score)
                }
            };
            MemorySearchResult {
                memory_id: MemoryId::new(candidate.memory_id),
                kind: candidate.kind,
                schema_id: candidate.schema_id,
                created_at: candidate.created_at,
                snippet: candidate.snippet,
                score,
                lexical_score: candidate.lexical_score,
                similarity_score: candidate.similarity_score,
            }
        })
        .collect();

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
    Ok(MemorySearchPage { results, has_more })
}

/// Strictly-after-the-cursor keyset predicate, matching the descending
/// `(score, memory_id)` / `(created_at, memory_id)` sort above. The
/// recency variant is also pushed into SQL; re-checking here keeps
/// correctness independent of which branch produced the row. Scores
/// round-trip as exact `f32` bits, so equality against the recomputed
/// fused score is meaningful.
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

fn merge_row(candidates: &mut BTreeMap<uuid::Uuid, Candidate>, row: SearchRow) {
    let entry = candidates
        .entry(row.memory_id)
        .or_insert_with(|| Candidate {
            memory_id: row.memory_id,
            kind: row.kind,
            schema_id: SchemaId::new(row.schema_id.clone()),
            created_at: row.created_at,
            snippet: row.snippet.clone(),
            lexical_score: 0.0,
            similarity_score: 0.0,
        });
    entry.lexical_score = entry.lexical_score.max(row.lexical_score.max(0.0));
    entry.similarity_score = entry
        .similarity_score
        .max(row.similarity_score.clamp(0.0, 1.0));
    if entry.snippet.is_empty() && !row.snippet.is_empty() {
        entry.snippet = row.snippet;
    }
}

/// Exact SQL of the lexical branch, factored out so EXPLAIN-based plan
/// tests can assert against precisely what production executes.
/// Parameter order: read-owner kind/id arrays, per-projection
/// `(schema_id, version)` pairs, optional filter params, query text.
fn lexical_branch_sql<'p>(
    req: &MemorySearchRequest,
    projections: &'p [MemorySearchProjection],
    limit: u32,
) -> Result<(String, Vec<&'p MemorySearchProjection>), StorageError> {
    let projections = memory_search_projections(req, projections);
    let mut next_param = 3;
    let mut sql = common_candidates_sql(req, &projections, &mut next_param, true)?;
    let query_param = next_param;
    let order_by = branch_order_by(req, "lexical_score");
    // The OR-rescue arm serves pure-lexical retrieval (including hybrid
    // searches degraded to lexical): without it, websearch AND semantics
    // leave most conversational queries empty-handed. Inside true hybrid
    // fusion it is noise — partial-match scores compete against precise
    // semantic scores and drag recall down (measured: session Recall@5
    // 0.99 → 0.82 when fused) — so hybrid keeps the strict+substring arms
    // only and lets the semantic leg carry recall.
    let rescue = matches!(req.mode, SearchMode::Lexical);
    let rescue_score_arm = if rescue {
        ", CASE WHEN c.search_tsv @@ q.any_tsq
                THEN 0.25 + LEAST(ts_rank(c.search_tsv, q.any_tsq, 1|32) * 100.0, 1.0) * 0.2
                ELSE 0.0 END"
    } else {
        ""
    };
    let rescue_where_arm = if rescue {
        " OR c.search_tsv @@ q.any_tsq"
    } else {
        ""
    };

    // `candidates` carries `search_tsv` per branch — read from the stored
    // column where the table has one, computed once inside the MATERIALIZED
    // CTE where it does not. Either way the vector is produced exactly once
    // per candidate row, so the match arm, the rank arm and the WHERE gate
    // share it rather than each re-tokenising the document.
    write!(
        sql,
        " , q AS (
               SELECT websearch_to_tsquery(
                   {ts_config},
                   regexp_replace(
                       regexp_replace(${query_param}, '[[:punct:]]+', ' ', 'g'),
                       '\\m[[:alnum:]]{{255}}[[:alnum:]]+\\M',
                       ' ',
                       'g'
                   )
               ) AS tsq,
               -- OR-rescue arm: the same content lexemes any-matched.
               -- plainto_tsquery emits only '&' between lexemes (no phrase
               -- or negation operators), so the operator swap is safe.
               NULLIF(
                   replace(
                       plainto_tsquery(
                           {ts_config},
                           regexp_replace(
                               regexp_replace(${query_param}, '[[:punct:]]+', ' ', 'g'),
                               '\\m[[:alnum:]]{{255}}[[:alnum:]]+\\M',
                               ' ',
                               'g'
                           )
                       )::text,
                       ' & ',
                       ' | '
                   ),
                   ''
               )::tsquery AS any_tsq
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 GREATEST(
                     CASE WHEN c.search_tsv @@ q.tsq
                          THEN 0.5 + LEAST(ts_rank_cd(c.search_tsv, q.tsq, 32), 1.0) * 0.5
                          ELSE 0.0 END{rescue_score_arm},
                     CASE WHEN lower(c.search_text) LIKE '%' || {like_literal} || '%' ESCAPE '\\'
                          THEN 0.25 ELSE 0.0 END
                 )::real AS lexical_score,
                 0.0::real AS similarity_score
          FROM candidates c, q
          WHERE c.search_text <> ''
            AND (
                c.search_tsv @@ q.tsq{rescue_where_arm}
                OR lower(c.search_text) LIKE '%' || {like_literal} || '%' ESCAPE '\\'
            )
          ORDER BY {order_by}
          LIMIT {}",
        u64::from(limit),
        order_by = order_by,
        ts_config = TEXT_SEARCH_CONFIG,
        rescue_score_arm = rescue_score_arm,
        rescue_where_arm = rescue_where_arm,
        like_literal = like_literal(query_param)
    )
    .expect("write to String is infallible");
    Ok((sql, projections))
}

/// The lexical branch SQL for EXPLAIN-based plan assertions in tests.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn lexical_search_sql_for_tests(
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    limit: u32,
) -> Result<String, StorageError> {
    lexical_branch_sql(req, projections, limit).map(|(sql, _)| sql)
}

async fn run_lexical(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    limit: u32,
) -> Result<Vec<SearchRow>, StorageError> {
    let (sql, projections) = lexical_branch_sql(req, projections, limit)?;

    // SQL-POLICY: PgIdent
    let mut q = bind_common(
        sqlx::query_as::<_, SearchRow>(sqlx::AssertSqlSafe(sql)),
        req,
    );
    for projection in &projections {
        q = q.bind(projection.schema_id.as_str().to_string());
        q = q.bind(projection.schema_version.into_inner().cast_signed());
    }
    q = bind_filter_params(q, req);
    q = q.bind(req.query.clone());
    q.fetch_all(pool).await.map_err(map_err)
}

/// Exact SQL of the semantic vector branch, factored out so
/// EXPLAIN-based plan tests can assert against precisely what
/// production executes. Parameter order: read-owner kind/id arrays,
/// per-projection `(schema_id, version)` pairs, optional filter
/// params, query vector literal, model id.
fn semantic_branch_sql<'p>(
    req: &MemorySearchRequest,
    projections: &'p [MemorySearchProjection],
    limit: u32,
    candidate_overfetch: u64,
) -> Result<(String, Vec<&'p MemorySearchProjection>), StorageError> {
    let projections = memory_search_projections(req, projections);
    let mut next_param = 3;
    let mut sql = common_candidates_sql(req, &projections, &mut next_param, false)?;
    let vec_param = next_param;
    let model_param = next_param + 1;
    let order_by = branch_order_by(req, "similarity_score");

    write!(
        sql,
        " , eligible_entities AS MATERIALIZED (
              SELECT DISTINCT ON (c.kind, c.memory_id)
                     c.memory_id, c.owner_kind, c.owner_id, c.kind,
                     c.schema_id, c.created_at, c.search_text
                FROM candidates c
               ORDER BY c.kind, c.memory_id, c.created_at DESC
          ),
          -- One row per *memory*, not per embedding row. Since chunked
          -- embeddings, an over-limit memory holds several vectors under one
          -- version, and the nearest-neighbour scan returns each of them
          -- separately. Collapsing here — rather than leaving it to the
          -- caller's merge — is what keeps the outer LIMIT a budget of
          -- memories: without it, a handful of heavily-chunked memories fill
          -- the page with their own chunks, the page under-fills after the
          -- merge collapses them, and `has_more` reports false while matching
          -- memories were never returned.
          --
          -- The inner scan keeps `ORDER BY <vector distance> LIMIT n` intact,
          -- which is the only shape the HNSW index can serve
          -- (`semantic_search_plan_uses_hnsw_index` pins that); deduplication
          -- happens over its result, not in place of it. A memory scores by
          -- its best chunk, so partial-match coverage is preserved.
          vector_candidates AS MATERIALIZED (
              SELECT DISTINCT ON (ann.kind, ann.memory_id)
                     ann.memory_id, ann.kind, ann.schema_id, ann.created_at,
                     ann.search_text, ann.similarity_score
                FROM (
                  SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                         c.search_text,
                         CASE
                             WHEN (1 - (emb.vec <=> ${vec_param}::vector)) = 'NaN'::float8 THEN 0.0
                             ELSE GREATEST(0.0, (1 - (emb.vec <=> ${vec_param}::vector)))
                         END::real AS similarity_score
                    FROM proxima_core.embeddings emb
                    JOIN proxima_core.embedding_heads head
                      ON head.entity_kind = emb.entity_kind
                     AND head.entity_id = emb.entity_id
                     AND head.model_id = emb.model_id
                     AND head.embedding_version = emb.embedding_version
                     AND head.owner_kind = emb.owner_kind
                     AND head.owner_id IS NOT DISTINCT FROM emb.owner_id
                    JOIN eligible_entities c
                      ON c.kind = emb.entity_kind
                     AND c.memory_id = emb.entity_id
                     AND c.owner_kind = emb.owner_kind
                     AND c.owner_id IS NOT DISTINCT FROM emb.owner_id
                   WHERE emb.model_id = ${model_param}
                   ORDER BY emb.vec <=> ${vec_param}::vector
                   LIMIT {candidate_overfetch}
                ) ann
               ORDER BY ann.kind, ann.memory_id, ann.similarity_score DESC
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 c.similarity_score
          FROM vector_candidates c
          ORDER BY {order_by}
          LIMIT {}",
        u64::from(limit),
        candidate_overfetch = candidate_overfetch,
        order_by = order_by
    )
    .expect("write to String is infallible");
    Ok((sql, projections))
}

/// The semantic branch SQL for EXPLAIN-based plan assertions in tests.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn semantic_search_sql_for_tests(
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    limit: u32,
    candidate_overfetch: u64,
) -> Result<String, StorageError> {
    semantic_branch_sql(req, projections, limit, candidate_overfetch).map(|(sql, _)| sql)
}

async fn run_semantic(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    limit: u32,
    candidate_overfetch: u64,
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

    let (sql, projections) = semantic_branch_sql(req, projections, limit, candidate_overfetch)?;

    // SQL-POLICY: PgIdent
    let mut q = bind_common(
        sqlx::query_as::<_, SearchRow>(sqlx::AssertSqlSafe(sql)),
        req,
    );
    for projection in &projections {
        q = q.bind(projection.schema_id.as_str().to_string());
        q = q.bind(projection.schema_version.into_inner().cast_signed());
    }
    q = bind_filter_params(q, req);
    q = q.bind(crate::pgvector::literal(query_embedding));
    q = q.bind(model_id.clone());

    let mut tx = pool.begin().await.map_err(map_err)?;
    // SQL-POLICY: fixed-fragment
    sqlx::raw_sql(SET_HNSW_SEARCH_SQL)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    let rows = q.fetch_all(&mut *tx).await.map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    Ok(rows)
}

/// Builds the owner-scoped candidate CTE shared by both branches.
///
/// `include_tsv` adds each candidate's lexical vector as a column. Only
/// the lexical branch needs it, and `candidates` is MATERIALIZED, so
/// emitting it unconditionally would make every semantic search
/// materialise a few hundred tsvectors it never reads.
fn common_candidates_sql(
    req: &MemorySearchRequest,
    projections: &[&MemorySearchProjection],
    next_param: &mut usize,
    include_tsv: bool,
) -> Result<String, StorageError> {
    let sidecar_first_param = *next_param;
    *next_param += projections.len() * 2;
    let schema_filter_param = req.schema_id.as_ref().map(|_| {
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
    let recency_cursor_param = matches!(req.after, Some(SearchCursor::Recency { .. })).then(|| {
        let param = *next_param;
        *next_param += 2;
        param
    });
    let filters = CandidateFilterParams {
        schema_filter: schema_filter_param,
        since: since_param,
        until: until_param,
        tags: tags_param,
        recency_cursor: recency_cursor_param,
    };

    // MATERIALIZED pins the plan to owner-first enumeration: candidates
    // are resolved via the owner-prefix indexes before any text or vector
    // predicate runs. Left inlinable, the planner pushed the lexical
    // tsvector filter into the Abstraction sidecar branch and seq-scanned
    // the whole table across every owner (measured: 3.1s of a 3.3s
    // query on the 150k corpus). The per-owner candidate set is a few
    // hundred rows, so the materialization itself is noise.
    let mut sql = String::from("WITH candidates AS MATERIALIZED (");
    push_candidate_branch_prefix(&mut sql);
    write!(
        sql,
        "NULL::text[] AS tags, COALESCE(m.text, '') AS search_text{base_tsv} \
         FROM proxima_core.memories m \
         LEFT JOIN {entity_owner_union} home_owner \
           ON home_owner.entity_id = m.memory_id \
",
        entity_owner_union = entity_owner_union(),
        // memories.search_tsv is generated from the same COALESCE(text, '')
        // this branch projects, so the column and the expression it
        // replaces are the same value by construction.
        base_tsv = if include_tsv {
            ", m.search_tsv AS search_tsv"
        } else {
            ""
        },
    )
    .expect("write to String is infallible");
    push_base_memory_filters(&mut sql, req, filters);
    sql.push_str(" AND NULLIF(m.text, '') IS NOT NULL");

    for (idx, projection) in projections.iter().enumerate() {
        let table = PgIdent::table(&projection.sidecar_table)?;
        let projection_expr = projection_search_expr(&projection.fields)?;
        let tag_expr = projection_tag_expr(projection)?;
        let schema_param = sidecar_first_param + (idx * 2);
        let version_param = schema_param + 1;
        let search_text_expr = format!("NULLIF(concat_ws(' ', {projection_expr}), '')");
        let tsv_expr = if include_tsv {
            format!(
                ", {} AS search_tsv",
                projection_tsv_expr(projection, &search_text_expr)?
            )
        } else {
            String::new()
        };
        sql.push_str(" UNION ALL ");
        push_candidate_branch_prefix(&mut sql);
        write!(
            sql,
            "{tag_expr} AS tags,
             {search_text_expr} AS search_text{tsv_expr}
             FROM proxima_core.memories m
             LEFT JOIN {entity_owner_union} home_owner
               ON home_owner.entity_id = m.memory_id
JOIN {table} s ON s.memory_id = m.memory_id",
            tag_expr = tag_expr.as_str(),
            entity_owner_union = entity_owner_union(),
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

/// The candidate's lexical vector: the stored column when the sidecar
/// declares one, else the same vector computed inline.
///
/// Both spellings resolve to `proxima_core.lexical_tsv` over the branch's
/// projected search text — the stored column is generated from it (see
/// migration 0011), so a sidecar with a column and one without cannot
/// score differently for the same content.
fn projection_tsv_expr(
    projection: &MemorySearchProjection,
    search_text_expr: &str,
) -> Result<String, StorageError> {
    let Some(tsv_column) = &projection.tsv_column else {
        return Ok(format!("proxima_core.lexical_tsv({search_text_expr})"));
    };
    let column = PgIdent::column(tsv_column)?;
    Ok(format!("s.{}", column.as_str()))
}

fn projection_tag_expr(projection: &MemorySearchProjection) -> Result<String, StorageError> {
    let Some(tag_column) = &projection.tag_column else {
        return Ok("NULL::text[]".to_string());
    };
    let column = PgIdent::column(tag_column)?;
    Ok(format!("s.{}", column.as_str()))
}

fn push_candidate_branch_prefix(sql: &mut String) {
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        "SELECT m.memory_id, home_owner.owner_kind, home_owner.owner_id, \
         COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, \
         m.schema_id, m.created_at, ",
    );
}

/// Owner-scope gate for one candidate branch, split at SQL-build time so
/// every arm is index-eligible. The generic `owner_id IS NOT DISTINCT
/// FROM s.id` join defeats the `(owner_kind, owner_id)` b-tree prefixes
/// (measured on a 150k-memory corpus: every branch seq-scanned all of
/// `memories` per owner check). `owner_binds` emits a NULL id only for
/// [`OwnerRef::World`], so the read set splits exactly into a plain
/// equality join plus — only when World is actually in the read set — a
/// constant World arm (`owner_id IS NULL` ⇔ World by the owner-shape
/// checks, and NULL never survives the equality join).
fn push_read_owner_scope(sql: &mut String, req: &MemorySearchRequest) {
    write!(
        sql,
        " WHERE (EXISTS (
              SELECT 1
                FROM {entity_owner_union} eo
                JOIN unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                  ON eo.owner_kind = s.kind AND eo.owner_id = s.id
               WHERE eo.entity_id = m.memory_id
           )",
        entity_owner_union = entity_owner_union(),
    )
    .expect("write to String is infallible");
    if req
        .read_owners
        .iter()
        .any(|owner| matches!(owner, OwnerRef::World))
    {
        write!(
            sql,
            " OR EXISTS (
              SELECT 1
                FROM {entity_owner_union} eo
               WHERE eo.entity_id = m.memory_id
                 AND eo.owner_kind = 'world'
                 AND eo.owner_id IS NULL
           )",
            entity_owner_union = entity_owner_union(),
        )
        .expect("write to String is infallible");
    }
    sql.push(')');
}

fn push_base_memory_filters(
    sql: &mut String,
    req: &MemorySearchRequest,
    filters: CandidateFilterParams,
) {
    push_read_owner_scope(sql, req);
    sql.push_str(" AND m.tombstoned_at IS NULL");
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
    push_recency_cursor_filter(sql, filters);
    push_tag_filter(sql, req, filters.tags, "NULL::text[]");
    push_search_head_filter(sql, req);
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
    push_read_owner_scope(sql, req);
    write!(
        sql,
        " AND m.tombstoned_at IS NULL
           AND m.schema_id = ${schema_param}
           AND m.schema_version = ${version_param}",
    )
    .expect("write to String is infallible");
    push_payload_kind_filter(sql, kind);
    push_time_filters(sql, filters);
    push_recency_cursor_filter(sql, filters);
    push_tag_filter(sql, req, filters.tags, tag_expr);
    push_search_head_filter(sql, req);
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

/// Keyset continuation for recency-ordered pages: only rows strictly
/// older than the cursor row survive, mirroring `query_memories`'
/// `(created_at, memory_id)` tiebreak.
fn push_recency_cursor_filter(sql: &mut String, filters: CandidateFilterParams) {
    if let Some(param) = filters.recency_cursor {
        write!(
            sql,
            " AND (m.created_at, m.memory_id) < (${param}, ${next})",
            next = param + 1
        )
        .expect("write to String is infallible");
    }
}

fn push_search_head_filter(sql: &mut String, req: &MemorySearchRequest) {
    if matches!(req.supersession, SupersessionStatus::IncludeSuperseded) {
        return;
    }
    // SQL-POLICY: fixed-fragment
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
                   AND m2.tombstoned_at IS NULL",
    );
    super::push_same_home_owner_successor_predicate(sql, "m2", "m");
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        " )) \
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

fn bind_filter_params<'q>(
    mut q: sqlx::query::QueryAs<'q, sqlx::Postgres, SearchRow, sqlx::postgres::PgArguments>,
    req: &'q MemorySearchRequest,
) -> sqlx::query::QueryAs<'q, sqlx::Postgres, SearchRow, sqlx::postgres::PgArguments> {
    if let Some(schema_id) = &req.schema_id {
        q = q.bind(schema_id.as_str().to_string());
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
    if let Some(SearchCursor::Recency {
        created_at,
        memory_id,
        ..
    }) = req.after
    {
        q = q.bind(created_at);
        q = q.bind(memory_id.into_inner());
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
