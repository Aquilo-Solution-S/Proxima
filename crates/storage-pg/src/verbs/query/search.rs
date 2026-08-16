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
/// Stems tokens and drops stopwords. An all-stopword query yields an empty
/// tsquery and falls back to the substring `LIKE` arm.
///
/// Fallback only when `lexical_languages` is empty. Otherwise:
/// - **Match** with the OR of one `websearch_to_tsquery` per active
///   language. Constant-config tsqueries fold to one OR the GIN can serve;
///   `websearch_to_tsquery(c.lexical_language, …)` in WHERE has no index.
/// - **Rank** each candidate with its own row's configuration. Ranking
///   against the OR query inflates wrong-config covers; a cross-config
///   strict match with no row-config cover scores the bare band base
///   (`COALESCE(…, 0)`).
const TEXT_SEARCH_CONFIG: &str = "proxima_core.lexical_config()";

/// SQL that lowercases a bound query parameter and neutralises the `LIKE`
/// metacharacters inside it, for use between `'%' || … || '%'` with
/// `ESCAPE '\'`.
///
/// Escape LIKE metacharacters for `'%' || … || '%'` with `ESCAPE '\'`.
/// `%` and `_` in the query are characters, not wildcards. Backslash first,
/// so an already-backslashed query cannot smuggle an escape.
fn like_literal(query_param: usize) -> String {
    format!(
        "replace(replace(replace(lower(${query_param}), '\\\\', '\\\\\\\\'), '%', '\\\\%'), '_', '\\\\_')"
    )
}

// Lexical score bands. Websearch AND semantics require every content word
// in one memory, so an OR-rescue arm re-runs the same lexemes any-matched.
// Match *tier* must dominate cover-density rank — hence disjoint bands:
// strict [0.5, 1.0] > rescue (0.25, 0.45] > substring LIKE 0.25.
//
// `ts_rank_cd` uses flag 32 (÷ self+1). Unlabelled docs are weight D;
// cover density ≥ 0.1, so `* 10` saturates at 1.0 and in-band order is
// whatever the plan emits. Flag 32 keeps the term in [0, 1).
// Rescue ranks with `ts_rank(v, q, 1|32)`: cover density rewards
// repetitive short spans; flag 1 divides by log document length.
// Strict keeps cover density (AND-match almost never fires on
// multi-sentence queries).

