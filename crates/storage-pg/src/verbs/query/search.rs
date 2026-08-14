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
use crate::pgvector::set_hnsw_search_sql;
use crate::tuning::{PgTuning, SemanticIndexFirst};

use super::read_owner_columns;

/// Default text-search configuration, read from the database rather than
/// written here.
///
/// The configuration stems both document and query tokens ("adopted" matches
/// "adopt") and drops stopwords, so a natural-language question's content
/// words drive the AND-semantics match instead of its function words ("what",
/// "my", …). With `simple` (the pre-v0.0.7 config), every question word had
/// to appear literally — on conversational corpora most queries matched
/// nothing. A query that is *all* stopwords yields an empty tsquery and falls
/// back to the substring `LIKE` arm below.
///
/// Since per-row languages (migration 0014) this is only the FALLBACK for a
/// database whose `lexical_languages` table is empty. The real query side is
/// two-part, shaped by measurement (130-question German goldset, this exact
/// band SQL):
///
/// - **Match** with the OR of one `websearch_to_tsquery` per active language
///   (`proxima_core.lexical_languages`). Constant-config tsqueries fold to
///   one OR constant the GIN index serves; the per-row spelling
///   `websearch_to_tsquery(c.lexical_language, …)` in a WHERE clause has no
///   index path at all.
/// - **Rank** each candidate with its own row's configuration. Ranked this
///   way the multi-language OR is bit-identical to a single-language
///   baseline (0/130 top-5 sets changed); ranked against the OR query it
///   costs 6.2 points of recall@5, because wrong-config covers inflate
///   non-gold rows. A cross-config strict match whose row-config rank finds
///   no cover scores the bare band base — which is exactly the measured-free
///   behavior, hence the COALESCE(…, 0) around the rank terms.
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
    /// Spell the head filter's successor test as a join over the successor
    /// set (`push_supersedes_anti_join`) instead of a probe per candidate
    /// row. Carried here because both branch builders already thread these
    /// params through to the filter writers.
    supersedes_anti_join: bool,
}

#[allow(clippy::too_many_lines)]
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
        .saturating_mul(tuning.semantic_overfetch_per_result)
        .max(tuning.semantic_overfetch_min);

    match req.mode {
        SearchMode::Lexical => {
            for row in run_lexical(pool, req, projections, overfetch, tuning).await? {
                merge_row(&mut candidates, row);
            }
        }
        SearchMode::Semantic => {
            for row in run_semantic(
                pool,
                req,
                projections,
                overfetch,
                candidate_overfetch,
                tuning,
            )
            .await?
            {
                merge_row(&mut candidates, row);
            }
        }
        SearchMode::Hybrid => {
            // The lexical and vector candidate queries are independent (disjoint
            // indexes) and merge order-independently, so run them concurrently
            // to halve wall-clock latency. Weights/ef_search are unchanged.
            let (lexical, semantic) = tokio::try_join!(
                run_lexical(pool, req, projections, overfetch, tuning),
                run_semantic(
                    pool,
                    req,
                    projections,
                    overfetch,
                    candidate_overfetch,
                    tuning
                ),
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
#[allow(clippy::too_many_lines)]
fn lexical_branch_sql<'p>(
    req: &MemorySearchRequest,
    projections: &'p [MemorySearchProjection],
    limit: u32,
    tuning: &PgTuning,
) -> Result<(String, Vec<&'p MemorySearchProjection>), StorageError> {
    let projections = memory_search_projections(req, projections);
    let mut next_param = 3;
    let mut sql = common_candidates_sql(req, &projections, &mut next_param, true, tuning)?;
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
    // Match against the cross-language `q.any_tsq`; rank with the row's own
    // configuration (see TEXT_SEARCH_CONFIG). The row-config rescue tsquery
    // can be NULL where the query has no lexemes under that configuration —
    // ts_rank is STRICT, so COALESCE keeps the arm at its band base instead
    // of dropping it to NULL.
    let rescue_score_arm = if rescue {
        ", CASE WHEN c.search_tsv @@ q.any_tsq
                THEN 0.25 + LEAST(COALESCE(ts_rank(c.search_tsv,
                         NULLIF(replace(plainto_tsquery(c.lexical_language,
                                            proxima_core.lexical_query_text(
                                                c.lexical_language, q.scrubbed))::text,
                                        ' & ', ' | '), '')::tsquery,
                         1|32), 0.0) * 100.0, 1.0) * 0.2
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
        " , scrubbed AS (
               -- The same scrub every stored `search_tsv` went through
               -- (`lexical_tsv` = `to_tsvector(config, lexical_scrub(txt))`).
               -- Called, not restated: a query token that keeps punctuation
               -- the document side dropped can never match the stored
               -- lexeme, and that failure is silent.
               SELECT proxima_core.lexical_scrub(${query_param}) AS q
          )
          , q AS (
               -- One tsquery per active language, OR-combined: the match
               -- side cannot know the query's language, and the OR is
               -- GIN-indexable where a per-row-parsed tsquery is not.
               -- lexical_query_text stop-filters the text for stop-list-free
               -- configurations (simple), so one CJK row in the corpus
               -- cannot turn every query's function words into match terms.
               -- tsquery_or_agg over an empty lexical_languages is NULL;
               -- the COALESCE falls back to the default configuration.
               SELECT s.q AS scrubbed,
                      COALESCE(
                          (SELECT proxima_core.tsquery_or_agg(
                                      websearch_to_tsquery(l.config,
                                          proxima_core.lexical_query_text(l.config, s.q))
                                      ORDER BY l.config)
                             FROM proxima_core.lexical_languages l),
                          websearch_to_tsquery({ts_config}, s.q)
                      ) AS tsq,
                      -- OR-rescue arm: the same content lexemes any-matched.
                      -- plainto_tsquery emits only '&' between lexemes (no
                      -- phrase or negation operators), so the operator swap
                      -- is safe. NULLIF folds a no-lexeme language out; the
                      -- STRICT transition function skips those NULLs.
                      COALESCE(
                          (SELECT proxima_core.tsquery_or_agg(
                                      NULLIF(
                                          replace(plainto_tsquery(l.config,
                                              proxima_core.lexical_query_text(
                                                  l.config, s.q))::text,
                                                  ' & ', ' | '),
                                          '')::tsquery
                                      ORDER BY l.config)
                             FROM proxima_core.lexical_languages l),
                          NULLIF(
                              replace(plainto_tsquery({ts_config}, s.q)::text,
                                      ' & ', ' | '),
                              '')::tsquery
                      ) AS any_tsq
                 FROM scrubbed s
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 GREATEST(
                     CASE WHEN c.search_tsv @@ q.tsq
                          THEN 0.5 + LEAST(COALESCE(ts_rank_cd(c.search_tsv,
                                   websearch_to_tsquery(c.lexical_language,
                                       proxima_core.lexical_query_text(
                                           c.lexical_language, q.scrubbed)),
                                   32), 0.0), 1.0) * 0.5
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
    tuning: &PgTuning,
) -> Result<String, StorageError> {
    lexical_branch_sql(req, projections, limit, tuning).map(|(sql, _)| sql)
}

async fn run_lexical(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    limit: u32,
    tuning: &PgTuning,
) -> Result<Vec<SearchRow>, StorageError> {
    let (sql, projections) = lexical_branch_sql(req, projections, limit, tuning)?;

    // SQL-POLICY: PgIdent
    let mut q = bind_common(
        sqlx::query_as::<_, SearchRow>(sqlx::AssertSqlSafe(sql)),
        req,
        &projections,
    );
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
    tuning: &PgTuning,
) -> Result<(String, Vec<&'p MemorySearchProjection>), StorageError> {
    let projections = memory_search_projections(req, projections);
    let mut next_param = 3;
    let mut sql = common_candidates_sql(req, &projections, &mut next_param, false, tuning)?;
    let vec_param = next_param;
    let model_param = next_param + 1;
    let order_by = branch_order_by(req, "similarity_score");

    push_eligible_entities(&mut sql, tuning);
    let chunk_rows = match tuning.semantic_index_first {
        SemanticIndexFirst::Off => {
            push_joined_scan_note(&mut sql);
            ChunkRows {
                row: "ann",
                score: "ann",
                from: joined_ann_from(vec_param, model_param, candidate_overfetch),
            }
        }
        mode => {
            push_ann_scan(&mut sql, mode, vec_param, model_param, candidate_overfetch);
            ChunkRows {
                row: "c",
                score: "ann",
                from: index_first_ann_from(),
            }
        }
    };
    push_vector_candidates(&mut sql, tuning, &chunk_rows);

    write!(
        sql,
        "
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 c.similarity_score
          FROM vector_candidates c
          ORDER BY {order_by}
          LIMIT {}",
        u64::from(limit),
        order_by = order_by
    )
    .expect("write to String is infallible");
    Ok((sql, projections))
}

/// Rows an `Overfetch` scan may hold. Behind its barrier the window is spent
/// before eligibility is known, so it is a work budget rather than a result
/// budget and needs a ceiling the request's own window does not give it.
const MAX_ANN_SCAN_ROWS: u64 = 20_000;

/// The chunk-level rows the memory collapse reads — one row per embedding
/// that survived the nearest-neighbour window — with the aliases carrying a
/// row's memory columns and its score.
struct ChunkRows {
    row: &'static str,
    score: &'static str,
    from: String,
}

/// The eligibility set the vector branch joins against: one row per memory,
/// newest first. `semantic_index_first` never moves it; only
/// `candidate_window_dedup` changes how it collapses, and the window
/// spelling ranks in the same pass the scan already makes.
///
/// Rows of one memory differ only in `search_text` (one candidate branch per
/// projection), and they share a `created_at`, so which one wins the
/// collapse is already unspecified under `DISTINCT ON`.
fn push_eligible_entities(sql: &mut String, tuning: &PgTuning) {
    // SQL-POLICY: fixed-fragment
    if tuning.candidate_window_dedup {
        sql.push_str(
            " , eligible_entities AS MATERIALIZED (
              SELECT e.memory_id, e.owner_kind, e.owner_id, e.kind,
                     e.schema_id, e.created_at, e.search_text
                FROM (
                  SELECT c.memory_id, c.owner_kind, c.owner_id, c.kind,
                         c.schema_id, c.created_at, c.search_text,
                         row_number() OVER (PARTITION BY c.kind, c.memory_id
                                            ORDER BY c.created_at DESC) AS rn
                    FROM candidates c
                ) e
               WHERE e.rn = 1
          ),",
        );
        return;
    }
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        " , eligible_entities AS MATERIALIZED (
              SELECT DISTINCT ON (c.kind, c.memory_id)
                     c.memory_id, c.owner_kind, c.owner_id, c.kind,
                     c.schema_id, c.created_at, c.search_text
                FROM candidates c
               ORDER BY c.kind, c.memory_id, c.created_at DESC
          ),",
    );
}

fn push_joined_scan_note(sql: &mut String) {
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        "
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
          -- its best chunk, so partial-match coverage is preserved.",
    );
}