/// The flat score a substring-only match earns.
///
/// Also the whole substring band: unlike the two tsquery bands it carries
/// no rank term, so every substring-only row scores exactly this. That is
/// what makes the band skippable — a page already holding its full width
/// of rows scoring *strictly* above it cannot be changed by rows that can
/// only ever equal it. See [`needs_substring_pass`].
const SUBSTRING_BAND: f32 = 0.25;

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
    let _ = (projections, tuning);
    return search_memories_timeseries(pool, req).await;
    #[allow(unreachable_code)]
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

    let semantic_weight = req
        .semantic_weight
        .unwrap_or(DEFAULT_HYBRID_SEMANTIC_WEIGHT)
        .clamp(0.0, 1.0);

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

    // The substring band, read from the corpus only when the candidates in
    // hand cannot already outrank everything it could produce.
    //
    // This is a second statement under its own snapshot, so a row written
    // between the two is visible to one and not the other. Hybrid has read
    // under two snapshots since its legs were made concurrent, and the
    // anomaly has the same shape here: every row returned existed during
    // the search, and the page is re-ranked from the union in one place.
    // What it is not is a substitute for the single-statement guarantee
    // `rank_first_semantic_branch_sql` documents — that one is about a
    // scan and its eligibility check disagreeing about liveness, which two
    // independent candidate sets merged by id cannot do.
    if !matches!(req.mode, SearchMode::Semantic)
        && needs_substring_pass(req, &candidates, fetch_target, semantic_weight, tuning)
    {
        for row in run_substring(pool, req, projections, overfetch, tuning).await? {
            merge_row(&mut candidates, row);
        }
    }

    let mut results: Vec<MemorySearchResult> = candidates
        .into_values()
        .map(|candidate| {
            let score = fused_score(req.mode, semantic_weight, &candidate);
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

async fn search_memories_timeseries(
    pool: &PgPool,
    req: &MemorySearchRequest,
) -> Result<MemorySearchPage, StorageError> {
    let owner_ids: Vec<uuid::Uuid> = req
        .read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let pattern = format!("%{}%", req.query.replace('%', "\\%").replace('_', "\\_"));
    let limit = i64::from(req.limit.min(MAX_SEARCH_PAGE_LIMIT).saturating_add(1));
    let kind_filter = match req.kind {
        Some(EntityKind::Fact) => Some("fact"),
        Some(EntityKind::Abstraction) => Some("abstraction"),
        Some(EntityKind::Perspective) => Some("perspective"),
        Some(EntityKind::Goal) | None => None,
    };
    if matches!(req.mode, SearchMode::Semantic)
        && let (Some(embedding), Some(model_id)) =
            (req.query_embedding.as_deref(), req.embedding_model_id.as_deref())
    {
        return search_memories_timeseries_semantic(
            pool,
            &owner_ids,
            kind_filter,
            model_id,
            embedding,
            limit,
            req.limit,
        )
        .await;
    }
    let rows: Vec<(
        uuid::Uuid,
        String,
        String,
        time::OffsetDateTime,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT m.t,
                m.kind::text,
                h.schema_id,
                COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01'),
                LEFT(COALESCE(n.body, u.text, d.body, i.claim, ''), 240)
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
           LEFT JOIN proxima_core.agent_note_v1 n ON n.t = m.t
           LEFT JOIN proxima_core.utterance_v1 u ON u.t = m.t
           LEFT JOIN proxima_core.agent_derivation_v1 d ON d.t = m.t
           LEFT JOIN proxima_core.interpretation_v1 i ON i.t = m.t
          WHERE m.owner_id = ANY($1::uuid[])
            AND ($3::text IS NULL OR m.kind::text = $3)
            AND (
                n.title ILIKE $2 ESCAPE '\\'
                OR n.body ILIKE $2 ESCAPE '\\'
                OR u.text ILIKE $2 ESCAPE '\\'
                OR d.body ILIKE $2 ESCAPE '\\'
                OR i.claim ILIKE $2 ESCAPE '\\'
            )
          ORDER BY m.t DESC
          LIMIT $4",
    )
    .bind(&owner_ids)
    .bind(&pattern)
    .bind(kind_filter)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let page_len = usize::try_from(req.limit.min(MAX_SEARCH_PAGE_LIMIT)).unwrap_or(usize::MAX);
    let has_more = rows.len() > page_len;
    let results = rows
        .into_iter()
        .take(page_len)
        .filter_map(|(t, kind, schema_id, created_at, snippet)| {
            let kind = match kind.as_str() {
                "fact" => EntityKind::Fact,
                "abstraction" => EntityKind::Abstraction,
                "perspective" => EntityKind::Perspective,
                _ => return None,
            };
            Some(MemorySearchResult {
                memory_id: MemoryId::new(t),
                kind,
                schema_id: SchemaId::new(schema_id),
                created_at,
                snippet: snippet.unwrap_or_default(),
                score: 1.0,
                lexical_score: 1.0,
                similarity_score: 0.0,
            })
        })
        .collect();
    Ok(MemorySearchPage { results, has_more })
}

async fn search_memories_timeseries_semantic(
    pool: &PgPool,
    owner_ids: &[uuid::Uuid],
    kind_filter: Option<&str>,
    model_id: &str,
    embedding: &[f32],
    fetch_limit: i64,
    page_limit: u32,
) -> Result<MemorySearchPage, StorageError> {
    let rows: Vec<(uuid::Uuid, String, String, time::OffsetDateTime, f32)> = sqlx::query_as(
        "SELECT m.t,
                m.kind::text,
                h.schema_id,
                COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01'),
                GREATEST(0.0, (1 - (emb.vec <=> $4::vector)))::real
           FROM proxima_core.embeddings emb
           JOIN proxima_core.embedding_heads head
             ON head.entity_id = emb.entity_id
            AND head.model_id = emb.model_id
            AND head.embedding_version = emb.embedding_version
           JOIN proxima_core.memory m ON m.t = emb.entity_id
           JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t
          WHERE m.owner_id = ANY($1::uuid[])
            AND emb.model_id = $2
            AND ($3::text IS NULL OR m.kind::text = $3)
          ORDER BY emb.vec <=> $4::vector
          LIMIT $5",
    )
    .bind(owner_ids)
    .bind(model_id)
    .bind(kind_filter)
    .bind(crate::pgvector::literal(embedding))
    .bind(fetch_limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let page_len = usize::try_from(page_limit.min(MAX_SEARCH_PAGE_LIMIT)).unwrap_or(usize::MAX);
    let has_more = rows.len() > page_len;
    let results = rows
        .into_iter()
        .take(page_len)
        .filter_map(|(t, kind, schema_id, created_at, score)| {
            let kind = match kind.as_str() {
                "fact" => EntityKind::Fact,
                "abstraction" => EntityKind::Abstraction,
                "perspective" => EntityKind::Perspective,
                _ => return None,
            };
            Some(MemorySearchResult {
                memory_id: MemoryId::new(t),
                kind,
                schema_id: SchemaId::new(schema_id),
                created_at,
                snippet: String::new(),
                score,
                lexical_score: 0.0,
                similarity_score: score,
            })
        })
        .collect();
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

/// The score a candidate is ranked and paged by, for its mode.
///
/// Factored out because [`needs_substring_pass`] has to reason about the
/// same number before the page is built, and a second spelling of it would
/// be a second thing to keep in step.
fn fused_score(mode: SearchMode, semantic_weight: f32, candidate: &Candidate) -> f32 {
    match mode {
        SearchMode::Lexical => candidate.lexical_score,
        SearchMode::Semantic => candidate.similarity_score,
        SearchMode::Hybrid => {
            (semantic_weight * candidate.similarity_score)
                + ((1.0 - semantic_weight) * candidate.lexical_score)
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
    let (sidecar_first_param, filters) =
        allocate_candidate_params(req, projections.len(), &mut next_param, tuning);
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
    // The branch set is written after the query CTEs it reads, so it is
    // built into its own buffer and appended below.
    let candidates = common_candidates_sql(
        req,
        &projections,
        sidecar_first_param,
        filters,
        CandidateShape {
            open: "\n          , candidates AS (",
            include_tsv: true,
            ann_restriction: None,
            match_gate: Some(MatchGate::Tsv { rescue }),
        },
    )?;
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

    // `candidates` carries `search_tsv` per branch — read from the stored
    // column where the table has one, computed inside the branch where it
    // does not. Either way the vector is produced exactly once per
    // candidate row, so the gate that admitted the row and the rank arm
    // that scores it share one vector rather than each re-tokenising the
    // document.
    //
    // The branch set no longer needs the `MATERIALIZED` fence, and must
    // not have it: the gate is inside each branch now, beside that
    // branch's owner predicate, which is both what lets an index serve it
    // and what the fence was protecting against losing.
    let mut sql = format!(
        "WITH scrubbed AS (
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
                          websearch_to_tsquery({TEXT_SEARCH_CONFIG}, s.q)
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
                              replace(plainto_tsquery({TEXT_SEARCH_CONFIG}, s.q)::text,
                                      ' & ', ' | '),
                              '')::tsquery
                      ) AS any_tsq
                 FROM scrubbed s
          )"
    );
    // SQL-POLICY: fixed-fragment — the audited candidate builder's own
    // output, written below the query CTEs because its gate reads them.
    sql.push_str(&candidates);

    // The substring band is absent here, and its absence is exact rather
    // than a trade. Every row this statement returns matched a tsquery, so
    // it already scores in the strict band [0.5, 1.0] or the rescue band
    // [0.25, 0.45]; the substring band is a flat 0.25, which `GREATEST`
    // can never prefer over either. Rows that match ONLY the substring
    // predicate are the fallback statement's job — see
    // [`substring_branch_sql`] and [`needs_substring_pass`].
    let strict_score_arm = "CASE WHEN c.search_tsv @@ q.tsq
                          THEN 0.5 + LEAST(COALESCE(ts_rank_cd(c.search_tsv,
                                   websearch_to_tsquery(c.lexical_language,
                                       proxima_core.lexical_query_text(
                                           c.lexical_language, q.scrubbed)),
                                   32), 0.0), 1.0) * 0.5
                          ELSE 0.0 END";
    let score_expr = if rescue {
        format!("GREATEST({strict_score_arm}{rescue_score_arm})")
    } else {
        format!("({strict_score_arm})")
    };

    write!(
        sql,
        "
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 {score_expr}::real AS lexical_score,
                 0.0::real AS similarity_score
          FROM candidates c, q
          WHERE c.search_text <> ''
          ORDER BY {order_by}
          LIMIT {}",
        u64::from(limit),
        order_by = order_by,
        score_expr = score_expr,
    )
    .expect("write to String is infallible");
    Ok((sql, projections))
}

/// The substring band's own statement: the rows `LIKE '%…%'` finds that no
/// tsquery does.
///
/// Split from the lexical branch: `LIKE '%…%'` has no core base-table
/// index (`docs/15` / `pg_trgm` vs projection-built `search_text`), so
/// `tsv @@ q OR text LIKE '%…%'` seq-scans the owner scope and the GIN
/// path is never chosen. Skip via [`needs_substring_pass`] when the
/// tsquery arms already cover the query (all-stopword / partial-word
/// are the cases this arm carries). Second round trip, own snapshot.
fn substring_branch_sql<'p>(
    req: &MemorySearchRequest,
    projections: &'p [MemorySearchProjection],
    limit: u32,
    tuning: &PgTuning,
) -> Result<(String, Vec<&'p MemorySearchProjection>), StorageError> {
    let projections = memory_search_projections(req, projections);
    let mut next_param = 3;
    let (sidecar_first_param, filters) =
        allocate_candidate_params(req, projections.len(), &mut next_param, tuning);
    let query_param = next_param;
    let order_by = branch_order_by(req, "lexical_score");

    let mut sql = common_candidates_sql(
        req,
        &projections,
        sidecar_first_param,
        filters,
        CandidateShape {
            // Owner-first enumeration is the right plan here and the only
            // one available, so the fence the tsvector statement drops is
            // kept: nothing about this predicate is index-servable, and
            // the fence is what stopped the planner driving a sidecar
            // branch across every owner.
            open: MATERIALIZED_CANDIDATES_OPEN,
            include_tsv: false,
            ann_restriction: None,
            match_gate: Some(MatchGate::Substring { query_param }),
        },
    )?;

    write!(
        sql,
        "
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 {SUBSTRING_BAND}::real AS lexical_score,
                 0.0::real AS similarity_score
          FROM candidates c
          WHERE c.search_text <> ''
          ORDER BY {order_by}
          LIMIT {}",
        u64::from(limit),
        order_by = order_by,
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

/// The substring fallback SQL for EXPLAIN-based plan assertions in tests.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn substring_search_sql_for_tests(
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    limit: u32,
    tuning: &PgTuning,
) -> Result<String, StorageError> {
    substring_branch_sql(req, projections, limit, tuning).map(|(sql, _)| sql)
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
    let q = bind_branch_prefix(
        sqlx::query_as::<_, SearchRow>(sqlx::AssertSqlSafe(sql)),
        req,
        &projections,
    )
    .bind(req.query.clone());
    q.fetch_all(pool).await.map_err(map_err)
}

async fn run_substring(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    limit: u32,
    tuning: &PgTuning,
) -> Result<Vec<SearchRow>, StorageError> {
    let (sql, projections) = substring_branch_sql(req, projections, limit, tuning)?;

    // SQL-POLICY: PgIdent
    let q = bind_branch_prefix(
        sqlx::query_as::<_, SearchRow>(sqlx::AssertSqlSafe(sql)),
        req,
        &projections,
    )
    .bind(req.query.clone());
    q.fetch_all(pool).await.map_err(map_err)
}

/// Whether the substring band still has to be read from the corpus.
///
/// A substring-only row scores exactly [`SUBSTRING_BAND`] on the lexical
/// component and nothing on the semantic one, so its final score is
/// `(1 - semantic_weight) * SUBSTRING_BAND` in hybrid and `SUBSTRING_BAND`
/// in lexical mode. If the candidates already in hand hold a full
/// `fetch_target` of rows scoring *strictly* above that, no row this
/// statement could return would reach the page — not even through the
/// `memory_id` tiebreak, which never runs between scores that differ. The
/// scan is then skippable without changing a single returned row.
///
/// Three things make that argument conditional, and all three are checked
/// here rather than assumed:
///
/// - **Order.** It is an argument about score ordering. Under
///   [`SearchOrder::Recency`] the page is ordered by `created_at`, so the
///   newest substring-only row outranks every tsquery hit and the band is
///   not skippable at all.
/// - **Width.** `fetch_target` — the page plus its has-more probe plus
///   whatever a relevance cursor has already emitted — not `req.limit`.
///   Counting against the page alone would drop rows that later pages
///   have to surface.
/// - **The other leg.** In hybrid the count is over fused scores, so the
///   semantic leg's rows count toward it. That is the whole reason hybrid
///   can skip: a corpus with embeddings fills `fetch_target` well above
///   `0.4 * 0.25` from the semantic side alone.
///
/// The escape-hatch semantic statement is frozen and cannot carry the
/// band, so a hybrid search running it has no in-hand substring scores to
/// count and must always read them.
fn needs_substring_pass(
    req: &MemorySearchRequest,
    candidates: &BTreeMap<uuid::Uuid, Candidate>,
    fetch_target: u32,
    semantic_weight: f32,
    tuning: &PgTuning,
) -> bool {
    if matches!(req.order, SearchOrder::Recency) {
        return true;
    }
    if matches!(req.mode, SearchMode::Hybrid)
        && matches!(tuning.semantic_index_first, SemanticIndexFirst::Off)
    {
        return true;
    }
    let band = match req.mode {
        SearchMode::Lexical => SUBSTRING_BAND,
        SearchMode::Hybrid => (1.0 - semantic_weight) * SUBSTRING_BAND,
        // No lexical component at all, so nothing to fall back to.
        SearchMode::Semantic => return false,
    };
    let above = candidates
        .values()
        .filter(|candidate| fused_score(req.mode, semantic_weight, candidate) > band)
        .count();
    above < fetch_target as usize
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
    let sql = match tuning.semantic_index_first {
        SemanticIndexFirst::Off => legacy_semantic_branch_sql(
            req,
            &projections,
            limit,
            candidate_overfetch,
            tuning,
            &mut next_param,
        )?,
        mode => rank_first_semantic_branch_sql(
            req,
            &projections,
            limit,
            candidate_overfetch,
            tuning,
            mode,
            &mut next_param,
        )?,
    };
    Ok((sql, projections))
}

/// The header an unrestricted candidate branch set is written into.
const MATERIALIZED_CANDIDATES_OPEN: &str = "WITH candidates AS MATERIALIZED (";

/// The escape hatch's statement: enumerate the whole read scope, then let
/// the nearest-neighbour window run inside it.
///
/// Frozen. `PROXIMA_PG_SEMANTIC_INDEX_FIRST=off` is a shipped promise that
/// the previous statement is still available byte-for-byte, and the unit
/// goldens hold this text to that. Nothing here specializes.
fn legacy_semantic_branch_sql(
    req: &MemorySearchRequest,
    projections: &[&MemorySearchProjection],
    limit: u32,
    candidate_overfetch: u64,
    tuning: &PgTuning,
    next_param: &mut usize,
) -> Result<String, StorageError> {
    let (sidecar_first_param, filters) =
        allocate_candidate_params(req, projections.len(), next_param, tuning);
    let mut sql = common_candidates_sql(
        req,
        projections,
        sidecar_first_param,
        filters,
        CandidateShape {
            open: MATERIALIZED_CANDIDATES_OPEN,
            include_tsv: false,
            ann_restriction: None,
            match_gate: None,
        },
    )?;
    let vec_param = *next_param;
    let model_param = *next_param + 1;
    let order_by = branch_order_by(req, "similarity_score");

    push_eligible_entities(&mut sql, tuning);
    push_joined_scan_note(&mut sql);
    let chunk_from = joined_ann_from(vec_param, model_param, candidate_overfetch);
    push_vector_candidates(&mut sql, tuning, &chunk_from);

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
    Ok(sql)
}

/// The nearest-neighbour restriction each candidate branch carries.
const ANN_RESTRICTION_SQL: &str =
    "\n             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a)";

/// Rank first, then decide eligibility — over the window, not over the
/// read scope.
///
/// Restrict each branch to the window's ids (semi-join). One statement:
/// two round trips would be two Read Committed snapshots, so a
/// tombstone/supersession between them can change membership.
/// Scan keeps `ORDER BY <distance> LIMIT n` with nothing above it (the
/// only HNSW-servable shape); the head join runs before per-memory
/// collapse so a nearer stale chunk cannot displace the live one.
fn rank_first_semantic_branch_sql(
    req: &MemorySearchRequest,
    projections: &[&MemorySearchProjection],
    limit: u32,
    candidate_overfetch: u64,
    tuning: &PgTuning,
    mode: SemanticIndexFirst,
    next_param: &mut usize,
) -> Result<String, StorageError> {
    // Built first because it allocates the parameter numbers the scan's
    // own binds follow, even though the scan is written above it.
    let (sidecar_first_param, filters) =
        allocate_candidate_params(req, projections.len(), next_param, tuning);
    let candidates = common_candidates_sql(
        req,
        projections,
        sidecar_first_param,
        filters,
        CandidateShape {
            open: "\n          candidates AS (",
            include_tsv: false,
            ann_restriction: Some(ANN_RESTRICTION_SQL),
            match_gate: None,
        },
    )?;
    let vec_param = *next_param;
    let model_param = *next_param + 1;
    let order_by = branch_order_by(req, "a.similarity_score");

    // Hybrid fuses this leg with the lexical one, so the substring band a
    // window row earns has to reach the fusion. Evaluating it here is
    // free: these rows are being read anyway, and the alternative — a
    // third statement probing the merged ids — would be a round trip to
    // learn something the plan already had in hand.
    //
    // It has to be evaluated per branch and OR-ed back together, because
    // `eligible_entities` collapses a memory's branches to one row and
    // keeps only that row's `search_text`. A memory whose base text
    // contains the query but whose sidecar projection does not would
    // otherwise lose the band to the collapse.
    let substring_band = matches!(req.mode, SearchMode::Hybrid).then(|| {
        format!(
            "bool_or(lower(c.search_text) LIKE '%' || {} || '%' ESCAPE '\\')\n                            OVER (PARTITION BY c.kind, c.memory_id)",
            like_literal(*next_param + 2)
        )
    });

    let mut sql = String::from("WITH");
    push_ann_scan(&mut sql, mode, vec_param, model_param, candidate_overfetch);
    push_ann_live(&mut sql);
    // SQL-POLICY: fixed-fragment — the audited candidate builder's own
    // output, written here rather than at the head of the statement
    // because the scan it is restricted to has to be declared first.
    sql.push_str(&candidates);
    sql.push(',');
    push_rank_first_eligible(&mut sql, substring_band.as_deref());

    write!(
        sql,
        "
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 {lexical_score} AS lexical_score,
                 a.similarity_score
          FROM eligible_entities c
          JOIN ann_live a
            ON a.entity_kind = c.kind
           AND a.entity_id = c.memory_id
           AND a.owner_kind = c.owner_kind
           AND a.owner_id IS NOT DISTINCT FROM c.owner_id
          ORDER BY {order_by}
          LIMIT {}",
        u64::from(limit),
        order_by = order_by,
        lexical_score = if substring_band.is_some() {
            format!("(CASE WHEN c.substring_match THEN {SUBSTRING_BAND} ELSE 0.0 END)::real")
        } else {
            "0.0::real".to_string()
        },
    )
    .expect("write to String is infallible");
    Ok(sql)
}

/// The window's live rows, one per memory, scored by its best chunk.
///
/// The head join is version liveness, so it runs before the collapse: a
/// memory whose nearest chunk in the window is stale must still be
/// reachable through whichever live chunk the window also holds, rather
/// than being dropped because the stale one won.
///
/// `max()` rather than a ranked row: every column the page reads other
/// than the score comes from the eligible set, and the score of the best
/// chunk is exactly what the ranked spelling projected.
fn push_ann_live(sql: &mut String) {
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        "
          ann_live AS MATERIALIZED (
              SELECT ann.entity_kind, ann.entity_id, ann.owner_kind, ann.owner_id,
                     max(ann.similarity_score) AS similarity_score
                FROM ann_scan ann
                JOIN proxima_core.embedding_heads head
                  ON head.entity_kind = ann.entity_kind
                 AND head.entity_id = ann.entity_id
                 AND head.model_id = ann.model_id
                 AND head.embedding_version = ann.embedding_version
                 AND head.owner_kind = ann.owner_kind
                 AND head.owner_id IS NOT DISTINCT FROM ann.owner_id
               GROUP BY ann.entity_kind, ann.entity_id, ann.owner_kind, ann.owner_id
          ),",
    );
}

/// One row per eligible memory, with the tie between its branches broken
/// by a written rule.
///
/// A memory reaches this set once per candidate branch that admits it —
/// the base branch and its schema's sidecar are both branches — and those
/// rows differ only in `search_text`. They share a `created_at`, so the
/// old `ORDER BY created_at DESC` never discriminated between them and
/// which one survived was left to the plan
/// (<https://www.postgresql.org/docs/current/sql-select.html>). Measured
/// on a real corpus, changing the plan changed the surviving snippet for
/// roughly half the page, in both directions.
///
/// So the rule is written down instead. A non-NULL `search_text` wins
/// first — a sidecar projection whose fields are all empty yields NULL,
/// and the base branch's text is non-empty by its own filter, so this
/// also stops an empty projection from blanking a snippet the base branch
/// could have filled. Among non-NULL rows the highest branch ordinal
/// wins, which is the schema's own projection rather than the generic
/// memory text.
///
/// The trailing `created_at DESC` is unreachable for every projection the
/// registry ships: each `UNION ALL` arm carries its own `branch_rank`
/// ordinal, and each shipped sidecar joins `memory_id` one-to-one, so a
/// `(kind, memory_id)` group holds at most one row per ordinal and the
/// ordinal alone already breaks the tie. It stays because it is the only
/// thing standing between a future one-to-many projection and the
/// plan-dependent snippet this function exists to eliminate: such a
/// projection would put two rows in one group at the same ordinal, and
/// without a further key `DISTINCT ON` would again be free to keep either.
/// Dead today, load-bearing the day the registry grows a fan-out.
fn push_rank_first_eligible(sql: &mut String, substring_band: Option<&str>) {
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        "
          eligible_entities AS (
              SELECT DISTINCT ON (c.kind, c.memory_id)
                     c.memory_id, c.owner_kind, c.owner_id, c.kind,
                     c.schema_id, c.created_at, c.search_text",
    );
    if let Some(band) = substring_band {
        // A window function is computed before DISTINCT ON chooses which
        // row of the partition survives, so this sees every branch of the
        // memory and the surviving row carries the answer for all of them.
        write!(sql, ",\n                     {band} AS substring_match")
            .expect("write to String is infallible");
    }
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        "
                FROM candidates c
               ORDER BY c.kind, c.memory_id,
                        (c.search_text IS NULL), c.branch_rank DESC,
                        c.created_at DESC
          )",
    );
}

/// Rows an `Overfetch` scan may hold. Behind its barrier the window is spent
/// before eligibility is known, so it is a work budget rather than a result
/// budget and needs a ceiling the request's own window does not give it.
const MAX_ANN_SCAN_ROWS: u64 = 20_000;

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
                         FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
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
///
/// `from` is the chunk-level scan this reads — one row per embedding that
/// survived the nearest-neighbour window — and `row` is the alias carrying
/// that row's memory columns, which differs by arm: the joined scan projects
/// them through its own subquery (`ann`), while an index-first scan leaves
/// them on `eligible_entities` (`c`). Both fall out of the same discriminant
/// the barrier does, so they are read off one match rather than carried
/// alongside `from` where they could disagree with it. The score is `ann` on
/// every arm — it is the similarity the nearest-neighbour scan itself
/// computed — so it is spelled literally in the templates below.
fn push_vector_candidates(sql: &mut String, tuning: &PgTuning, from: &str) {
    let (row, barrier) = match tuning.semantic_index_first {
        SemanticIndexFirst::Off => ("ann", "AS MATERIALIZED ("),
        SemanticIndexFirst::Overfetch => ("c", "AS MATERIALIZED ("),
        SemanticIndexFirst::Pushdown => ("c", "AS ("),
    };

    if tuning.candidate_window_dedup {
        write!(
            sql,
            "
          ann_ranked AS (
              SELECT {row}.memory_id, {row}.kind, {row}.schema_id, {row}.created_at,
                     {row}.search_text, ann.similarity_score,
                     row_number() OVER (PARTITION BY {row}.kind, {row}.memory_id
                                        ORDER BY ann.similarity_score DESC) AS rn
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
                     {row}.search_text, ann.similarity_score
{from}
               ORDER BY {row}.kind, {row}.memory_id, ann.similarity_score DESC
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
    let q = bind_branch_prefix(
        sqlx::query_as::<_, SearchRow>(sqlx::AssertSqlSafe(sql)),
        req,
        &projections,
    )
    .bind(crate::pgvector::literal(query_embedding))
    .bind(model_id.clone());
    // Hybrid's statement carries the substring band, which reads the query
    // text; the other two modes never emit that predicate. See
    // `rank_first_semantic_branch_sql`.
    let q = if matches!(req.mode, SearchMode::Hybrid)
        && !matches!(tuning.semantic_index_first, SemanticIndexFirst::Off)
    {
        q.bind(req.query.clone())
    } else {
        q
    };

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

/// How a candidate branch set is written, and what it is restricted to.
///
/// The lexical branch and the legacy semantic branch enumerate the whole
/// read scope behind a `MATERIALIZED` fence. The rank-first semantic
/// assembly restricts every branch to the nearest-neighbour window it has
/// already computed, so those branches are small by construction and the
/// fence is neither needed nor wanted there.
#[derive(Clone, Copy)]
struct CandidateShape<'a> {
    /// The CTE header the branch set is written into, up to and including
    /// its opening parenthesis.
    open: &'a str,
    /// Adds each candidate's lexical vector as a column. Only the lexical
    /// branch reads it, and an unrestricted `candidates` is MATERIALIZED,
    /// so emitting it unconditionally would make every semantic search
    /// materialise tsvectors nothing reads.
    include_tsv: bool,
    /// The nearest-neighbour restriction every branch carries, when the
    /// caller has already computed that window. Its presence also emits
    /// each branch's ordinal, which is what lets the collapse above break
    /// its tie by a written rule rather than by whatever order the plan
    /// happened to produce.
    ann_restriction: Option<&'a str>,
    /// The text predicate written into each branch's own WHERE, on that
    /// branch's own base table, rather than applied to the branch set from
    /// above. See [`MatchGate`].
    match_gate: Option<MatchGate>,
}

/// A branch's text predicate, written where an index can serve it.
///
/// Applied above the branch set, a text predicate is unservable: it
/// matches a CTE result, and no index exists on a CTE result. Written into
/// the branch, beside the branch's own owner predicate, the same predicate
/// becomes an ordinary base-table qualifier and the planner may pick
/// `idx_memories_search_tsv` (migration 0019) for it.
///
/// The owner predicate has to stay in that same WHERE. The `MATERIALIZED`
/// fence documented on [`common_candidates_sql`] exists because an
/// inlinable branch set once let the planner push the text filter into a
/// sidecar branch and scan it across every owner; a pushed-down gate
/// without the owner scope beside it reintroduces exactly that.
#[derive(Clone, Copy)]
enum MatchGate {
    /// The tsvector arms. `rescue` adds the OR-rescue tsquery, which only
    /// [`SearchMode::Lexical`] fires — see the arm's own comment in
    /// [`lexical_branch_sql`].
    ///
    /// Both tsqueries are read as `(SELECT … FROM q)`, so each folds to a
    /// one-time `InitPlan` constant. A per-row-parsed tsquery in this
    /// position has no index path at all (see [`TEXT_SEARCH_CONFIG`]).
    Tsv { rescue: bool },
    /// The substring band's own predicate, for the fallback statement that
    /// runs only when the tsvector arms cannot already fill the page.
    Substring { query_param: usize },
}

impl CandidateShape<'_> {
    /// The branch ordinal column, written only for a restricted branch
    /// set. Unrestricted branches keep their historical column list, which
    /// the goldens pin byte-for-byte.
    fn ordinal(self, ordinal: usize) -> String {
        if self.ann_restriction.is_some() {
            format!(", {ordinal}::int AS branch_rank")
        } else {
            String::new()
        }
    }
}

/// Claims the `$n` every candidate branch shares, in the one order
/// [`bind_branch_prefix`] can bind them: each projection's
/// `(schema_id, schema_version)` pair, then the optional filters —
/// schema, since, until, tags, recency cursor.
///
/// Returns the first sidecar parameter alongside the filter set, because
/// each branch addresses its own projection pair relative to it.
fn allocate_candidate_params(
    req: &MemorySearchRequest,
    projections: usize,
    next_param: &mut usize,
    tuning: &PgTuning,
) -> (usize, CandidateFilterParams) {
    let sidecar_first_param = *next_param;
    *next_param += projections * 2;
    let mut claim = |width: usize| {
        let param = *next_param;
        *next_param += width;
        param
    };
    let filters = CandidateFilterParams {
        schema_filter: req.schema_id.as_ref().map(|_| claim(1)),
        since: req.since.map(|_| claim(1)),
        until: req.until.map(|_| claim(1)),
        tags: (!req.tags.is_empty()).then(|| claim(1)),
        // A recency cursor binds a `(created_at, memory_id)` pair.
        recency_cursor: matches!(req.after, Some(SearchCursor::Recency { .. })).then(|| claim(2)),
        supersedes_anti_join: tuning.candidate_window_dedup,
    };
    (sidecar_first_param, filters)
}

/// Builds the owner-scoped candidate CTE shared by every branch.
///
/// Takes the parameter numbers rather than claiming them, because a
/// statement whose gate reads a bound value — [`MatchGate::Substring`] —
/// has to know that value's `$n` before the branch set it sits inside can
/// be written. [`allocate_candidate_params`] stays the one place that
/// assigns them.
fn common_candidates_sql(
    req: &MemorySearchRequest,
    projections: &[&MemorySearchProjection],
    sidecar_first_param: usize,
    filters: CandidateFilterParams,
    shape: CandidateShape<'_>,
) -> Result<String, StorageError> {
    let include_tsv = shape.include_tsv;

    // On an unrestricted branch set MATERIALIZED pins the plan to
    // owner-first enumeration: candidates are resolved via the
    // owner-prefix indexes before any text or vector predicate runs. Left
    // inlinable, the planner pushed the lexical tsvector filter into the
    // Abstraction sidecar branch and seq-scanned the whole table across
    // every owner (measured: 3.1s of a 3.3s query on the 150k corpus).
    //
    // That fence is what a restricted branch set does not want: its rows
    // are already bounded by the nearest-neighbour window, and the fence
    // would stop the planner from driving each branch off that window's
    // ids. The caller supplies the header for the shape it is building.
    let mut sql = String::from(shape.open);
    push_candidate_branch_prefix(&mut sql);
    write!(
        sql,
        "NULL::text[] AS tags, COALESCE(m.text, '') AS search_text{base_tsv}{ordinal} \
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
        ordinal = shape.ordinal(0),
    )
    .expect("write to String is infallible");
    push_supersedes_anti_join(&mut sql, req, filters, base_branch_kinds(req));
    push_base_memory_filters(&mut sql, req, filters);
    sql.push_str(" AND NULLIF(m.text, '') IS NOT NULL");
    push_match_gate(&mut sql, shape, "m.search_tsv", "COALESCE(m.text, '')");
    push_ann_restriction(&mut sql, shape);

    for (idx, projection) in projections.iter().enumerate() {
        let table = PgIdent::table(&projection.sidecar_table)?;
        let projection_expr = projection_search_expr(&projection.fields)?;
        let tag_expr = projection_tag_expr(projection)?;
        let schema_param = sidecar_first_param + (idx * 2);
        let version_param = schema_param + 1;
        let search_text_expr = format!("NULLIF(concat_ws(' ', {projection_expr}), '')");
        let branch_tsv = projection_tsv_expr(projection, &search_text_expr)?;
        let tsv_expr = if include_tsv {
            format!(
                ", {branch_tsv} AS search_tsv, {} AS lexical_language",
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
             {search_text_expr} AS search_text{tsv_expr}{ordinal}
             FROM proxima_core.memories m
JOIN {table} s ON s.memory_id = m.memory_id",
            tag_expr = tag_expr.as_str(),
            table = table.as_str(),
            ordinal = shape.ordinal(idx + 1),
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
        push_match_gate(&mut sql, shape, &branch_tsv, &search_text_expr);
        push_ann_restriction(&mut sql, shape);
    }

    sql.push(')');
    Ok(sql)
}

/// The nearest-neighbour restriction, written onto every branch of a
/// restricted branch set.
///
/// The join this feeds is an inner join, so a candidate row whose memory
/// is not in the window can never reach the result. Restricting each
/// branch by that window's ids is therefore a semi-join reduction: it
/// removes rows the join would have removed anyway, and the branch's own
/// predicates are untouched. Written as `IN (SELECT ...)` rather than
/// against a bound array so the planner keeps the choice of join strategy
/// — the window can be as wide as `candidate_overfetch` at full
/// pagination depth.
fn push_ann_restriction(sql: &mut String, shape: CandidateShape<'_>) {
    if let Some(restriction) = shape.ann_restriction {
        // SQL-POLICY: fixed-fragment
        sql.push_str(restriction);
    }
}

/// Writes one branch's [`MatchGate`] into that branch's own WHERE.
///
/// `tsv_expr` and `search_text_expr` are the branch's own spellings of the
/// two columns the gate can read: the base branch reads `memories`
/// directly, a sidecar branch reads its stored `search_tsv` where it
/// declares one and an inline `lexical_tsv(…)` where it does not. Only the
/// stored spellings can be served by an index — the inline one is the
/// expression migration 0019 explains it will not chase.
fn push_match_gate(sql: &mut String, shape: CandidateShape<'_>, tsv_expr: &str, text_expr: &str) {
    match shape.match_gate {
        None => {}
        Some(MatchGate::Tsv { rescue }) => {
            write!(sql, "\n             AND ({tsv_expr} @@ (SELECT tsq FROM q)")
                .expect("write to String is infallible");
            if rescue {
                write!(sql, " OR {tsv_expr} @@ (SELECT any_tsq FROM q)")
                    .expect("write to String is infallible");
            }
            sql.push(')');
        }
        Some(MatchGate::Substring { query_param }) => {
            write!(
                sql,
                "\n             AND lower({text_expr}) LIKE '%' || {} || '%' ESCAPE '\\'",
                like_literal(query_param)
            )
            .expect("write to String is infallible");
        }
    }
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

/// Candidate home owner is `m`'s own columns (`FROM proxima_core.memories m`).
/// `publish_to_world` UPDATEs those columns, so they are live. Do not look
/// the owner up through `entity_owner_union`: the memories arm *is* `m`,
/// and a `goal_id`/`memory_id` collision would duplicate the row.
fn push_candidate_branch_prefix(sql: &mut String) {
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        "SELECT m.memory_id, m.owner_kind, m.owner_id, \
         m.kind AS kind, \
         m.schema_id, m.created_at, ",
    );
}

/// Owner-scope gate, split at SQL-build time so every arm is index-eligible.
/// `owner_id IS NOT DISTINCT FROM s.id` defeats the `(owner_kind, owner_id)`
/// b-tree prefixes. `owner_binds` emits a NULL id only for
/// [`OwnerRef::World`], so the read set is an equality join plus — only
/// when World is in the set — a constant World arm (`owner_id IS NULL`).
/// Reads `m`'s own owner columns (see [`push_candidate_branch_prefix`]).
fn push_read_owner_scope(sql: &mut String, req: &MemorySearchRequest) {
    // SQL-POLICY: fixed-fragment — the read set arrives as the two bound
    // arrays $1/$2; nothing here is interpolated.
    sql.push_str(
        " WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
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
        Some(EntityKind::Fact) => sql.push_str(" AND m.kind = 'Fact'"),
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
        PayloadKind::Fact => sql.push_str(" AND m.kind = 'Fact'"),
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
/// `PayloadKind`): facts are `m.kind = 'Fact'`, derived rows
/// (`Abstraction` / `Perspective`) are `m.kind <> 'Fact'`. Under that
/// predicate the mixed head filter reduces to exactly one of its
/// disjuncts, which is what lets the dedup arm specialize the head SQL
/// per branch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BranchKinds {
    /// No kind predicate on the branch (or a `AND false` arm that holds no
    /// rows either way): both disjuncts of the head filter are reachable.
    Mixed,
    /// `AND m.kind = 'Fact'`: only the fact-entity head test can hold.
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
/// A fact-only branch pins `m.kind = 'Fact'`, under which the head filter's
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

/// The fact-head liveness test, which both head-filter spellings carry
/// verbatim: a row is live when it is not a fact projection at all, or when
/// its fact entity still names it as the current memory.
///
/// Held once because the kind-specialized fact-only arm and the mixed arm's
/// first disjunct are the same predicate — the specialization drops the kind
/// dispatch around it, not the test itself.
const FACT_HEAD_TEST: &str = "m.fact_entity_id IS NULL \
     OR EXISTS ( \
         SELECT 1 FROM proxima_core.fact_entities fe \
          WHERE fe.fact_entity_id = m.fact_entity_id \
            AND fe.current_memory_id = m.memory_id \
     )";

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
                write!(sql, " AND ( {FACT_HEAD_TEST} )").expect("write to String is infallible");
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
    write!(
        sql,
        " AND ( (m.kind = 'Fact' AND ( {FACT_HEAD_TEST} )) OR (m.kind <> 'Fact' AND "
    )
    .expect("write to String is infallible");
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
/// projection's `(schema_id, schema_version)` pair, then the optional
/// filters in the order [`common_candidates_sql`] allocates their `$n`
/// — schema filter, since, until, tags, recency cursor.
///
/// One function rather than two called back to back, because back to back in
/// exactly this order is the only sequence the builders' parameter assignment
/// admits; splitting it offered no caller a choice, only a way to get the
/// order wrong. Each branch binds its own trailing parameters after this
/// returns: the query text for lexical, the vector and model id for semantic.
fn bind_branch_prefix<'q>(
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

    /// The lexical branch in hybrid mode: the OR-rescue arm is absent, so
    /// the gate is the strict tsquery alone and the score has no
    /// `GREATEST` to take. This is the statement the product default
    /// runs, and the one migration 0019's index exists for.
    const LEXICAL_BRANCH_HYBRID_GOLDEN: &str = r"WITH scrubbed AS (
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
          , candidates AS (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, m.search_tsv AS search_tsv, m.lexical_language AS lexical_language FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL
             AND (m.search_tsv @@ (SELECT tsq FROM q)) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, s.search_tsv AS search_tsv, s.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )
             AND (s.search_tsv @@ (SELECT tsq FROM q)) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) AS search_tsv, m.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND m2.memory_id IS NULL
             AND (proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) @@ (SELECT tsq FROM q)))
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 (CASE WHEN c.search_tsv @@ q.tsq
                          THEN 0.5 + LEAST(COALESCE(ts_rank_cd(c.search_tsv,
                                   websearch_to_tsquery(c.lexical_language,
                                       proxima_core.lexical_query_text(
                                           c.lexical_language, q.scrubbed)),
                                   32), 0.0), 1.0) * 0.5
                          ELSE 0.0 END)::real AS lexical_score,
                 0.0::real AS similarity_score
          FROM candidates c, q
          WHERE c.search_text <> ''
          ORDER BY lexical_score DESC, c.memory_id DESC
          LIMIT 44";

    /// The substring fallback. Its gate is per branch and unservable by
    /// any core index, which is exactly why it is a separate statement
    /// rather than a third arm of the one above.
    const SUBSTRING_BRANCH_DEFAULT_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL
             AND lower(COALESCE(m.text, '')) LIKE '%' || replace(replace(replace(lower($10), '\\', '\\\\'), '%', '\\%'), '_', '\\_') || '%' ESCAPE '\' UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )
             AND lower(NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '')) LIKE '%' || replace(replace(replace(lower($10), '\\', '\\\\'), '%', '\\%'), '_', '\\_') || '%' ESCAPE '\' UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND m2.memory_id IS NULL
             AND lower(NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) LIKE '%' || replace(replace(replace(lower($10), '\\', '\\\\'), '%', '\\%'), '_', '\\_') || '%' ESCAPE '\')
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.25::real AS lexical_score,
                 0.0::real AS similarity_score
          FROM candidates c
          WHERE c.search_text <> ''
          ORDER BY lexical_score DESC, c.memory_id DESC
          LIMIT 44";

    /// The semantic branch in hybrid mode carries the substring band, so
    /// a window row that the lexical statement's tsquery gate excluded
    /// still reaches fusion with the 0.25 it earns. Semantic mode's
    /// golden above pins that the same builder emits a constant zero
    /// there — the band belongs to fusion, not to similarity.
    const SEMANTIC_BRANCH_HYBRID_GOLDEN: &str = r"WITH
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
                         FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
                        WHERE emb.owner_kind = s.kind AND emb.owner_id = s.id
                     )
               ORDER BY emb.vec <=> $10::vector
               LIMIT 704
          ),
          ann_live AS MATERIALIZED (
              SELECT ann.entity_kind, ann.entity_id, ann.owner_kind, ann.owner_id,
                     max(ann.similarity_score) AS similarity_score
                FROM ann_scan ann
                JOIN proxima_core.embedding_heads head
                  ON head.entity_kind = ann.entity_kind
                 AND head.entity_id = ann.entity_id
                 AND head.model_id = ann.model_id
                 AND head.embedding_version = ann.embedding_version
                 AND head.owner_kind = ann.owner_kind
                 AND head.owner_id IS NOT DISTINCT FROM ann.owner_id
               GROUP BY ann.entity_kind, ann.entity_id, ann.owner_kind, ann.owner_id
          ),
          candidates AS (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, 0::int AS branch_rank FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, 1::int AS branch_rank
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, 2::int AS branch_rank
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND m2.memory_id IS NULL
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a)),
          eligible_entities AS (
              SELECT DISTINCT ON (c.kind, c.memory_id)
                     c.memory_id, c.owner_kind, c.owner_id, c.kind,
                     c.schema_id, c.created_at, c.search_text,
                     bool_or(lower(c.search_text) LIKE '%' || replace(replace(replace(lower($12), '\\', '\\\\'), '%', '\\%'), '_', '\\_') || '%' ESCAPE '\')
                            OVER (PARTITION BY c.kind, c.memory_id) AS substring_match
                FROM candidates c
               ORDER BY c.kind, c.memory_id,
                        (c.search_text IS NULL), c.branch_rank DESC,
                        c.created_at DESC
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 (CASE WHEN c.substring_match THEN 0.25 ELSE 0.0 END)::real AS lexical_score,
                 a.similarity_score
          FROM eligible_entities c
          JOIN ann_live a
            ON a.entity_kind = c.kind
           AND a.entity_id = c.memory_id
           AND a.owner_kind = c.owner_kind
           AND a.owner_id IS NOT DISTINCT FROM c.owner_id
          ORDER BY a.similarity_score DESC, c.memory_id DESC
          LIMIT 44";

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

    #[test]
    fn hybrid_lexical_and_substring_statements_are_byte_identical() {
        let (lexical, _) = lexical_branch_sql(
            &golden_request(SearchMode::Hybrid),
            &golden_projections(),
            GOLDEN_LIMIT,
            &PgTuning::default(),
        )
        .unwrap();
        let (substring, _) = substring_branch_sql(
            &golden_request(SearchMode::Lexical),
            &golden_projections(),
            GOLDEN_LIMIT,
            &PgTuning::default(),
        )
        .unwrap();
        let (semantic, _) = semantic_branch_sql(
            &golden_request(SearchMode::Hybrid),
            &golden_projections(),
            GOLDEN_LIMIT,
            GOLDEN_CANDIDATE_OVERFETCH,
            &PgTuning::default(),
        )
        .unwrap();

        assert_eq!(lexical, LEXICAL_BRANCH_HYBRID_GOLDEN);
        assert_eq!(substring, SUBSTRING_BRANCH_DEFAULT_GOLDEN);
        assert_eq!(semantic, SEMANTIC_BRANCH_HYBRID_GOLDEN);
    }

    /// The gate has to be inside each branch's own WHERE, not above the
    /// branch set. Above it, no index applies — that is the whole finding
    /// migrations 0009 and 0011 recorded and migration 0019 reverses.
    #[test]
    fn every_lexical_branch_gates_on_its_own_table() {
        let (sql, _) = lexical_branch_sql(
            &golden_request(SearchMode::Hybrid),
            &golden_projections(),
            GOLDEN_LIMIT,
            &PgTuning::default(),
        )
        .unwrap();

        // One gate per branch: base `memories`, the note sidecar's stored
        // column, and the interpretation sidecar's inline tokenisation.
        assert!(sql.contains("AND (m.search_tsv @@ (SELECT tsq FROM q))"));
        assert!(sql.contains("AND (s.search_tsv @@ (SELECT tsq FROM q))"));
        assert!(sql.contains(
            "AND (proxima_core.lexical_tsv(m.lexical_language, \
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) \
             @@ (SELECT tsq FROM q))"
        ));
        // And nothing left matching above the branch set.
        let tail = sql.split("FROM candidates c, q").nth(1).expect("tail");
        assert!(
            !tail.contains("@@"),
            "a match predicate survived above the branch set"
        );
        // The fence the tsvector statement must not carry.
        assert!(!sql.contains("candidates AS MATERIALIZED"));
    }

    /// The substring band is absent from the tsvector statement, and the
    /// rescue arm only from hybrid's.
    #[test]
    fn the_tsvector_statement_carries_no_substring_band() {
        for mode in [SearchMode::Lexical, SearchMode::Hybrid] {
            let (sql, _) = lexical_branch_sql(
                &golden_request(mode),
                &golden_projections(),
                GOLDEN_LIMIT,
                &PgTuning::default(),
            )
            .unwrap();
            assert!(
                !sql.contains("LIKE"),
                "{mode:?} still carries the substring band"
            );
            assert_eq!(
                sql.contains("any_tsq @@") || sql.contains("@@ q.any_tsq"),
                matches!(mode, SearchMode::Lexical),
                "{mode:?} rescue arm"
            );
        }
    }

    /// Every clause of the skip rule, because each is the difference
    /// between a page that is right and a page that quietly lost a row.
    #[test]
    fn the_substring_pass_is_skipped_only_when_the_page_is_already_decided() {
        fn holding(scores: &[(f32, f32)]) -> BTreeMap<uuid::Uuid, Candidate> {
            scores
                .iter()
                .enumerate()
                .map(|(idx, &(lexical_score, similarity_score))| {
                    let memory_id = uuid::Uuid::from_u128(idx as u128 + 1);
                    (
                        memory_id,
                        Candidate {
                            memory_id,
                            kind: EntityKind::Fact,
                            schema_id: SchemaId::new("test/x".into()),
                            created_at: time::OffsetDateTime::from_unix_timestamp(0).unwrap(),
                            snippet: String::new(),
                            lexical_score,
                            similarity_score,
                        },
                    )
                })
                .collect()
        }
        let default = PgTuning::default();

        // Lexical: three rows in the strict band decide a page of three.
        let mut req = golden_request(SearchMode::Lexical);
        req.after = None;
        let strict = holding(&[(0.9, 0.0), (0.8, 0.0), (0.7, 0.0)]);
        assert!(!needs_substring_pass(&req, &strict, 3, 0.6, &default));
        // The same rows do not decide a page of four.
        assert!(needs_substring_pass(&req, &strict, 4, 0.6, &default));

        // A rescue row that scored no cover sits exactly ON the band, and a
        // substring-only row would tie it — so it does not count.
        let on_band = holding(&[(0.9, 0.0), (0.8, 0.0), (SUBSTRING_BAND, 0.0)]);
        assert!(needs_substring_pass(&req, &on_band, 3, 0.6, &default));

        // Recency orders by created_at, so no score argument applies at all.
        let mut recency = golden_request(SearchMode::Lexical);
        recency.order = SearchOrder::Recency;
        recency.after = None;
        assert!(needs_substring_pass(&recency, &strict, 1, 0.6, &default));

        // Hybrid counts fused scores against the fused band, so the
        // semantic leg alone can decide the page: 0.6 * 0.2 = 0.12 clears
        // 0.4 * 0.25 = 0.10.
        let mut hybrid = golden_request(SearchMode::Hybrid);
        hybrid.after = None;
        let semantic_only = holding(&[(0.0, 0.2), (0.0, 0.2), (0.0, 0.2)]);
        assert!(!needs_substring_pass(
            &hybrid,
            &semantic_only,
            3,
            0.6,
            &default
        ));
        // Just under it does not.
        let weak = holding(&[(0.0, 0.16), (0.0, 0.16), (0.0, 0.16)]);
        assert!(needs_substring_pass(&hybrid, &weak, 3, 0.6, &default));

        // The escape-hatch semantic statement cannot carry the substring
        // band, so a hybrid search running it has nothing to count.
        assert!(needs_substring_pass(
            &hybrid,
            &semantic_only,
            3,
            0.6,
            &legacy_tuning()
        ));

        // Semantic mode has no lexical component to fall back to.
        let semantic = golden_request(SearchMode::Semantic);
        assert!(!needs_substring_pass(
            &semantic,
            &holding(&[]),
            50,
            0.6,
            &default
        ));
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
            let req = golden_request(SearchMode::Lexical);
            let (sidecar_first_param, filters) =
                allocate_candidate_params(&req, selected.len(), &mut next_param, &tuning);
            let sql = common_candidates_sql(
                &req,
                &selected,
                sidecar_first_param,
                filters,
                CandidateShape {
                    open: MATERIALIZED_CANDIDATES_OPEN,
                    include_tsv: true,
                    ann_restriction: None,
                    match_gate: None,
                },
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
            let tuning = PgTuning {
                candidate_window_dedup,
                ..PgTuning::default()
            };
            let (sidecar_first_param, filters) =
                allocate_candidate_params(&req, selected.len(), &mut next_param, &tuning);
            sql.push(
                common_candidates_sql(
                    &req,
                    &selected,
                    sidecar_first_param,
                    filters,
                    CandidateShape {
                        open: MATERIALIZED_CANDIDATES_OPEN,
                        include_tsv: true,
                        ann_restriction: None,
                        match_gate: None,
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
    const SEMANTIC_BRANCH_LEGACY_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text FROM proxima_core.memories m  WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) )) , eligible_entities AS MATERIALIZED (
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
    const LEXICAL_BRANCH_DEFAULT_GOLDEN: &str = r"WITH scrubbed AS (
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
          , candidates AS (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, m.search_tsv AS search_tsv, m.lexical_language AS lexical_language FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL
             AND (m.search_tsv @@ (SELECT tsq FROM q) OR m.search_tsv @@ (SELECT any_tsq FROM q)) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, s.search_tsv AS search_tsv, s.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )
             AND (s.search_tsv @@ (SELECT tsq FROM q) OR s.search_tsv @@ (SELECT any_tsq FROM q)) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) AS search_tsv, m.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND m2.memory_id IS NULL
             AND (proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) @@ (SELECT tsq FROM q) OR proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) @@ (SELECT any_tsq FROM q)))
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 GREATEST(CASE WHEN c.search_tsv @@ q.tsq
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
                ELSE 0.0 END)::real AS lexical_score,
                 0.0::real AS similarity_score
          FROM candidates c, q
          WHERE c.search_text <> ''
          ORDER BY lexical_score DESC, c.memory_id DESC
          LIMIT 44";

    /// The legacy lexical branch — the text the escape hatch restores.
    /// Never regenerate.
    const LEXICAL_BRANCH_LEGACY_GOLDEN: &str = r"WITH scrubbed AS (
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
          , candidates AS (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, m.search_tsv AS search_tsv, m.lexical_language AS lexical_language FROM proxima_core.memories m  WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) AND NULLIF(m.text, '') IS NOT NULL
             AND (m.search_tsv @@ (SELECT tsq FROM q) OR m.search_tsv @@ (SELECT any_tsq FROM q)) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, s.search_tsv AS search_tsv, s.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) )
             AND (s.search_tsv @@ (SELECT tsq FROM q) OR s.search_tsv @@ (SELECT any_tsq FROM q)) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) AS search_tsv, m.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) )
             AND (proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) @@ (SELECT tsq FROM q) OR proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) @@ (SELECT any_tsq FROM q)))
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 GREATEST(CASE WHEN c.search_tsv @@ q.tsq
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
                ELSE 0.0 END)::real AS lexical_score,
                 0.0::real AS similarity_score
          FROM candidates c, q
          WHERE c.search_text <> ''
          ORDER BY lexical_score DESC, c.memory_id DESC
          LIMIT 44";

    /// The legacy candidate CTE — per-row `NOT EXISTS` successor probe.
    /// Never regenerate.
    const COMMON_CANDIDATES_LEGACY_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, m.search_tsv AS search_tsv, m.lexical_language AS lexical_language FROM proxima_core.memories m  WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, s.search_tsv AS search_tsv, s.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) AS search_tsv, m.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ))";

    const SEMANTIC_INDEX_FIRST_OVERFETCH_GOLDEN: &str = r"WITH
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
          ann_live AS MATERIALIZED (
              SELECT ann.entity_kind, ann.entity_id, ann.owner_kind, ann.owner_id,
                     max(ann.similarity_score) AS similarity_score
                FROM ann_scan ann
                JOIN proxima_core.embedding_heads head
                  ON head.entity_kind = ann.entity_kind
                 AND head.entity_id = ann.entity_id
                 AND head.model_id = ann.model_id
                 AND head.embedding_version = ann.embedding_version
                 AND head.owner_kind = ann.owner_kind
                 AND head.owner_id IS NOT DISTINCT FROM ann.owner_id
               GROUP BY ann.entity_kind, ann.entity_id, ann.owner_kind, ann.owner_id
          ),
          candidates AS (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, 0::int AS branch_rank FROM proxima_core.memories m  WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) AND NULLIF(m.text, '') IS NOT NULL
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, 1::int AS branch_rank
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) )
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, 2::int AS branch_rank
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) )
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a)),
          eligible_entities AS (
              SELECT DISTINCT ON (c.kind, c.memory_id)
                     c.memory_id, c.owner_kind, c.owner_id, c.kind,
                     c.schema_id, c.created_at, c.search_text
                FROM candidates c
               ORDER BY c.kind, c.memory_id,
                        (c.search_text IS NULL), c.branch_rank DESC,
                        c.created_at DESC
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 a.similarity_score
          FROM eligible_entities c
          JOIN ann_live a
            ON a.entity_kind = c.kind
           AND a.entity_id = c.memory_id
           AND a.owner_kind = c.owner_kind
           AND a.owner_id IS NOT DISTINCT FROM c.owner_id
          ORDER BY a.similarity_score DESC, c.memory_id DESC
          LIMIT 44";

    const SEMANTIC_INDEX_FIRST_PUSHDOWN_GOLDEN: &str = r"WITH
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
                         FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
                        WHERE emb.owner_kind = s.kind AND emb.owner_id = s.id
                     )
               ORDER BY emb.vec <=> $10::vector
               LIMIT 704
          ),
          ann_live AS MATERIALIZED (
              SELECT ann.entity_kind, ann.entity_id, ann.owner_kind, ann.owner_id,
                     max(ann.similarity_score) AS similarity_score
                FROM ann_scan ann
                JOIN proxima_core.embedding_heads head
                  ON head.entity_kind = ann.entity_kind
                 AND head.entity_id = ann.entity_id
                 AND head.model_id = ann.model_id
                 AND head.embedding_version = ann.embedding_version
                 AND head.owner_kind = ann.owner_kind
                 AND head.owner_id IS NOT DISTINCT FROM ann.owner_id
               GROUP BY ann.entity_kind, ann.entity_id, ann.owner_kind, ann.owner_id
          ),
          candidates AS (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, 0::int AS branch_rank FROM proxima_core.memories m  WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) ) AND NULLIF(m.text, '') IS NOT NULL
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, 1::int AS branch_rank
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) )
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, 2::int AS branch_rank
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND NOT EXISTS ( SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id )) )
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a)),
          eligible_entities AS (
              SELECT DISTINCT ON (c.kind, c.memory_id)
                     c.memory_id, c.owner_kind, c.owner_id, c.kind,
                     c.schema_id, c.created_at, c.search_text
                FROM candidates c
               ORDER BY c.kind, c.memory_id,
                        (c.search_text IS NULL), c.branch_rank DESC,
                        c.created_at DESC
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 a.similarity_score
          FROM eligible_entities c
          JOIN ann_live a
            ON a.entity_kind = c.kind
           AND a.entity_id = c.memory_id
           AND a.owner_kind = c.owner_kind
           AND a.owner_id IS NOT DISTINCT FROM c.owner_id
          ORDER BY a.similarity_score DESC, c.memory_id DESC
          LIMIT 44";

    const SEMANTIC_WINDOW_DEDUP_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
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

    const SEMANTIC_INDEX_FIRST_OVERFETCH_WINDOW_DEDUP_GOLDEN: &str = r"WITH
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
          ann_live AS MATERIALIZED (
              SELECT ann.entity_kind, ann.entity_id, ann.owner_kind, ann.owner_id,
                     max(ann.similarity_score) AS similarity_score
                FROM ann_scan ann
                JOIN proxima_core.embedding_heads head
                  ON head.entity_kind = ann.entity_kind
                 AND head.entity_id = ann.entity_id
                 AND head.model_id = ann.model_id
                 AND head.embedding_version = ann.embedding_version
                 AND head.owner_kind = ann.owner_kind
                 AND head.owner_id IS NOT DISTINCT FROM ann.owner_id
               GROUP BY ann.entity_kind, ann.entity_id, ann.owner_kind, ann.owner_id
          ),
          candidates AS (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, 0::int AS branch_rank FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, 1::int AS branch_rank
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, 2::int AS branch_rank
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND m2.memory_id IS NULL
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a)),
          eligible_entities AS (
              SELECT DISTINCT ON (c.kind, c.memory_id)
                     c.memory_id, c.owner_kind, c.owner_id, c.kind,
                     c.schema_id, c.created_at, c.search_text
                FROM candidates c
               ORDER BY c.kind, c.memory_id,
                        (c.search_text IS NULL), c.branch_rank DESC,
                        c.created_at DESC
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 a.similarity_score
          FROM eligible_entities c
          JOIN ann_live a
            ON a.entity_kind = c.kind
           AND a.entity_id = c.memory_id
           AND a.owner_kind = c.owner_kind
           AND a.owner_id IS NOT DISTINCT FROM c.owner_id
          ORDER BY a.similarity_score DESC, c.memory_id DESC
          LIMIT 44";

    /// The DEFAULT semantic branch: index-first pushdown + window dedup.
    const SEMANTIC_BRANCH_DEFAULT_GOLDEN: &str = r"WITH
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
                         FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
                        WHERE emb.owner_kind = s.kind AND emb.owner_id = s.id
                     )
               ORDER BY emb.vec <=> $10::vector
               LIMIT 704
          ),
          ann_live AS MATERIALIZED (
              SELECT ann.entity_kind, ann.entity_id, ann.owner_kind, ann.owner_id,
                     max(ann.similarity_score) AS similarity_score
                FROM ann_scan ann
                JOIN proxima_core.embedding_heads head
                  ON head.entity_kind = ann.entity_kind
                 AND head.entity_id = ann.entity_id
                 AND head.model_id = ann.model_id
                 AND head.embedding_version = ann.embedding_version
                 AND head.owner_kind = ann.owner_kind
                 AND head.owner_id IS NOT DISTINCT FROM ann.owner_id
               GROUP BY ann.entity_kind, ann.entity_id, ann.owner_kind, ann.owner_id
          ),
          candidates AS (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, 0::int AS branch_rank FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, 1::int AS branch_rank
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, 2::int AS branch_rank
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND m2.memory_id IS NULL
             AND m.memory_id IN (SELECT a.entity_id FROM ann_live a)),
          eligible_entities AS (
              SELECT DISTINCT ON (c.kind, c.memory_id)
                     c.memory_id, c.owner_kind, c.owner_id, c.kind,
                     c.schema_id, c.created_at, c.search_text
                FROM candidates c
               ORDER BY c.kind, c.memory_id,
                        (c.search_text IS NULL), c.branch_rank DESC,
                        c.created_at DESC
          )
          SELECT c.memory_id, c.kind, c.schema_id, c.created_at,
                 left(c.search_text, 480) AS snippet,
                 0.0::real AS lexical_score,
                 a.similarity_score
          FROM eligible_entities c
          JOIN ann_live a
            ON a.entity_kind = c.kind
           AND a.entity_id = c.memory_id
           AND a.owner_kind = c.owner_kind
           AND a.owner_id IS NOT DISTINCT FROM c.owner_id
          ORDER BY a.similarity_score DESC, c.memory_id DESC
          LIMIT 44";

    /// The DEFAULT candidate CTE: unique-join successor anti-join.
    const COMMON_CANDIDATES_DEFAULT_GOLDEN: &str = r"WITH candidates AS MATERIALIZED (SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags, COALESCE(m.text, '') AS search_text, m.search_tsv AS search_tsv, m.lexical_language AS lexical_language FROM proxima_core.memories m  LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND ( (m.kind = 'Fact' AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) )) OR (m.kind <> 'Fact' AND m2.memory_id IS NULL) ) AND NULLIF(m.text, '') IS NOT NULL UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, s.tags AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.title::text, ''), NULLIF(array_to_string(s.tags, ' '), '')), '') AS search_text, s.search_tsv AS search_tsv, s.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.agent_note_v1 s ON s.memory_id = m.memory_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $3
           AND m.schema_version = $4 AND m.kind = 'Fact' AND m.created_at >= $7 AND m.created_at <= $8 AND s.tags @> $9::text[] AND ( m.fact_entity_id IS NULL OR EXISTS ( SELECT 1 FROM proxima_core.fact_entities fe WHERE fe.fact_entity_id = m.fact_entity_id AND fe.current_memory_id = m.memory_id ) ) UNION ALL SELECT m.memory_id, m.owner_kind, m.owner_id, m.kind AS kind, m.schema_id, m.created_at, NULL::text[] AS tags,
             NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '') AS search_text, proxima_core.lexical_tsv(m.lexical_language, NULLIF(concat_ws(' ', NULLIF(s.claim::text, '')), '')) AS search_tsv, m.lexical_language AS lexical_language
             FROM proxima_core.memories m
JOIN proxima_core.interpretation_v1 s ON s.memory_id = m.memory_id LEFT JOIN proxima_core.memories m2 ON m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT FROM m.owner_id WHERE (EXISTS (
              SELECT 1
                FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
               WHERE m.owner_kind = s.kind AND m.owner_id = s.id
           ) OR (m.owner_kind = 'world' AND m.owner_id IS NULL)) AND m.tombstoned_at IS NULL
           AND m.schema_id = $5
           AND m.schema_version = $6 AND m.kind = 'Abstraction' AND m.created_at >= $7 AND m.created_at <= $8 AND NULL::text[] @> $9::text[] AND m2.memory_id IS NULL)";
}