/// The similarity projection both vector scans share: cosine similarity
/// clamped to `[0, 1]`, with the `NaN` a zero-magnitude query produces
/// mapped to 0. `indent` reproduces each caller's layout, so the emitted
/// statements — the ones the goldens pin — stay byte-identical to when
/// this fragment was written out twice. No `ORDER BY` alias and no
/// LATERAL: the scan's `ORDER BY emb.vec <=> $n` must stay the raw
/// operator expression for HNSW to serve it.
fn similarity_score_sql(vec_param: usize, indent: usize) -> String {
    let pad = " ".repeat(indent);
    format!(
        "{pad}CASE\n\
         {pad}    WHEN (1 - (emb.vec <=> ${vec_param}::vector)) = 'NaN'::float8 THEN 0.0\n\
         {pad}    ELSE GREATEST(0.0, (1 - (emb.vec <=> ${vec_param}::vector)))\n\
         {pad}END::real AS similarity_score"
    )
}

/// The head-liveness and eligibility joins both vector scans read, over
/// whichever relation supplies the embedding row: `emb` when the scan is the
/// subquery itself, `ann` when it is the `ann_scan` CTE above.
///
/// Membership is decided here on both arms — an index-first scan's pushed
/// owner predicate can only narrow its input to rows these joins would admit
/// anyway. `indent` reproduces each caller's layout, so the emitted
/// statements — the ones the goldens pin — stay byte-identical to when this
/// block was written out twice.
fn head_and_eligibility_joins(source_alias: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    format!(
        "{pad}JOIN proxima_core.embedding_heads head\n\
         {pad}  ON head.entity_kind = {source_alias}.entity_kind\n\
         {pad} AND head.entity_id = {source_alias}.entity_id\n\
         {pad} AND head.model_id = {source_alias}.model_id\n\
         {pad} AND head.embedding_version = {source_alias}.embedding_version\n\
         {pad} AND head.owner_kind = {source_alias}.owner_kind\n\
         {pad} AND head.owner_id IS NOT DISTINCT FROM {source_alias}.owner_id\n\
         {pad}JOIN eligible_entities c\n\
         {pad}  ON c.kind = {source_alias}.entity_kind\n\
         {pad} AND c.memory_id = {source_alias}.entity_id\n\
         {pad} AND c.owner_kind = {source_alias}.owner_kind\n\
         {pad} AND c.owner_id IS NOT DISTINCT FROM {source_alias}.owner_id"
    )
}

/// The nearest-neighbour window with eligibility joined *under* it: the
/// window is a budget of eligible rows, at the price of the two joins
/// standing between the vector scan and the `ORDER BY … LIMIT` that would
/// drive it.
fn joined_ann_from(vec_param: usize, model_param: usize, candidate_overfetch: u64) -> String {
    format!(
        "                FROM (
                  SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                         c.search_text,
{similarity}
                    FROM proxima_core.embeddings emb
{joins}
                   WHERE emb.model_id = ${model_param}
                   ORDER BY emb.vec <=> ${vec_param}::vector
                   LIMIT {candidate_overfetch}
                ) ann",
        similarity = similarity_score_sql(vec_param, 25),
        joins = head_and_eligibility_joins("emb", 20)
    )
}

/// The same head and eligibility joins, read above an index-first scan.
fn index_first_ann_from() -> String {
    format!(
        "                FROM ann_scan ann\n{joins}",
        joins = head_and_eligibility_joins("ann", 16)
    )
}

/// Owner scope for an index-first scan, in the split-arm equality shape the
/// candidate branches use ([`push_read_owner_scope`]): the read set arrives
/// as the bound `$1`/`$2` arrays and every arm is a plain `=`, so
/// `idx_embeddings_owner` stays reachable.
///
/// No World arm, unlike the candidate branches:
/// `embeddings_world_not_write_owner_chk` forbids a world-owned embedding
/// row, so the arm could only ever match nothing. A world-owned *memory* is
/// likewise unreachable from here with or without this predicate — the
/// eligibility join demands `c.owner_kind = ann.owner_kind`, which no
/// embedding row can satisfy for it.
const EMBEDDING_OWNER_SCOPE_SQL: &str = "
                 AND EXISTS (
                       SELECT 1
                         FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                        WHERE emb.owner_kind = s.kind AND emb.owner_id = s.id
                     )";

/// The index-first nearest-neighbour scan: nothing stands between the
/// embeddings scan and the `ORDER BY <distance> LIMIT` that drives it, and
/// every eligibility join reads its result.
///
/// `Overfetch` keeps a materialization barrier, so the joins above consume a
/// window that was spent on unfiltered rows — recall then depends on that
/// window exceeding the inverse of the eligibility filter's selectivity, and
/// pgvector's iterative scan cannot help, because the scan's own LIMIT is
/// already satisfied. `Pushdown` drops the barrier and puts the owner arms
/// on the scan itself, which is what leaves the filter reachable for an
/// iterative scan.
fn push_ann_scan(
    sql: &mut String,
    mode: SemanticIndexFirst,
    vec_param: usize,
    model_param: usize,
    candidate_overfetch: u64,
) {
    let pushdown = matches!(mode, SemanticIndexFirst::Pushdown);
    let (barrier, note) = if pushdown {
        (
            "AS (",
            "          -- Index-first (pushdown): owner and model arms ride on the scan
          -- itself and nothing materializes above it.",
        )
    } else {
        (
            "AS MATERIALIZED (",
            "          -- Index-first (overfetch): the scan's window is spent before
          -- eligibility is known, so it is a work budget, not a result budget.",
        )
    };
    let scan_limit = if pushdown {
        candidate_overfetch
    } else {
        candidate_overfetch.min(MAX_ANN_SCAN_ROWS)
    };

    write!(
        sql,
        "
{note}
          ann_scan {barrier}
              SELECT emb.entity_kind, emb.entity_id, emb.model_id,
                     emb.embedding_version, emb.owner_kind, emb.owner_id,
{similarity}
                FROM proxima_core.embeddings emb
               WHERE emb.model_id = ${model_param}",
        similarity = similarity_score_sql(vec_param, 21)
    )
    .expect("write to String is infallible");
    if pushdown {
        // SQL-POLICY: fixed-fragment
        sql.push_str(EMBEDDING_OWNER_SCOPE_SQL);
    }
    write!(
        sql,
        "
               ORDER BY emb.vec <=> ${vec_param}::vector
               LIMIT {scan_limit}
          ),"
    )
    .expect("write to String is infallible");
}

/// Collapse the chunk-level rows to one row per memory, above the
/// nearest-neighbour cut either way: a memory scores by its best chunk, and
/// the outer LIMIT stays a budget of memories rather than of chunks.
///
/// `candidate_window_dedup` ranks in one pass instead of re-reading the
/// sorted set per group. `ann_ranked` carries no barrier of its own: it is
/// read once, and a window function already blocks the outer filter from
/// being pushed under it.
fn push_vector_candidates(sql: &mut String, tuning: &PgTuning, chunk: &ChunkRows) {
    let ChunkRows { row, score, from } = chunk;
    let barrier = if matches!(tuning.semantic_index_first, SemanticIndexFirst::Pushdown) {
        "AS ("
    } else {
        "AS MATERIALIZED ("
    };

    if tuning.candidate_window_dedup {
        write!(
            sql,
            "
          ann_ranked AS (
              SELECT {row}.memory_id, {row}.kind, {row}.schema_id, {row}.created_at,
                     {row}.search_text, {score}.similarity_score,
                     row_number() OVER (PARTITION BY {row}.kind, {row}.memory_id
                                        ORDER BY {score}.similarity_score DESC) AS rn
{from}
          ),
          vector_candidates {barrier}
              SELECT r.memory_id, r.kind, r.schema_id, r.created_at,
                     r.search_text, r.similarity_score
                FROM ann_ranked r
               WHERE r.rn = 1
          )"
        )
        .expect("write to String is infallible");
        return;
    }
    write!(
        sql,
        "
          vector_candidates {barrier}
              SELECT DISTINCT ON ({row}.kind, {row}.memory_id)
                     {row}.memory_id, {row}.kind, {row}.schema_id, {row}.created_at,
                     {row}.search_text, {score}.similarity_score
{from}
               ORDER BY {row}.kind, {row}.memory_id, {score}.similarity_score DESC
          )"
    )
    .expect("write to String is infallible");
}

/// The production HNSW session-settings statement, for EXPLAIN-based plan
/// assertions that must run under exactly the settings `run_semantic`
/// applies rather than a restated copy of them.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn set_hnsw_search_sql_for_tests(tuning: &PgTuning) -> String {
    set_hnsw_search_sql(tuning)
}

/// The semantic branch SQL for EXPLAIN-based plan assertions in tests.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn semantic_search_sql_for_tests(
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    limit: u32,
    candidate_overfetch: u64,
    tuning: &PgTuning,
) -> Result<String, StorageError> {
    semantic_branch_sql(req, projections, limit, candidate_overfetch, tuning).map(|(sql, _)| sql)
}

async fn run_semantic(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    limit: u32,
    candidate_overfetch: u64,
    tuning: &PgTuning,
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
        return Err(StorageError::ConstraintViolation(format!(
            "semantic search embedding length must be {EMBEDDING_DIM}"
        )));
    }

    let (sql, projections) =
        semantic_branch_sql(req, projections, limit, candidate_overfetch, tuning)?;

    // SQL-POLICY: PgIdent
    let mut q = bind_common(
        sqlx::query_as::<_, SearchRow>(sqlx::AssertSqlSafe(sql)),
        req,
        &projections,
    );
    q = bind_filter_params(q, req);
    q = q.bind(crate::pgvector::literal(query_embedding));
    q = q.bind(model_id.clone());

    let mut tx = pool.begin().await.map_err(map_err)?;
    // SQL-POLICY: fixed-fragment — the settings statement interpolates
    // nothing but this deployment's own tuning integers and enum spellings.
    sqlx::raw_sql(sqlx::AssertSqlSafe(set_hnsw_search_sql(tuning)))
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
    tuning: &PgTuning,
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
        supersedes_anti_join: tuning.candidate_window_dedup,
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
",
        // memories.search_tsv is generated from the same COALESCE(text, '')
        // this branch projects, so the column and the expression it
        // replaces are the same value by construction. lexical_language
        // rides along for the per-row rank tsquery.
        base_tsv = if include_tsv {
            ", m.search_tsv AS search_tsv, m.lexical_language AS lexical_language"
        } else {
            ""
        },
    )
    .expect("write to String is infallible");
    push_supersedes_anti_join(&mut sql, req, filters, base_branch_kinds(req));
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
                ", {} AS search_tsv, {} AS lexical_language",
                projection_tsv_expr(projection, &search_text_expr)?,
                projection_language_expr(projection)?
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
JOIN {table} s ON s.memory_id = m.memory_id",
            tag_expr = tag_expr.as_str(),
            table = table.as_str()
        )
        .expect("write to String is infallible");
        push_supersedes_anti_join(
            &mut sql,
            req,
            filters,
            sidecar_branch_kinds(projection.kind),
        );
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
        // `MemoryText` names no sidecar column, so it is resolved before
        // the identifier check rather than being made to pass one.
        // Every candidate branch joins `proxima_core.memories m` (see
        // the UNION ALL above), so `m.text` is in scope here.
        if matches!(field.kind, SearchProjectionColumnKind::MemoryText) {
            expressions.push("NULLIF(m.text, '')".to_string());
            continue;
        }
        let column = PgIdent::column(&field.column)?;
        let expression = match field.kind {
            SearchProjectionColumnKind::Text => {
                format!("NULLIF(s.{}::text, '')", column.as_str())
            }
            SearchProjectionColumnKind::TextArray => {
                format!("NULLIF(array_to_string(s.{}, ' '), '')", column.as_str())
            }
            SearchProjectionColumnKind::MemoryText => unreachable!("handled above"),
        };
        expressions.push(expression);
    }
    Ok(expressions.join(", "))
}

/// True when a branch's search text is exactly the owning memory's text,
/// ranked in that memory's own language — in which case the stored
/// `memories.search_tsv` *is* this branch's vector and there is nothing
/// to tokenise.
///
/// Migration 0014 generates that column as `lexical_tsv(lexical_language,
/// COALESCE(text, ''))`. This branch's search text is
/// `NULLIF(concat_ws(' ', NULLIF(m.text, '')), '')`, which equals
/// `COALESCE(m.text, '')` for every row with non-empty text and is NULL
/// for the rest — and a NULL search text and an empty tsvector are both
/// unmatchable, so the two spellings cannot rank a candidate differently.
///
/// A declared `language_column` breaks the equivalence: the sidecar then
/// pins a configuration the memories column was not tokenised with (the
/// code flavor pins `english`), so that case falls through to the inline
/// vector.
fn projection_is_memory_text_only(projection: &MemorySearchProjection) -> bool {
    projection.language_column.is_none()
        && matches!(
            projection.fields.as_slice(),
            [MemorySearchProjectionField {
                kind: SearchProjectionColumnKind::MemoryText,
                ..
            }]
        )
}

/// The candidate's lexical vector: the stored column when the sidecar
/// declares one, else the same vector computed inline.
///
/// Both spellings resolve to `proxima_core.lexical_tsv` over the branch's
/// projected search text — the stored column is generated from it (see
/// migrations 0011/0014), so a sidecar with a column and one without cannot
/// score differently for the same content. The inline fallback tokenises
/// with the owning memory row's language, which is what a stored sidecar
/// column mirrors at insert (0014's sidecar trigger).
fn projection_tsv_expr(
    projection: &MemorySearchProjection,
    search_text_expr: &str,
) -> Result<String, StorageError> {
    let Some(tsv_column) = &projection.tsv_column else {
        if projection_is_memory_text_only(projection) {
            return Ok("m.search_tsv".to_string());
        }
        return Ok(format!(
            "proxima_core.lexical_tsv(m.lexical_language, {search_text_expr})"
        ));
    };
    let column = PgIdent::column(tsv_column)?;
    Ok(format!("s.{}", column.as_str()))
}

/// The candidate's lexical language, for the per-row rank tsquery: the
/// sidecar's own column when declared (the code flavor pins `english`
/// there), else the owning memory row's. Must name the configuration the
/// branch's `search_tsv` was actually tokenised with, or ranking stems
/// the query differently from the document.
fn projection_language_expr(projection: &MemorySearchProjection) -> Result<String, StorageError> {
    let Some(language_column) = &projection.language_column else {
        return Ok("m.lexical_language".to_string());
    };
    let column = PgIdent::column(language_column)?;
    Ok(format!("s.{}", column.as_str()))
}

fn projection_tag_expr(projection: &MemorySearchProjection) -> Result<String, StorageError> {
    let Some(tag_column) = &projection.tag_column else {
        return Ok("NULL::text[]".to_string());
    };
    let column = PgIdent::column(tag_column)?;
    Ok(format!("s.{}", column.as_str()))
}

/// Every candidate branch reads `FROM proxima_core.memories m`, so the
/// candidate's home owner is `m`'s own owner columns. It used to be read
/// back off an `entity_owner_union` (memories ∪ goals) outer-joined on
/// `entity_id = m.memory_id` — for a memory row the union's memories arm
/// *is* `m`, and its goals arm can only match on a uuid collision between
/// a `goal_id` and a `memory_id`, which would corrupt the branch by
/// duplicating the row rather than help it. `publish_to_world` transfers
/// ownership with an UPDATE on `memories`, so `m.owner_kind`/`m.owner_id`
/// is the live home owner, not a stale copy.
fn push_candidate_branch_prefix(sql: &mut String) {
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        "SELECT m.memory_id, m.owner_kind, m.owner_id, \
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
///
/// The gate reads `m`'s own owner columns rather than looking the owner up
/// through an `entity_owner_union` (memories ∪ goals) keyed on
/// `entity_id = m.memory_id`. Every candidate branch selects
/// `FROM proxima_core.memories m`, so that lookup could only ever return
/// `m` itself, but it cost a scan of both `memories` and `goals` per
/// branch to prove it — and the planner drove the whole query off that
/// union, which put the text and vector predicates permanently out of
/// reach of any index. Measured on a 15,265-memory corpus with the six
/// projections a code-flavor deployment registers, six lexical queries:
/// 1,799.9 ms → 1,297.3 ms total (1.4×), with identical
/// `(memory_id, lexical_score)` result sets on every query.
fn push_read_owner_scope(sql: &mut String, req: &MemorySearchRequest) {
    // SQL-POLICY: fixed-fragment — the read set arrives as the two bound
    // arrays $1/$2; nothing here is interpolated.
    sql.push_str(
        " WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           )",
    );
    if req
        .read_owners
        .iter()
        .any(|owner| matches!(owner, OwnerRef::World))
    {
        // SQL-POLICY: fixed-fragment
        sql.push_str(" OR (m.owner_kind = 'world' AND m.owner_id IS NULL)");
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
    push_search_head_filter(sql, req, filters, base_branch_kinds(req));
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
    push_search_head_filter(sql, req, filters, sidecar_branch_kinds(kind));
}

fn push_payload_kind_filter(sql: &mut String, kind: PayloadKind) {
    match kind {
        PayloadKind::Fact => sql.push_str(" AND m.kind IS NULL"),
        PayloadKind::Abstraction => sql.push_str(" AND m.kind = 'Abstraction'"),
        PayloadKind::Perspective => sql.push_str(" AND m.kind = 'Perspective'"),
        PayloadKind::Goal | PayloadKind::CitedObject | PayloadKind::CitationMapping => {
            sql.push_str(" AND false");
        }
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

/// The kind population one candidate branch can hold, as pinned by the
/// kind predicate that branch already carries (`push_base_memory_filters`
/// via `req.kind`, `push_payload_kind_filter` via the sidecar's
/// `PayloadKind`): facts are `m.kind IS NULL`, derived rows
/// (`Abstraction` / `Perspective`) are `m.kind IS NOT NULL`. Under that
/// predicate the mixed head filter reduces to exactly one of its
/// disjuncts, which is what lets the dedup arm specialize the head SQL
/// per branch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BranchKinds {
    /// No kind predicate on the branch (or a `AND false` arm that holds no
    /// rows either way): both disjuncts of the head filter are reachable.
    Mixed,
    /// `AND m.kind IS NULL`: only the fact-entity head test can hold.
    FactOnly,
    /// `AND m.kind = '…'`: only the successor head test can hold.
    DerivedOnly,
}

/// The base (memories-only) candidate branch's kind population, from the
/// predicate `push_base_memory_filters` writes for `req.kind`.
fn base_branch_kinds(req: &MemorySearchRequest) -> BranchKinds {
    match req.kind {
        Some(EntityKind::Fact) => BranchKinds::FactOnly,
        Some(EntityKind::Abstraction | EntityKind::Perspective) => BranchKinds::DerivedOnly,
        // `Some(Goal)` writes `AND false`: the branch is empty under either
        // spelling, so it keeps the mixed one.
        None | Some(EntityKind::Goal) => BranchKinds::Mixed,
    }
}

/// A sidecar candidate branch's kind population, from the predicate
/// `push_payload_kind_filter` writes for the projection's kind.
fn sidecar_branch_kinds(kind: PayloadKind) -> BranchKinds {
    match kind {
        PayloadKind::Fact => BranchKinds::FactOnly,
        PayloadKind::Abstraction | PayloadKind::Perspective => BranchKinds::DerivedOnly,
        // These write `AND false`: empty branch, mixed spelling.
        PayloadKind::Goal | PayloadKind::CitedObject | PayloadKind::CitationMapping => {
            BranchKinds::Mixed
        }
    }
}

/// The successor set, joined once per branch instead of probed once per
/// candidate row. `idx_memories_supersedes_uq` is unique on `supersedes`, so
/// the join matches at most one row and cannot multiply a candidate — which
/// is what makes it interchangeable with the `NOT EXISTS` spelling rather
/// than merely equivalent in truth value.
///
/// A fact-only branch pins `m.kind IS NULL`, under which the head filter's
/// successor disjunct is statically false — `m2` would never be read — so
/// the join is not written at all there. The same uniqueness is what makes
/// that omission cardinality-neutral: a LEFT JOIN matching at most one row
/// can neither drop nor duplicate the branch's rows.
fn push_supersedes_anti_join(
    sql: &mut String,
    req: &MemorySearchRequest,
    filters: CandidateFilterParams,
    kinds: BranchKinds,
) {
    if !filters.supersedes_anti_join
        || matches!(req.supersession, SupersessionStatus::IncludeSuperseded)
        || kinds == BranchKinds::FactOnly
    {
        return;
    }
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        " LEFT JOIN proxima_core.memories m2 \
            ON m2.supersedes = m.memory_id \
           AND m2.tombstoned_at IS NULL",
    );
    super::push_same_home_owner_successor_predicate(sql, "m2", "m");
}

fn push_search_head_filter(
    sql: &mut String,
    req: &MemorySearchRequest,
    filters: CandidateFilterParams,
    kinds: BranchKinds,
) {
    if matches!(req.supersession, SupersessionStatus::IncludeSuperseded) {
        return;
    }
    // On the dedup arm only, a branch whose kind predicate already decides
    // the head filter's disjunct emits just that disjunct. The legacy
    // (`!supersedes_anti_join`) spelling below never specializes: its text
    // is the escape-hatch guarantee and must not move.
    if filters.supersedes_anti_join {
        match kinds {
            BranchKinds::FactOnly => {
                // SQL-POLICY: fixed-fragment
                sql.push_str(
                    " AND ( \
                        m.fact_entity_id IS NULL \
                        OR EXISTS ( \
                            SELECT 1 FROM proxima_core.fact_entities fe \
                             WHERE fe.fact_entity_id = m.fact_entity_id \
                               AND fe.current_memory_id = m.memory_id \
                        ) \
                    )",
                );
                return;
            }
            BranchKinds::DerivedOnly => {
                // SQL-POLICY: fixed-fragment — the successor arrives as the
                // join `push_supersedes_anti_join` wrote onto this branch.
                sql.push_str(" AND m2.memory_id IS NULL");
                return;
            }
            BranchKinds::Mixed => {}
        }
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
            OR (m.kind IS NOT NULL AND ",
    );
    if filters.supersedes_anti_join {
        // SQL-POLICY: fixed-fragment — the successor arrives as the join
        // `push_supersedes_anti_join` wrote onto this branch.
        sql.push_str(
            "m2.memory_id IS NULL) \
        )",
        );
        return;
    }
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        "NOT EXISTS ( \
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

/// The bind prefix both branches share, in the order the builders assign
/// parameters: the read-owner kind/id arrays, then each selected
/// projection's `(schema_id, schema_version)` pair.
fn bind_common<'q>(
    mut q: sqlx::query::QueryAs<'q, sqlx::Postgres, SearchRow, sqlx::postgres::PgArguments>,
    req: &'q MemorySearchRequest,
    projections: &[&MemorySearchProjection],
) -> sqlx::query::QueryAs<'q, sqlx::Postgres, SearchRow, sqlx::postgres::PgArguments> {
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(&req.read_owners);
    q = q.bind(read_owner_kinds);
    q = q.bind(read_owner_ids);
    for projection in projections {
        q = q.bind(projection.schema_id.as_str().to_string());
        q = q.bind(projection.schema_version.into_inner().cast_signed());
    }
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

/// Golden SQL for the search branches. Two contracts are pinned here:
///
/// - **Default** tuning (index-first pushdown + window dedup) must emit the
///   shipped statements byte-for-byte, so a flag added later is provably
///   inert by default.
/// - The **escape hatch** — `PROXIMA_PG_SEMANTIC_INDEX_FIRST=off` plus
///   `PROXIMA_PG_CANDIDATE_WINDOW_DEDUP=off` — must emit the legacy
///   statements UNCHANGED. Those goldens are the guarantee that the legacy
///   result membership is still reachable; do not regenerate them.
#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::UserId;
    use proxima_core::verbs::schema::MemorySearchProjectionField;

    /// The window sizes `search_memories` derives for `limit = 10`.
    const GOLDEN_LIMIT: u32 = 44;
    const GOLDEN_CANDIDATE_OVERFETCH: u64 = 704;

    /// One request that reaches every structural arm the builders have:
    /// a World member in the read set, both time bounds, an ALL tag match,
    /// heads-only supersession, and relevance order.
    fn golden_request(mode: SearchMode) -> MemorySearchRequest {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::from_u128(0x1111_2222_3333_4444)));
        MemorySearchRequest {
            owner,
            read_owners: vec![owner, OwnerRef::World],
            query: "golden probe".into(),
            mode,
            supersession: SupersessionStatus::HeadsOnly,
            limit: 10,
            kind: None,
            schema_id: None,
            tags: vec!["alpha".into(), "beta".into()],
            tag_match: TagMatch::All,
            since: Some(time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()),
            until: Some(time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()),
            order: SearchOrder::Relevance,
            min_score: None,
            semantic_weight: None,
            after: None,
            query_embedding: Some(vec![0.0; EMBEDDING_DIM]),
            embedding_model_id: Some("golden-embed".into()),
        }
    }

    /// Both sidecar shapes in one list: a Fact projection carrying its own
    /// stored tsvector, tag and language columns, and an Abstraction
    /// projection carrying none of them (inline tokenisation).
    fn golden_projections() -> Vec<MemorySearchProjection> {
        vec![
            MemorySearchProjection {
                schema_id: SchemaId::new("core/agent-note-v1".into()),
                schema_version: proxima_core::SchemaVersion::new(1),
                kind: PayloadKind::Fact,
                sidecar_table: "proxima_core.agent_note_v1".into(),
                fields: vec![
                    MemorySearchProjectionField {
                        column: "title".into(),
                        kind: SearchProjectionColumnKind::Text,
                    },
                    MemorySearchProjectionField {
                        column: "tags".into(),
                        kind: SearchProjectionColumnKind::TextArray,
                    },
                ],
                tag_column: Some("tags".into()),
                tsv_column: Some("search_tsv".into()),
                language_column: Some("lexical_language".into()),
            },
            MemorySearchProjection {
                schema_id: SchemaId::new("core/interpretation-v1".into()),
                schema_version: proxima_core::SchemaVersion::new(2),
                kind: PayloadKind::Abstraction,
                sidecar_table: "proxima_core.interpretation_v1".into(),
                fields: vec![MemorySearchProjectionField {
                    column: "claim".into(),
                    kind: SearchProjectionColumnKind::Text,
                }],
                tag_column: None,
                tsv_column: None,
                language_column: None,
            },
        ]
    }

    /// The legacy configuration, exactly as the environment escape hatch
    /// (`PROXIMA_PG_SEMANTIC_INDEX_FIRST=off`,
    /// `PROXIMA_PG_CANDIDATE_WINDOW_DEDUP=off`) selects it.
    fn legacy_tuning() -> PgTuning {
        PgTuning {
            semantic_index_first: SemanticIndexFirst::Off,
            candidate_window_dedup: false,
            ..PgTuning::default()
        }
    }

    #[test]
    fn semantic_branch_sql_at_default_tuning_is_byte_identical() {
        let (sql, _) = semantic_branch_sql(
            &golden_request(SearchMode::Semantic),
            &golden_projections(),
            GOLDEN_LIMIT,
            GOLDEN_CANDIDATE_OVERFETCH,
            &PgTuning::default(),
        )
        .unwrap();

        assert_eq!(sql, SEMANTIC_BRANCH_DEFAULT_GOLDEN);
    }

    #[test]
    fn lexical_branch_sql_at_default_tuning_is_byte_identical() {
        let (sql, _) = lexical_branch_sql(
            &golden_request(SearchMode::Lexical),
            &golden_projections(),
            GOLDEN_LIMIT,
            &PgTuning::default(),
        )
        .unwrap();

        assert_eq!(sql, LEXICAL_BRANCH_DEFAULT_GOLDEN);
    }

    /// The escape-hatch guarantee: the explicit legacy configuration emits
    /// the statements that shipped before the tuning surface existed,
    /// byte-for-byte. These golden strings must never be regenerated.
    #[test]
    fn the_legacy_escape_hatch_emits_the_legacy_text_unchanged() {
        let (semantic, _) = semantic_branch_sql(
            &golden_request(SearchMode::Semantic),
            &golden_projections(),
            GOLDEN_LIMIT,
            GOLDEN_CANDIDATE_OVERFETCH,
            &legacy_tuning(),
        )
        .unwrap();
        let (lexical, _) = lexical_branch_sql(
            &golden_request(SearchMode::Lexical),
            &golden_projections(),
            GOLDEN_LIMIT,
            &legacy_tuning(),
        )
        .unwrap();

        assert_eq!(semantic, SEMANTIC_BRANCH_LEGACY_GOLDEN);
        assert_eq!(lexical, LEXICAL_BRANCH_LEGACY_GOLDEN);
    }

    #[test]
    fn common_candidates_sql_is_pinned_per_arm() {
        let projections = golden_projections();
        let selected: Vec<&MemorySearchProjection> = projections.iter().collect();
        for (tuning, golden) in [
            (PgTuning::default(), COMMON_CANDIDATES_DEFAULT_GOLDEN),
            (legacy_tuning(), COMMON_CANDIDATES_LEGACY_GOLDEN),
        ] {
            let mut next_param = 3;
            let sql = common_candidates_sql(
                &golden_request(SearchMode::Lexical),
                &selected,
                &mut next_param,
                true,
                &tuning,
            )
            .unwrap();

            assert_eq!(sql, golden);
            assert_eq!(next_param, 10);
        }
    }

    /// Every arm a deployment can select is pinned too: the SQL text *is*
    /// the behaviour being selected, so an arm may only move when its
    /// golden moves with it.
    #[test]
    fn every_tuned_semantic_arm_is_pinned() {
        for (index_first, window_dedup, golden) in [
            (
                SemanticIndexFirst::Off,
                false,
                SEMANTIC_BRANCH_LEGACY_GOLDEN,
            ),
            (
                SemanticIndexFirst::Overfetch,
                false,
                SEMANTIC_INDEX_FIRST_OVERFETCH_GOLDEN,
            ),
            (
                SemanticIndexFirst::Pushdown,
                false,
                SEMANTIC_INDEX_FIRST_PUSHDOWN_GOLDEN,
            ),
            (SemanticIndexFirst::Off, true, SEMANTIC_WINDOW_DEDUP_GOLDEN),
            (
                SemanticIndexFirst::Overfetch,
                true,
                SEMANTIC_INDEX_FIRST_OVERFETCH_WINDOW_DEDUP_GOLDEN,
            ),
            (
                SemanticIndexFirst::Pushdown,
                true,
                SEMANTIC_BRANCH_DEFAULT_GOLDEN,
            ),
        ] {
            let tuning = PgTuning {
                semantic_index_first: index_first,
                candidate_window_dedup: window_dedup,
                ..PgTuning::default()
            };

            let (sql, _) = semantic_branch_sql(
                &golden_request(SearchMode::Semantic),
                &golden_projections(),
                GOLDEN_LIMIT,
                GOLDEN_CANDIDATE_OVERFETCH,
                &tuning,
            )
            .unwrap();

            assert_eq!(sql, golden, "{index_first:?}, window dedup {window_dedup}");
        }
    }

    /// A request that asks for superseded rows runs no head filter, so the
    /// flag has no probe to replace and writes no join.
    #[test]
    fn include_superseded_leaves_the_successor_join_unwritten() {
        let mut req = golden_request(SearchMode::Lexical);
        req.supersession = SupersessionStatus::IncludeSuperseded;
        let projections = golden_projections();
        let selected: Vec<&MemorySearchProjection> = projections.iter().collect();

        let mut sql = Vec::new();
        for candidate_window_dedup in [false, true] {
            let mut next_param = 3;
            sql.push(
                common_candidates_sql(
                    &req,
                    &selected,
                    &mut next_param,
                    true,
                    &PgTuning {
                        candidate_window_dedup,
                        ..PgTuning::default()
                    },
                )
                .unwrap(),
            );
        }

        assert!(!sql[1].contains("LEFT JOIN"));
        assert_eq!(sql[0], sql[1]);
    }

    /// `semantic_index_first` moves the vector scan and nothing else: the
    /// lexical branch is the same query in every arm.
    #[test]
    fn index_first_leaves_the_lexical_branch_byte_identical() {
        for index_first in [SemanticIndexFirst::Off, SemanticIndexFirst::Overfetch] {
            let (sql, _) = lexical_branch_sql(
                &golden_request(SearchMode::Lexical),
                &golden_projections(),
                GOLDEN_LIMIT,
                &PgTuning {
                    semantic_index_first: index_first,
                    ..PgTuning::default()
                },
            )
            .unwrap();

            assert_eq!(sql, LEXICAL_BRANCH_DEFAULT_GOLDEN, "{index_first:?}");
        }
    }

    /// Only the overfetch arm caps its window: behind the barrier the window
    /// is spent on rows the joins above may all discard, while the pushdown
    /// arm and the off branch spend theirs on rows that already passed the
    /// owner scope.
    #[test]
    fn only_the_overfetch_arm_caps_its_scan_window() {
        let asked = MAX_ANN_SCAN_ROWS * 3;
        for (index_first, expected) in [
            (SemanticIndexFirst::Off, asked),
            (SemanticIndexFirst::Overfetch, MAX_ANN_SCAN_ROWS),
            (SemanticIndexFirst::Pushdown, asked),
        ] {
            let (sql, _) = semantic_branch_sql(
                &golden_request(SearchMode::Semantic),
                &golden_projections(),
                GOLDEN_LIMIT,
                asked,
                &PgTuning {
                    semantic_index_first: index_first,
                    ..PgTuning::default()
                },
            )
            .unwrap();

            assert!(
                sql.contains(&format!("LIMIT {expected}\n")),
                "{index_first:?} asked {asked}"
            );
        }
    }

    /// The legacy semantic branch — the text `PROXIMA_PG_SEMANTIC_INDEX_FIRST=off`
    /// plus `PROXIMA_PG_CANDIDATE_WINDOW_DEDUP=off` restores. Never regenerate.
    const SEMANTIC_BRANCH_LEGACY_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text FROM proxima_core.memories m  WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) )) , eligible_entities AS MATERIALIZED (
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
                             WHEN (1 - (emb.vec <=> $10::vector)) = 'NaN'::float8 THEN 0.0
                             ELSE GREATEST(0.0, (1 - (emb.vec <=> $10::vector)))
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
                   WHERE emb.model_id = $11
                   ORDER BY emb.vec <=> $10::vector
                   LIMIT 704
                ) ann
               ORDER BY ann.kind, ann.memory_id, ann.similarity_score DESC
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 c.similarity_score
          FROM vector_candidates c
          ORDER BY similarity_score DESC, c.memory_id DESC
          LIMIT 44";

    /// The DEFAULT lexical branch: same tail as the legacy branch over the
    /// window-dedup candidate CTE.
    const LEXICAL_BRANCH_DEFAULT_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, m.search_tsv AS search_tsv, m.lexical_language AS lexical_language FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, s.search_tsv AS search_tsv, s.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) AS search_tsv, m.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND m2.memory_id IS NULL) , scrubbed AS (
               -- The same scrub every stored `search_tsv` went through
               -- (`lexical_tsv` = `to_tsvector(config, lexical_scrub(txt))`).
               -- Called, not restated: a query token that keeps punctuation
               -- the document side dropped can never match the stored
               -- lexeme, and that failure is silent.
               SELECT proxima_core.lexical_scrub($10) AS q
          )
          , q AS (
               -- One tsquery per active language, OR-combined: the match
               -- side cannot know the query's language, and the OR is
               -- GIN-indexable where a per-row-parsed tsquery is not.
               -- lexical_query_text stop-filters the text for stop-list-free
               -- configurations (simple), so one CJK row in the corpus
               -- cannot turn every query's function words into match terms.
               -- tsquery_or_agg over an empty lexical_languages is NULL;
               -- the COALESCE falls back to the default configuration.
               SELECT s.q AS scrubbed,
                      COALESCE(
                          (SELECT proxima_core.tsquery_or_agg(
                                      websearch_to_tsquery(l.config,
                                          proxima_core.lexical_query_text(l.config, s.q))
                                      ORDER BY l.config)
                             FROM proxima_core.lexical_languages l),
                          websearch_to_tsquery(proxima_core.lexical_config(), s.q)
                      ) AS tsq,
                      -- OR-rescue arm: the same content lexemes any-matched.
                      -- plainto_tsquery emits only '&' between lexemes (no
                      -- phrase or negation operators), so the operator swap
                      -- is safe. NULLIF folds a no-lexeme language out; the
                      -- STRICT transition function skips those NULLs.
                      COALESCE(
                          (SELECT proxima_core.tsquery_or_agg(
                                      NULLIF(
                                          replace(plainto_tsquery(l.config,
                                              proxima_core.lexical_query_text(
                                                  l.config, s.q))::text,
                                                  ' & ', ' | '),
                                          '')::tsquery
                                      ORDER BY l.config)
                             FROM proxima_core.lexical_languages l),
                          NULLIF(
                              replace(plainto_tsquery(proxima_core.lexical_config(), s.q)::text,
                                      ' & ', ' | '),
                              '')::tsquery
                      ) AS any_tsq
                 FROM scrubbed s
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 GREATEST(
                     CASE WHEN c.search_tsv @@ q.tsq
                          THEN 0.5 + LEAST(COALESCE(ts_rank_cd(c.search_tsv,
                                   websearch_to_tsquery(c.lexical_language,
                                       proxima_core.lexical_query_text(
                                           c.lexical_language, q.scrubbed)),
                                   32), 0.0), 1.0) * 0.5
                          ELSE 0.0 END, CASE WHEN c.search_tsv @@ q.any_tsq
                THEN 0.25 + LEAST(COALESCE(ts_rank(c.search_tsv,
                         NULLIF(replace(plainto_tsquery(c.lexical_language,
                                            proxima_core.lexical_query_text(
                                                c.lexical_language, q.scrubbed))::text,
                                        ' & ', ' | '), '')::tsquery,
                         1|32), 0.0) * 100.0, 1.0) * 0.2
                ELSE 0.0 END,
                     CASE WHEN lower(c.search_text) LIKE '%' || replace(replace(replace(lower($10), '\\', '\\\\'), '%', '\\%'), '_', '\\_') || '%' ESCAPE '\'
                          THEN 0.25 ELSE 0.0 END
                 )::real AS lexical_score,
                 0.0::real AS similarity_score
          FROM candidates c, q
          WHERE c.search_text <> ''
            AND (
                c.search_tsv @@ q.tsq OR c.search_tsv @@ q.any_tsq
                OR lower(c.search_text) LIKE '%' || replace(replace(replace(lower($10), '\\', '\\\\'), '%', '\\%'), '_', '\\_') || '%' ESCAPE '\'
            )
          ORDER BY lexical_score DESC, c.memory_id DESC
          LIMIT 44";

    /// The legacy lexical branch — the text the escape hatch restores.
    /// Never regenerate.
    const LEXICAL_BRANCH_LEGACY_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, m.search_tsv AS search_tsv, m.lexical_language AS lexical_language FROM proxima_core.memories m  WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, s.search_tsv AS search_tsv, s.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) AS search_tsv, m.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) )) , scrubbed AS (
               -- The same scrub every stored `search_tsv` went through
               -- (`lexical_tsv` = `to_tsvector(config, lexical_scrub(txt))`).
               -- Called, not restated: a query token that keeps punctuation
               -- the document side dropped can never match the stored
               -- lexeme, and that failure is silent.
               SELECT proxima_core.lexical_scrub($10) AS q
          )
          , q AS (
               -- One tsquery per active language, OR-combined: the match
               -- side cannot know the query's language, and the OR is
               -- GIN-indexable where a per-row-parsed tsquery is not.
               -- lexical_query_text stop-filters the text for stop-list-free
               -- configurations (simple), so one CJK row in the corpus
               -- cannot turn every query's function words into match terms.
               -- tsquery_or_agg over an empty lexical_languages is NULL;
               -- the COALESCE falls back to the default configuration.
               SELECT s.q AS scrubbed,
                      COALESCE(
                          (SELECT proxima_core.tsquery_or_agg(
                                      websearch_to_tsquery(l.config,
                                          proxima_core.lexical_query_text(l.config, s.q))
                                      ORDER BY l.config)
                             FROM proxima_core.lexical_languages l),
                          websearch_to_tsquery(proxima_core.lexical_config(), s.q)
                      ) AS tsq,
                      -- OR-rescue arm: the same content lexemes any-matched.
                      -- plainto_tsquery emits only '&' between lexemes (no
                      -- phrase or negation operators), so the operator swap
                      -- is safe. NULLIF folds a no-lexeme language out; the
                      -- STRICT transition function skips those NULLs.
                      COALESCE(
                          (SELECT proxima_core.tsquery_or_agg(
                                      NULLIF(
                                          replace(plainto_tsquery(l.config,
                                              proxima_core.lexical_query_text(
                                                  l.config, s.q))::text,
                                                  ' & ', ' | '),
                                          '')::tsquery
                                      ORDER BY l.config)
                             FROM proxima_core.lexical_languages l),
                          NULLIF(
                              replace(plainto_tsquery(proxima_core.lexical_config(), s.q)::text,
                                      ' & ', ' | '),
                              '')::tsquery
                      ) AS any_tsq
                 FROM scrubbed s
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 GREATEST(
                     CASE WHEN c.search_tsv @@ q.tsq
                          THEN 0.5 + LEAST(COALESCE(ts_rank_cd(c.search_tsv,
                                   websearch_to_tsquery(c.lexical_language,
                                       proxima_core.lexical_query_text(
                                           c.lexical_language, q.scrubbed)),
                                   32), 0.0), 1.0) * 0.5
                          ELSE 0.0 END, CASE WHEN c.search_tsv @@ q.any_tsq
                THEN 0.25 + LEAST(COALESCE(ts_rank(c.search_tsv,
                         NULLIF(replace(plainto_tsquery(c.lexical_language,
                                            proxima_core.lexical_query_text(
                                                c.lexical_language, q.scrubbed))::text,
                                        ' & ', ' | '), '')::tsquery,
                         1|32), 0.0) * 100.0, 1.0) * 0.2
                ELSE 0.0 END,
                     CASE WHEN lower(c.search_text) LIKE '%' || replace(replace(replace(lower($10), '\\', '\\\\'), '%', '\\%'), '_', '\\_') || '%' ESCAPE '\'
                          THEN 0.25 ELSE 0.0 END
                 )::real AS lexical_score,
                 0.0::real AS similarity_score
          FROM candidates c, q
          WHERE c.search_text <> ''
            AND (
                c.search_tsv @@ q.tsq OR c.search_tsv @@ q.any_tsq
                OR lower(c.search_text) LIKE '%' || replace(replace(replace(lower($10), '\\', '\\\\'), '%', '\\%'), '_', '\\_') || '%' ESCAPE '\'
            )
          ORDER BY lexical_score DESC, c.memory_id DESC
          LIMIT 44";

    /// The legacy candidate CTE — per-row `NOT EXISTS` successor probe.
    /// Never regenerate.
    const COMMON_CANDIDATES_LEGACY_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, m.search_tsv AS search_tsv, m.lexical_language AS lexical_language FROM proxima_core.memories m  WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, s.search_tsv AS search_tsv, s.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) AS search_tsv, m.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ))";

    const SEMANTIC_INDEX_FIRST_OVERFETCH_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text FROM proxima_core.memories m  WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) )) , eligible_entities AS MATERIALIZED (
              SELECT DISTINCT ON (c.kind, c.memory_id)
                     c.memory_id, c.owner_kind, c.owner_id, c.kind,
                     c.schema_id, c.created_at, c.search_text
                FROM candidates c
               ORDER BY c.kind, c.memory_id, c.created_at DESC
          ),
          -- Index-first (overfetch): the scan's window is spent before
          -- eligibility is known, so it is a work budget, not a result budget.
          ann_scan AS MATERIALIZED (
              SELECT emb.entity_kind, emb.entity_id, emb.model_id,
                     emb.embedding_version, emb.owner_kind, emb.owner_id,
                     CASE
                         WHEN (1 - (emb.vec <=> $10::vector)) = 'NaN'::float8 THEN 0.0
                         ELSE GREATEST(0.0, (1 - (emb.vec <=> $10::vector)))
                     END::real AS similarity_score
                FROM proxima_core.embeddings emb
               WHERE emb.model_id = $11
               ORDER BY emb.vec <=> $10::vector
               LIMIT 704
          ),
          vector_candidates AS MATERIALIZED (
              SELECT DISTINCT ON (c.kind, c.memory_id)
                     c.memory_id, c.kind, c.schema_id, c.created_at,
                     c.search_text, ann.similarity_score
                FROM ann_scan ann
                JOIN proxima_core.embedding_heads head
                  ON head.entity_kind = ann.entity_kind
                 AND head.entity_id = ann.entity_id
                 AND head.model_id = ann.model_id
                 AND head.embedding_version = ann.embedding_version
                 AND head.owner_kind = ann.owner_kind
                 AND head.owner_id IS NOT DISTINCT FROM ann.owner_id
                JOIN eligible_entities c
                  ON c.kind = ann.entity_kind
                 AND c.memory_id = ann.entity_id
                 AND c.owner_kind = ann.owner_kind
                 AND c.owner_id IS NOT DISTINCT FROM ann.owner_id
               ORDER BY c.kind, c.memory_id, ann.similarity_score DESC
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 c.similarity_score
          FROM vector_candidates c
          ORDER BY similarity_score DESC, c.memory_id DESC
          LIMIT 44";

    const SEMANTIC_INDEX_FIRST_PUSHDOWN_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text FROM proxima_core.memories m  WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) )) , eligible_entities AS MATERIALIZED (
              SELECT DISTINCT ON (c.kind, c.memory_id)
                     c.memory_id, c.owner_kind, c.owner_id, c.kind,
                     c.schema_id, c.created_at, c.search_text
                FROM candidates c
               ORDER BY c.kind, c.memory_id, c.created_at DESC
          ),
          -- Index-first (pushdown): owner and model arms ride on the scan
          -- itself and nothing materializes above it.
          ann_scan AS (
              SELECT emb.entity_kind, emb.entity_id, emb.model_id,
                     emb.embedding_version, emb.owner_kind, emb.owner_id,
                     CASE
                         WHEN (1 - (emb.vec <=> $10::vector)) = 'NaN'::float8 THEN 0.0
                         ELSE GREATEST(0.0, (1 - (emb.vec <=> $10::vector)))
                     END::real AS similarity_score
                FROM proxima_core.embeddings emb
               WHERE emb.model_id = $11
                 AND EXISTS (
                       SELECT 1
                         FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                        WHERE emb.owner_kind = s.kind AND emb.owner_id = s.id
                     )
               ORDER BY emb.vec <=> $10::vector
               LIMIT 704
          ),
          vector_candidates AS (
              SELECT DISTINCT ON (c.kind, c.memory_id)
                     c.memory_id, c.kind, c.schema_id, c.created_at,
                     c.search_text, ann.similarity_score
                FROM ann_scan ann
                JOIN proxima_core.embedding_heads head
                  ON head.entity_kind = ann.entity_kind
                 AND head.entity_id = ann.entity_id
                 AND head.model_id = ann.model_id
                 AND head.embedding_version = ann.embedding_version
                 AND head.owner_kind = ann.owner_kind
                 AND head.owner_id IS NOT DISTINCT FROM ann.owner_id
                JOIN eligible_entities c
                  ON c.kind = ann.entity_kind
                 AND c.memory_id = ann.entity_id
                 AND c.owner_kind = ann.owner_kind
                 AND c.owner_id IS NOT DISTINCT FROM ann.owner_id
               ORDER BY c.kind, c.memory_id, ann.similarity_score DESC
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 c.similarity_score
          FROM vector_candidates c
          ORDER BY similarity_score DESC, c.memory_id DESC
          LIMIT 44";

    const SEMANTIC_WINDOW_DEDUP_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND m2.memory_id IS NULL) , eligible_entities AS MATERIALIZED (
              SELECT e.memory_id, e.owner_kind, e.owner_id, e.kind,
                     e.schema_id, e.created_at, e.search_text
                FROM (
                  SELECT c.memory_id, c.owner_kind, c.owner_id, c.kind,
                         c.schema_id, c.created_at, c.search_text,
                         row_number() OVER (PARTITION BY c.kind, c.memory_id
                                            ORDER BY c.created_at DESC) AS rn
                    FROM candidates c
                ) e
               WHERE e.rn = 1
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
          ann_ranked AS (
              SELECT ann.memory_id, ann.kind, ann.schema_id, ann.created_at,
                     ann.search_text, ann.similarity_score,
                     row_number() OVER (PARTITION BY ann.kind, ann.memory_id
                                        ORDER BY ann.similarity_score DESC) AS rn
                FROM (
                  SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                         c.search_text,
                         CASE
                             WHEN (1 - (emb.vec <=> $10::vector)) = 'NaN'::float8 THEN 0.0
                             ELSE GREATEST(0.0, (1 - (emb.vec <=> $10::vector)))
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
                   WHERE emb.model_id = $11
                   ORDER BY emb.vec <=> $10::vector
                   LIMIT 704
                ) ann
          ),
          vector_candidates AS MATERIALIZED (
              SELECT r.memory_id, r.kind, r.schema_id, r.created_at,
                     r.search_text, r.similarity_score
                FROM ann_ranked r
               WHERE r.rn = 1
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 c.similarity_score
          FROM vector_candidates c
          ORDER BY similarity_score DESC, c.memory_id DESC
          LIMIT 44";

    const SEMANTIC_INDEX_FIRST_OVERFETCH_WINDOW_DEDUP_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND m2.memory_id IS NULL) , eligible_entities AS MATERIALIZED (
              SELECT e.memory_id, e.owner_kind, e.owner_id, e.kind,
                     e.schema_id, e.created_at, e.search_text
                FROM (
                  SELECT c.memory_id, c.owner_kind, c.owner_id, c.kind,
                         c.schema_id, c.created_at, c.search_text,
                         row_number() OVER (PARTITION BY c.kind, c.memory_id
                                            ORDER BY c.created_at DESC) AS rn
                    FROM candidates c
                ) e
               WHERE e.rn = 1
          ),
          -- Index-first (overfetch): the scan's window is spent before
          -- eligibility is known, so it is a work budget, not a result budget.
          ann_scan AS MATERIALIZED (
              SELECT emb.entity_kind, emb.entity_id, emb.model_id,
                     emb.embedding_version, emb.owner_kind, emb.owner_id,
                     CASE
                         WHEN (1 - (emb.vec <=> $10::vector)) = 'NaN'::float8 THEN 0.0
                         ELSE GREATEST(0.0, (1 - (emb.vec <=> $10::vector)))
                     END::real AS similarity_score
                FROM proxima_core.embeddings emb
               WHERE emb.model_id = $11
               ORDER BY emb.vec <=> $10::vector
               LIMIT 704
          ),
          ann_ranked AS (
              SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                     c.search_text, ann.similarity_score,
                     row_number() OVER (PARTITION BY c.kind, c.memory_id
                                        ORDER BY ann.similarity_score DESC) AS rn
                FROM ann_scan ann
                JOIN proxima_core.embedding_heads head
                  ON head.entity_kind = ann.entity_kind
                 AND head.entity_id = ann.entity_id
                 AND head.model_id = ann.model_id
                 AND head.embedding_version = ann.embedding_version
                 AND head.owner_kind = ann.owner_kind
                 AND head.owner_id IS NOT DISTINCT FROM ann.owner_id
                JOIN eligible_entities c
                  ON c.kind = ann.entity_kind
                 AND c.memory_id = ann.entity_id
                 AND c.owner_kind = ann.owner_kind
                 AND c.owner_id IS NOT DISTINCT FROM ann.owner_id
          ),
          vector_candidates AS MATERIALIZED (
              SELECT r.memory_id, r.kind, r.schema_id, r.created_at,
                     r.search_text, r.similarity_score
                FROM ann_ranked r
               WHERE r.rn = 1
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 c.similarity_score
          FROM vector_candidates c
          ORDER BY similarity_score DESC, c.memory_id DESC
          LIMIT 44";

    /// The DEFAULT semantic branch: index-first pushdown + window dedup.
    const SEMANTIC_BRANCH_DEFAULT_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND m2.memory_id IS NULL) , eligible_entities AS MATERIALIZED (
              SELECT e.memory_id, e.owner_kind, e.owner_id, e.kind,
                     e.schema_id, e.created_at, e.search_text
                FROM (
                  SELECT c.memory_id, c.owner_kind, c.owner_id, c.kind,
                         c.schema_id, c.created_at, c.search_text,
                         row_number() OVER (PARTITION BY c.kind, c.memory_id
                                            ORDER BY c.created_at DESC) AS rn
                    FROM candidates c
                ) e
               WHERE e.rn = 1
          ),
          -- Index-first (pushdown): owner and model arms ride on the scan
          -- itself and nothing materializes above it.
          ann_scan AS (
              SELECT emb.entity_kind, emb.entity_id, emb.model_id,
                     emb.embedding_version, emb.owner_kind, emb.owner_id,
                     CASE
                         WHEN (1 - (emb.vec <=> $10::vector)) = 'NaN'::float8 THEN 0.0
                         ELSE GREATEST(0.0, (1 - (emb.vec <=> $10::vector)))
                     END::real AS similarity_score
                FROM proxima_core.embeddings emb
               WHERE emb.model_id = $11
                 AND EXISTS (
                       SELECT 1
                         FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                        WHERE emb.owner_kind = s.kind AND emb.owner_id = s.id
                     )
               ORDER BY emb.vec <=> $10::vector
               LIMIT 704
          ),
          ann_ranked AS (
              SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                     c.search_text, ann.similarity_score,
                     row_number() OVER (PARTITION BY c.kind, c.memory_id
                                        ORDER BY ann.similarity_score DESC) AS rn
                FROM ann_scan ann
                JOIN proxima_core.embedding_heads head
                  ON head.entity_kind = ann.entity_kind
                 AND head.entity_id = ann.entity_id
                 AND head.model_id = ann.model_id
                 AND head.embedding_version = ann.embedding_version
                 AND head.owner_kind = ann.owner_kind
                 AND head.owner_id IS NOT DISTINCT FROM ann.owner_id
                JOIN eligible_entities c
                  ON c.kind = ann.entity_kind
                 AND c.memory_id = ann.entity_id
                 AND c.owner_kind = ann.owner_kind
                 AND c.owner_id IS NOT DISTINCT FROM ann.owner_id
          ),
          vector_candidates AS (
              SELECT r.memory_id, r.kind, r.schema_id, r.created_at,
                     r.search_text, r.similarity_score
                FROM ann_ranked r
               WHERE r.rn = 1
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 c.similarity_score
          FROM vector_candidates c
          ORDER BY similarity_score DESC, c.memory_id DESC
          LIMIT 44";

    /// The DEFAULT candidate CTE: unique-join successor anti-join.
    const COMMON_CANDIDATES_DEFAULT_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, m.search_tsv AS search_tsv, m.lexical_language AS lexical_language FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind IS NULL AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind IS NOT NULL AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, s.search_tsv AS search_tsv, s.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) AS search_tsv, m.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND m2.memory_id IS NULL)";
}
