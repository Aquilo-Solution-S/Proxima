//! `core_search_memories` — projection-first content search.
//!
//! **ONE ranked statement per participating flavor.** Not one per schema:
//! the projection row is keyed `(memory_id, schema_id)` and carries the
//! vector, the tags and the language, so the whole of a flavor's lexical
//! surface is one composite-index scan with `schema_id` as a row predicate.
//!
//! 1. **ranked** — one statement over `<flavor>.projection`. The owner is
//!    an Index Cond on the composite `gin(owner_id, search_tsv)`,
//!    `schema_id = ANY(..)` narrows to the participating schemas, the score
//!    windows come from the flavor's DECLARATION, and the candidate budget
//!    is the flavor's declared `overfetch_k`.
//! 2. **substring** — at most ONE more statement, over exactly the schemas
//!    the ranked arm returned nothing for, and only where the schema
//!    declares an arm. `SubstringArm::Off` contributes no statement and no
//!    rows. There is no blanket "GIN missed, re-query with `LIKE`" retry;
//!    the arm is opt-in per schema.
//! 3. **admit** — owner + optional current-head on the hit `t`s;
//!    `schema_id` is on `memory`. The projection's `owner_id` is an
//!    index accelerator; admit still filters `memory.owner_id`, so
//!    authorization never rests on the copy. Both candidate arms ALSO
//!    apply every filter this step applies — supersession and kind — because
//!    a window spent on rows this step will drop is a window that returns
//!    fewer results than the corpus contains, at a wrong band or not at
//!    all. See [`admit_side_restriction`].
//! 4. **snippets** — one primary-key lookup per distinct sidecar present in
//!    the PAGE, after admission and paging have run. The text lives in
//!    exactly one place and is fetched for at most `limit` rows
//!    instead of for the whole candidate window.
//! 5. **pins** — engine neighbor load, only if the caller asked
//!
//! Unscoped search (no tags) scans only flavor #0's schemas. A tag filter
//! is the documented flavor scope (`docs/09-developing-flavors.md`): those
//! queries also scan flavor schemas that declare a `tag_column`, and only
//! flavors that declare `BandComparability::CoreBands` and
//! `RankSource::Projection` — a score this merge cannot compare and a shape
//! this renderer cannot serve are both exclusions the contract states.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use futures_util::future::try_join_all;
use proxima_core::flavor::{
    BAND_NAME_EXACT, BAND_NAME_RESCUE, BAND_NAME_SUBSTRING, Band, BandComparability,
    LanguagePolicy, SubstringArm,
};
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
use crate::projection::sidecar_text_sql;
use crate::tuning::PgTuning;

use super::lineage::load_one_schema_snippets;

/// How far past the caller's `limit` one statement fetches before the merge
/// trims. The CAP is declared — [`ProjectionSpec::overfetch_k`], a
/// shard-level property and therefore the flavor's — but the request
/// scaling has no contract home, because it is a function of the request
/// rather than of the shard.
const REQUEST_OVERFETCH_FACTOR: u32 = 20;

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
}

/// The ranked arm's row. `schema_id` rides along because the substring arm's
/// trigger is per-SCHEMA: one statement per flavor has no per-projection
/// signal, and reading the trigger as "the flavor's ranked arm returned
/// nothing" would LOSE rows — a query matching a note by lexeme and an
/// utterance only by substring must still return the utterance. One extra
/// column in the select list buys that.
#[derive(Debug, sqlx::FromRow)]
struct RankedRow {
    t: uuid::Uuid,
    schema_id: String,
    lexical_score: f32,
}

#[derive(Debug, sqlx::FromRow)]
struct SubstringRow {
    t: uuid::Uuid,
    lexical_score: f32,
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
    let flavors = core_search_flavors(req, projections);

    let mut hits: BTreeMap<uuid::Uuid, Hit> = BTreeMap::new();
    match req.mode {
        SearchMode::Lexical => {
            merge_hits(
                &mut hits,
                scan_flavors(pool, req, &flavors, limit, true).await?,
            );
        }
        SearchMode::Semantic => {
            merge_hits(
                &mut hits,
                scan_embeddings(pool, req, tuning, semantic_overfetch(limit)).await?,
            );
        }
        SearchMode::Hybrid => {
            if req.query_embedding.is_some() && req.embedding_model_id.is_some() {
                let (lexical, semantic) = tokio::try_join!(
                    scan_flavors(pool, req, &flavors, limit, false),
                    scan_embeddings(pool, req, tuning, semantic_overfetch(limit)),
                )?;
                merge_hits(&mut hits, lexical);
                merge_hits(&mut hits, semantic);
            } else {
                merge_hits(
                    &mut hits,
                    scan_flavors(pool, req, &flavors, limit, true).await?,
                );
            }
        }
    }

    let admitted = admit_hits(pool, req, &hits).await?;
    let mut page = page_hits(req, limit, admitted);
    // The snippet is fetched LAST, for the rows that made the page.
    //
    // It cannot ride the ranked statement: `{sidecar}` is a per-SCHEMA
    // value, so one statement per flavor would need one `LEFT JOIN` per
    // sidecar TABLE (four, for flavor #0's five projected schemas) and a
    // `COALESCE` over as many snippet expressions — per-schema SQL in a
    // statement whose whole point is that it has none. Hydrating after
    // `page_hits` fetches at most `limit` rows instead of the whole
    // candidate window.
    hydrate_snippets(pool, projections, &mut page.results).await?;
    Ok(page)
}

/// The candidate budget for one statement: the request-scaling rule,
/// clamped to what the flavor DECLARES.
///
/// One statement per flavor hands `merge_hits` at most `overfetch_k`.
///
/// For `Lexical` and `Relevance` the returned page cannot move — but the
/// candidate-superset argument alone does not establish that. "The global
/// top-`overfetch` by `lexical_score` contains the top-`limit` by
/// `lexical_score`" is a claim about candidates, and admission is not
/// candidate-preserving: it drops superseded revisions. A window spent on
/// rows admission will drop returns fewer than `limit` results while
/// admissible rows sat below the cut. [`admit_side_restriction`] closes
/// that: with it the window is the global top-`overfetch` of the ADMISSIBLE
/// set, and the superset argument holds over the set the page is drawn from.
///
/// The page CAN still move for `Hybrid` — a row with weak lexical rank but
/// strong similarity — and for very deep cursor pages. Both are accepted
/// movement; the `Lexical` + `Relevance` page is not.
fn overfetch(limit: u32, overfetch_k: u32) -> u32 {
    limit
        .saturating_mul(REQUEST_OVERFETCH_FACTOR)
        .max(limit)
        .min(overfetch_k)
}

/// The embedding scan is not per-flavor — `proxima_core.embeddings` is one
/// table for every owner — so its cap is spelled where its one reader lives.
fn semantic_overfetch(limit: u32) -> u32 {
    const SEMANTIC_OVERFETCH_CAP: u32 = 1_000;
    overfetch(limit, SEMANTIC_OVERFETCH_CAP)
}

/// One flavor's participating schemas, grouped for the ONE statement that
/// serves them.
///
/// Every property that statement can spell only once — the lexical
/// configuration, the score windows, the weight array — is read off
/// [`Self::head`], and freeze is what makes that legal: a flavor declaring
/// `RankSource::Projection` whose schemas disagree about any of them is a
/// registry error, not a query-build-time `StorageError` on a hot path.
struct FlavorScan<'a> {
    schemas: Vec<&'a MemorySearchProjection>,
}

impl<'a> FlavorScan<'a> {
    /// The schema whose declaration the flavor-wide statement renders.
    fn head(&self) -> &'a MemorySearchProjection {
        self.schemas[0]
    }
}

fn core_search_flavors<'a>(
    req: &MemorySearchRequest,
    projections: &'a [MemorySearchProjection],
) -> Vec<FlavorScan<'a>> {
    let mut by_schema = BTreeMap::<&str, &MemorySearchProjection>::new();
    for projection in projections {
        // "Core" is the ordinal, asked of the contract. A schema-name
        // prefix (`starts_with("proxima_core.")`) would be a schema name
        // standing in for a flavor identity, satisfiable by any flavor that
        // picked the same schema.
        let is_core = proxima_core::FLAVOR_0.declares_sidecar_table(&projection.sidecar_table);
        // Unscoped search stays on flavor #0's sidecars. A tag filter is
        // how a flavor scopes `core_search_memories` (docs/09); those
        // queries must reach the flavor sidecar that declared `tag_column`.
        if !is_core && req.tags.is_empty() {
            continue;
        }
        // A foreign flavor's score enters this merge only if its flavor
        // says the score is comparable, and only if its read shape is one
        // this renderer can serve.
        if !is_core && !matches!(projection.band_comparability, BandComparability::CoreBands) {
            continue;
        }
        if !projection.rank_source.is_projection() {
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
        // so a table shared by two schemas must be scanned for both.
        by_schema
            .entry(projection.schema_id.as_str())
            .or_insert(projection);
    }
    // `projection_table` IS the flavor: `ProjectionSpec::table`, one per
    // flavor by construction, in the flavor's own schema.
    let mut by_flavor = BTreeMap::<&str, Vec<&MemorySearchProjection>>::new();
    for projection in by_schema.into_values() {
        by_flavor
            .entry(projection.projection_table.as_str())
            .or_default()
            .push(projection);
    }
    by_flavor
        .into_values()
        .map(|schemas| FlavorScan { schemas })
        .collect()
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
            })
            .or_insert(hit);
    }
}

/// One job per participating FLAVOR, not one per schema.
///
/// A flavor's whole lexical surface is one statement against its own shard;
/// a tag-scoped request that reaches a second flavor runs one more.
async fn scan_flavors(
    pool: &PgPool,
    req: &MemorySearchRequest,
    flavors: &[FlavorScan<'_>],
    limit: u32,
    rescue: bool,
) -> Result<Vec<Hit>, StorageError> {
    if flavors.is_empty() {
        return Ok(Vec::new());
    }
    let jobs = flavors
        .iter()
        .map(|flavor| scan_one_flavor(pool, req, flavor, limit, rescue));
    let batches = try_join_all(jobs).await?;
    Ok(batches.into_iter().flatten().collect())
}

async fn scan_one_flavor(
    pool: &PgPool,
    req: &MemorySearchRequest,
    flavor: &FlavorScan<'_>,
    limit: u32,
    rescue: bool,
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
    let schema_ids: Vec<&str> = flavor
        .schemas
        .iter()
        .map(|projection| projection.schema_id.as_str())
        .collect();
    let overfetch = i64::from(overfetch(limit, flavor.head().overfetch_k));
    let tags = (!req.tags.is_empty()).then_some(req.tags.as_slice());
    let sql = ranked_projection_sql(flavor, req, rescue)?;

    // SQL-POLICY: PgIdent
    let rows: Vec<RankedRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(&req.query)
        .bind(overfetch)
        .bind(tags)
        .bind(req.since)
        .bind(req.until)
        .bind(recency_t)
        .bind(&owner_ids)
        .bind(&schema_ids)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;

    let present: BTreeSet<&str> = rows.iter().map(|row| row.schema_id.as_str()).collect();
    let mut hits: Vec<Hit> = rows
        .iter()
        .map(|row| Hit {
            t: row.t,
            lexical_score: row.lexical_score.max(0.0),
            similarity_score: 0.0,
        })
        .collect();

    // Exact per-schema parity: the substring arm runs over the schemas the
    // ranked arm returned NOTHING for, and only where the schema declares an
    // arm. One extra statement at most.
    //
    // The window is flavor-global, and that direction is NOT harmless: a
    // schema whose ranked hits all fell outside the flavor-global
    // top-`overfetch` is reported missing and served by the substring arm
    // instead — which stamps the FLAT substring floor, so a row the ranked
    // arm would have scored 0.545455 comes back at 0.250000, and a
    // lexeme-only match (`cartographies` stems onto a row that does not
    // contain the substring) comes back not at all. `admit_side_restriction`
    // is what keeps the window from being spent on rows admission is going
    // to drop, which is the only way that starvation is reachable at default
    // limits.
    let missing: Vec<&MemorySearchProjection> = flavor
        .schemas
        .iter()
        .copied()
        .filter(|projection| !present.contains(projection.schema_id.as_str()))
        .filter(|projection| projection.substring != SubstringArm::Off)
        .collect();
    if !missing.is_empty() {
        let missing_ids: Vec<&str> = missing
            .iter()
            .map(|projection| projection.schema_id.as_str())
            .collect();
        let sql = substring_sql(&missing, req)?;
        // SQL-POLICY: PgIdent
        let rows: Vec<SubstringRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(like_pattern(&req.query))
            .bind(overfetch)
            .bind(tags)
            .bind(req.since)
            .bind(req.until)
            .bind(recency_t)
            .bind(&owner_ids)
            .bind(&missing_ids)
            .fetch_all(pool)
            .await
            .map_err(map_err)?;
        hits.extend(rows.into_iter().map(|row| Hit {
            t: row.t,
            lexical_score: row.lexical_score.max(0.0),
            similarity_score: 0.0,
        }));
    }
    Ok(hits)
}

/// The band this schema declares under `name`.
///
/// A `&[Band]` is an unordered set with a `name` on each member, so the
/// renderer looks its arms up by string. Freeze holds a
/// `RankSource::Projection` flavor to declaring all three names, which is
/// what keeps this `Err` unreachable in a frozen registry rather than a
/// runtime hazard on a hot path.
fn band(projection: &MemorySearchProjection, name: &str) -> Result<Band, StorageError> {
    projection
        .bands
        .iter()
        .copied()
        .find(|band| band.name == name)
        .ok_or_else(|| {
            StorageError::Internal(format!(
                "schema {} declares no band named {name:?}; the core renderer resolves its \
                 arms by name and freeze should have refused this contract",
                projection.schema_id
            ))
        })
}

/// The substring arm: at most ONE statement, over exactly the schemas the
/// ranked arm returned nothing for.
///
/// The shape is `MemoryFirstNestedLoop`, unchanged and deliberately NOT
/// routed at the composite index (probe-measured regression, recorded on
/// the declaration): drive `proxima_core.memory` on the owner index, probe
/// the sidecar by `t`, filter `LIKE` on an already-fetched row.
///
/// One leg per distinct sidecar, `UNION ALL`ed under one `ORDER BY` and one
/// `LIMIT`, because `lower(<search text>)` is a per-schema expression and
/// no single table alias can stand for four sidecar tables. The legs share the
/// bound schema-id array: a leg over `agent_note_v1` can only join memories
/// that HAVE an `agent_note_v1` row, so the array narrows rather than
/// selects, and one bind serves every leg.
///
/// The text expression is the DECLARED search fields
/// (`sidecar_text_sql`) — the same expression the stored vector and the
/// snippet are built from, so the three cannot disagree about what a row's
/// searchable text is.
fn substring_sql(
    schemas: &[&MemorySearchProjection],
    req: &MemorySearchRequest,
) -> Result<String, StorageError> {
    // One leg per sidecar TABLE. Two schemas sharing a sidecar (core's
    // agent-derivation registers as Abstraction and Perspective) would
    // otherwise scan it twice for one answer.
    let mut by_table = BTreeMap::<&str, &MemorySearchProjection>::new();
    for projection in schemas {
        by_table
            .entry(projection.sidecar_table.as_str())
            .or_insert(projection);
    }
    let legs = by_table
        .into_values()
        .map(|projection| substring_leg_sql(projection, req))
        .collect::<Result<Vec<_>, StorageError>>()?;
    let [only] = legs.as_slice() else {
        let order_by = match req.order {
            SearchOrder::Relevance => "s.lexical_score DESC, s.t DESC",
            SearchOrder::Recency => "s.t DESC",
        };
        let union = legs.join("\n          UNION ALL\n");
        // SQL-POLICY: PgIdent
        return Ok(format!(
            "SELECT s.t, s.lexical_score
               FROM (
          {union}
                    ) s
              ORDER BY {order_by}
              LIMIT $2"
        ));
    };
    let order_by = match req.order {
        SearchOrder::Relevance => "lexical_score DESC, c.t DESC",
        SearchOrder::Recency => "c.t DESC",
    };
    // SQL-POLICY: PgIdent
    Ok(format!(
        "{only}
          ORDER BY {order_by}
          LIMIT $2"
    ))
}

/// One sidecar's leg of the substring arm.
///
/// There is no query-side CTE here: the `LIKE` predicate reads no tsquery,
/// so computing a `websearch_to_tsquery` and a `plainto_tsquery` per
/// statement would be paid for and never read.
fn substring_leg_sql(
    projection: &MemorySearchProjection,
    req: &MemorySearchRequest,
) -> Result<String, StorageError> {
    let table = PgIdent::table(&projection.sidecar_table)?;
    let search_text = sidecar_text_sql(projection)?;
    let tag_pred = match projection.tag_column.as_deref() {
        Some(column) if !req.tags.is_empty() => {
            let column = PgIdent::column(column)?;
            format!(
                " AND c.{} {op} $3::text[]",
                column.as_str(),
                op = tag_operator(req.tag_match)
            )
        }
        _ => String::new(),
    };
    // A flat band: the substring arm ranks nothing, it only admits — hence
    // zero width, and no `ts_rank` call to normalize.
    let (floor, _) = band(projection, BAND_NAME_SUBSTRING)?.parts();
    // Same reason as the ranked arm's: this window is shared across the
    // missing schemas now, so a row admission will drop must not spend a
    // slot in it. See `admit_side_restriction`.
    let admit_pred = admit_side_restriction(req, SUBSTRING_MEMORY_ID);
    // SQL-POLICY: PgIdent
    Ok(format!(
        "SELECT c.t,
                {floor}::real AS lexical_score
           FROM {table} c
           JOIN proxima_core.memory m ON m.t = c.t
          WHERE m.owner_id = ANY($7::uuid[])
            AND m.schema_id = ANY($8::text[])
            AND (lower({search_text}) LIKE $1 ESCAPE '\\')
            {tag_pred}{admit_pred}
            AND ($4::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(c.t), TIMESTAMPTZ '1970-01-01') >= $4)
            AND ($5::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(c.t), TIMESTAMPTZ '1970-01-01') <= $5)
            AND ($6::uuid IS NULL OR c.t < $6)",
        table = table.as_str(),
    ))
}

/// The ranked arm: `<flavor>.projection` ALONE, one statement for the whole
/// flavor.
///
/// "Alone" is literal and load-bearing, in two ways.
///
/// The owner predicate reads `p.owner_id`, not `memory.owner_id` through a
/// join, which is the only spelling the composite
/// `gin(owner_id, search_tsv)` can serve — a join puts the owner on the
/// other side of the index and leaves the GIN with the tsvector half of a
/// two-column index. The two columns cannot disagree: the projection row is
/// FK'd to `memory (t) ON DELETE CASCADE`, so it never outlives its
/// admission, and owner transfer rewrites both in one transaction
/// (`access::owner_columns`, driven by the projection surface's declared
/// `TransferLeg::Rehomed` like every other followed table).
///
/// And there is no sidecar join at all. `{sidecar}` is a per-SCHEMA value;
/// one statement over a flavor's five projected schemas cannot name it, so
/// the snippet is fetched after paging instead. The arm therefore binds no
/// `LIKE` pattern, and `search_projection_identity` pins the emitted
/// placeholder numbering.
///
/// `schema_id` is a row predicate — `= ANY($8)` — rather than a
/// projection-selection criterion, which is what lets one statement serve
/// the whole participating set.
///
/// The admit-side restriction ([`admit_side_restriction`]) is the third
/// predicate that has to be here rather than only at admit. See that
/// function.
fn ranked_projection_sql(
    flavor: &FlavorScan<'_>,
    req: &MemorySearchRequest,
    rescue: bool,
) -> Result<String, StorageError> {
    let head = flavor.head();
    let table = PgIdent::table(&head.projection_table)?;
    let tsv = "p.search_tsv";
    let tag_pred = if req.tags.is_empty() {
        String::new()
    } else {
        // Every participating schema declares a `tag_column` when the
        // request carries tags — `core_search_flavors` drops the ones that
        // do not — and the predicate reads `p.tag`, a column every
        // projection row has (default `'{}'`). So it is uniform across the
        // flavor and only the SCHEMA SET narrows.
        format!(
            " AND p.tag {op} $3::text[]",
            op = tag_operator(req.tag_match)
        )
    };
    let multilingual = multilingual(head);
    let rank_tsq = rank_tsquery_expr(multilingual);
    let weights = rank_weight_array(head);
    // The score windows and the `ts_rank` normalization flags are READ OFF
    // the declaration, resolved by name. Raw `ts_rank` is not comparable
    // across corpora; a band is, which is what makes a cross-flavor merge
    // meaningful — and a band whose normalization is undeclared is a window
    // two renderers can fill differently while claiming the same number.
    let exact = band(head, BAND_NAME_EXACT)?;
    let rescue_band = band(head, BAND_NAME_RESCUE)?;
    let (rescue_floor, rescue_width) = rescue_band.parts();
    let rescue_norm = rescue_band.normalization_arg();
    let rescue_score = if rescue {
        format!(
            ", CASE WHEN q.any_tsq IS NOT NULL AND {tsv} @@ q.any_tsq
                    THEN {rescue_floor} + LEAST(COALESCE(ts_rank({weights}{tsv}, q.any_tsq{rescue_norm}), 0.0) * 100.0, 1.0) * {rescue_width}
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
    let (exact_floor, exact_width) = exact.parts();
    let exact_norm = exact.normalization_arg();
    let score_expr = format!(
        "GREATEST(
                    CASE WHEN {tsv} @@ q.tsq
                         THEN {exact_floor} + LEAST(COALESCE(ts_rank_cd({weights}{tsv}, {rank_tsq}{exact_norm}), 0.0), 1.0) * {exact_width}
                         ELSE 0.0 END{rescue_score},
                    0.0
                )::real"
    );
    let order_by = match req.order {
        SearchOrder::Relevance => "lexical_score DESC, p.memory_id DESC",
        SearchOrder::Recency => "p.memory_id DESC",
    };
    let admit_pred = admit_side_restriction(req, RANKED_MEMORY_ID);
    // SQL-POLICY: PgIdent
    Ok(format!(
        "{q_cte}
         SELECT p.memory_id AS t,
                p.schema_id,
                {score_expr} AS lexical_score
           FROM {table} p, q
          WHERE p.owner_id = ANY($7::uuid[])
            AND p.schema_id = ANY($8::text[])
            AND ({tsv} @@ q.tsq{rescue_where})
            {tag_pred}{admit_pred}
            AND ($4::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(p.memory_id), TIMESTAMPTZ '1970-01-01') >= $4)
            AND ($5::timestamptz IS NULL
                 OR COALESCE(uuid_extract_timestamp(p.memory_id), TIMESTAMPTZ '1970-01-01') <= $5)
            AND ($6::uuid IS NULL OR p.memory_id < $6)
          ORDER BY {order_by}
          LIMIT $2",
        q_cte = query_side_cte(multilingual),
        table = table.as_str(),
    ))
}

/// The candidate-side spelling of "this row's memory" in the ranked arm.
const RANKED_MEMORY_ID: &str = "p.memory_id";
/// …and in a substring leg, which already drives `proxima_core.memory m`.
const SUBSTRING_MEMORY_ID: &str = "m.t";

/// The `memory_kind` literal a request's `kind` filter selects.
///
/// ONE author, used by the candidate statements AND by [`admit_hits`], so
/// the two cannot drift: a filter the admit side applies and the candidate
/// side does not know about is a starved window. `EntityKind::Goal` selects
/// nothing — `proxima_core.memory_kind` has exactly three values and goals
/// are not among them — which is also why the admit side's `parse_kind` can
/// never drop an admitted row for being unrecognised.
///
/// A closed enum in, a fixed fragment out: no caller text reaches the SQL,
/// which is why the candidate statements can RENDER this rather than bind it
/// and still keep their eight-parameter contract.
fn kind_literal(kind: Option<EntityKind>) -> Option<&'static str> {
    match kind {
        Some(EntityKind::Fact) => Some("fact"),
        Some(EntityKind::Abstraction) => Some("abstraction"),
        Some(EntityKind::Perspective) => Some("perspective"),
        Some(EntityKind::Goal) | None => None,
    }
}

/// Every filter admission applies, applied to the CANDIDATE statements too.
///
/// Admission is not candidate-preserving. `search_admit_sql` drops rows the
/// candidate window already paid for, so a window spent on rows this step
/// will drop is a window that admits fewer than `limit` results while
/// admissible rows sat just below the cut. With ONE global top-`overfetch`
/// per flavor it is reachable at `limit = 1` (`overfetch` =
/// 20): twenty-five doomed rows of one schema starve another schema's live
/// row out of the window entirely, the starved schema is then reported
/// "missing", and the substring arm re-finds it at its FLAT floor — a real
/// result at a wrong, lower score, or no result at all when the match is
/// lexeme-only.
///
/// TWO filters land here:
///
/// 1. **Supersession.** `HeadsOnly` is the tool default and what
///    `core_recall` sends. Measured on a 25-revision backlog: the starved
///    row scores `0.250000` (substring floor) without this predicate and
///    `0.545455` (its exact band) with it.
/// 2. **Kind.** `kind` does NOT narrow the participating schema set the way
///    `schema_id` does, because ONE schema id may register under two kinds —
///    flavor #0's `core/agent-derivation-v1` is both an `Abstraction` and a
///    `Perspective`. The projection carries no kind column, so under
///    `kind = Perspective` the abstraction-kind rows of that same schema id
///    enter the window and admit drops them after. Measured on the same
///    tree, same row: `0.250000` at `limit = 1` and `0.545455` at
///    `limit = 8`. A score that depends on the page size is not a score.
///
/// Restricting the candidates is what makes "the page cannot move for
/// `Lexical` + `Relevance`" true rather than nearly true: with
/// these predicates the window is the global top-`overfetch` of the
/// ADMISSIBLE set, and the top-`limit` of an admissible set is contained in
/// its own top-`overfetch` whenever `overfetch >= limit`, which
/// [`overfetch`] guarantees. Owner, `since` and `until` are already row
/// predicates on the candidate side, and `schema_id` narrows the
/// participating schema set in `core_search_flavors` before a statement is
/// rendered. That the list is complete is checked mechanically by
/// `every_admit_filter_is_mirrored_on_the_candidate_side`, not asserted
/// here.
///
/// # Cost
///
/// One index probe into `proxima_core.memory` (`UNIQUE (t)`) per MATCHING
/// row — not per RETURNED candidate. `ORDER BY lexical_score DESC` has no
/// index to walk, so the sort is fed by every row the `@@` matches and
/// `overfetch_k` bounds only what comes back out; the EXISTS is evaluated
/// below that sort. Measured at 50 000 matching rows: **+51%**. `HeadsOnly`
/// adds a second probe into `memory_head` (keyed by `handle`).
///
/// The planner is free to answer with a hash join instead, and at 25 000
/// heads it was FASTER doing so (51.5 ms vs 85.8 ms), so the plan pin asserts
/// the nested loop's index names when it gets a nested loop and does not
/// forbid the alternative. `include_superseded` requests with no `kind`
/// filter render nothing here and keep the unfiltered window.
fn admit_side_restriction(req: &MemorySearchRequest, memory_id: &str) -> String {
    let heads_only = matches!(req.supersession, SupersessionStatus::HeadsOnly);
    let kind = kind_literal(req.kind);
    if !heads_only && kind.is_none() {
        return String::new();
    }
    if memory_id == SUBSTRING_MEMORY_ID {
        // The leg already drives `proxima_core.memory m`, so the kind is a
        // column predicate on a row it has already fetched and only the head
        // lookup needs a probe.
        let kind_pred = kind.map_or_else(String::new, |kind| {
            format!("\n            AND m.kind = '{kind}'")
        });
        let head_pred = if heads_only {
            "
            AND EXISTS (SELECT 1
                          FROM proxima_core.memory_head hh
                         WHERE hh.handle = m.handle AND hh.t = m.t)"
        } else {
            ""
        };
        return format!("{kind_pred}{head_pred}");
    }
    let head_join = if heads_only {
        "
                          JOIN proxima_core.memory_head hh
                            ON hh.handle = hm.handle AND hh.t = hm.t"
    } else {
        ""
    };
    let kind_pred = kind.map_or_else(String::new, |kind| {
        format!("\n                           AND hm.kind = '{kind}'")
    });
    format!(
        "
            AND EXISTS (SELECT 1
                          FROM proxima_core.memory hm{head_join}
                         WHERE hm.t = {memory_id}{kind_pred})"
    )
}

/// The snippet, for the rows that made the page.
///
/// The shape `load_lineage_snippets` already runs: group the ids by
/// `schema_id`, look the projection up, and fan out one primary-key lookup
/// per sidecar. Same `snippet_sql` expression, same `c.t = ANY($1)` shape,
/// same function — reused rather than restated.
///
/// *Identity note.* The join this replaces was an INNER JOIN, so a
/// projection row whose sidecar row was missing dropped out of the result
/// entirely; here it comes back with an empty snippet. The
/// `projection_memory_id_fkey` cascade makes that state unreachable, but
/// the difference is real.
async fn hydrate_snippets(
    pool: &PgPool,
    projections: &[MemorySearchProjection],
    results: &mut [MemorySearchResult],
) -> Result<(), StorageError> {
    if results.is_empty() {
        return Ok(());
    }
    let mut by_schema = BTreeMap::<String, Vec<uuid::Uuid>>::new();
    for result in results.iter() {
        by_schema
            .entry(result.schema_id.as_str().to_owned())
            .or_default()
            .push(result.memory_id.into_inner());
    }
    let jobs = by_schema
        .into_iter()
        .filter_map(|(schema_id, ts)| {
            projections
                .iter()
                .find(|projection| projection.schema_id.as_str() == schema_id)
                .map(|projection| (projection, ts))
        })
        .map(|(projection, ts)| load_one_schema_snippets(pool, projection, ts));
    let batches = try_join_all(jobs).await?;
    let snippets: HashMap<uuid::Uuid, String> = batches.into_iter().flatten().collect();
    for result in results {
        if let Some(snippet) = snippets.get(&result.memory_id.into_inner()) {
            result.snippet.clone_from(snippet);
        }
    }
    Ok(())
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
    // The SAME author the candidate statements render from. Two copies of
    // this match is how the kind filter came to be applied on one side and
    // not the other.
    let kind_filter = kind_literal(req.kind);
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
                // Hydrated after paging, for the rows that survive it.
                snippet: String::new(),
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

/// The ranked arm, for the plan pins.
///
/// It takes the flavor's participating schemas rather than one projection,
/// because that is what one statement serves.
///
/// # Errors
///
/// Propagates the builder's, including a schema whose declaration is
/// missing a band the renderer resolves by name.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn ranked_projection_sql_for_tests(
    schemas: &[&MemorySearchProjection],
    req: &MemorySearchRequest,
    rescue: bool,
) -> Result<String, StorageError> {
    ranked_projection_sql(
        &FlavorScan {
            schemas: schemas.to_vec(),
        },
        req,
        rescue,
    )
}

/// The substring arm, for the plan pins.
///
/// # Errors
///
/// Propagates the builder's.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn substring_sql_for_tests(
    schemas: &[&MemorySearchProjection],
    req: &MemorySearchRequest,
) -> Result<String, StorageError> {
    substring_sql(schemas, req)
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
    use super::{
        EntityKind, MemorySearchRequest, OwnerRef, SearchMode, SearchOrder, SupersessionStatus,
        TagMatch,
    };

    #[test]
    fn admit_reads_schema_from_memory() {
        let src = include_str!("search.rs");
        let join = format!(
            "{}{}",
            "JOIN proxima_core.memory_head h ON h.handle = ", "m.handle\""
        );
        assert!(
            !src.contains(&join),
            "non-HeadsOnly admit does not join head for schema_id"
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
            prod.contains("ranked_projection_sql(flavor, req, rescue)"),
            "the ranked scan must run the exported builder"
        );
        assert!(
            prod.contains("substring_sql(&missing, req)"),
            "the substring scan must run the exported builder"
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
    /// `core_search_flavors` selects it with
    /// `FLAVOR_0.declares_sidecar_table(..)`. The literal lives HERE so
    /// production carries no second copy: if the contract stops declaring
    /// one of these, unscoped search silently narrows and this test says
    /// which one went.
    #[test]
    fn unscoped_search_scans_exactly_flavor_0s_projected_sidecars() {
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

    /// Where each arm's owner predicate sits, and that the placeholders did
    /// not shift when the LIKE bind left the ranked arm.
    ///
    /// The ranked arm does not join `memory` at all: its owner is
    /// `p.owner_id`, on the projection, which is the only spelling the
    /// composite `gin(owner_id, search_tsv)` can serve. The substring arm's
    /// sits on `memory`, at candidate time and not only at admit — a
    /// sidecar carries no `owner_id`, so the loop drives from the owner
    /// index and probes the sidecar by `t`.
    ///
    /// The numbering is the mechanical hazard: a mis-shifted `$8` turns the
    /// owner predicate into the tag array. This asserts the EMITTED SQL, so
    /// a shift fails here as well as in the identity fixture.
    #[test]
    fn every_bind_is_where_the_scan_binds_it() {
        let note = proxima_core::FlavorRegistry::new()
            .freeze_or_panic_for_tests()
            .search_projections()
            .iter()
            .find(|projection| projection.schema_id.as_str() == "core/agent-note-v1")
            .expect("core/agent-note-v1 is a search surface")
            .clone();
        let mut req = request_with_tags();
        let flavor = super::FlavorScan {
            schemas: vec![&note],
        };

        let ranked = super::ranked_projection_sql(&flavor, &req, true).expect("ranked");
        assert!(ranked.contains("lexical_scrub($1)"), "$1 is the query");
        assert!(ranked.contains("LIMIT $2"), "$2 is the overfetch budget");
        assert!(
            ranked.contains("p.tag && $3::text[]"),
            "$3 is the tag array"
        );
        // By COMPARISON, not by cast. `>= $4` / `<= $5` and `>= $5` /
        // `<= $4` cast identically and bind identically; only the direction
        // tells `since` from `until`, and swapping them leaves every other
        // test in the workspace green.
        assert!(
            ranked.contains("TIMESTAMPTZ '1970-01-01') >= $4"),
            "$4 is `since` — a LOWER bound"
        );
        assert!(
            ranked.contains("TIMESTAMPTZ '1970-01-01') <= $5"),
            "$5 is `until` — an UPPER bound"
        );
        assert!(
            ranked.contains("($6::uuid IS NULL OR p.memory_id < $6)"),
            "$6 is the recency cursor, and it pages DOWNWARD"
        );
        assert!(
            ranked.contains("p.owner_id = ANY($7::uuid[])"),
            "$7 is the owner set, ON THE PROJECTION, for the composite GIN"
        );
        assert!(
            ranked.contains("p.schema_id = ANY($8::text[])"),
            "$8 is the participating schema set, a row predicate now"
        );
        assert!(
            !ranked.contains("$9"),
            "the ranked arm binds eight parameters; a ninth is a shift"
        );
        assert!(
            !ranked.contains("length("),
            "the unused-parameter guard went with the LIKE bind it kept alive"
        );
        // The ranked arm's OWNER never comes through a join — that is what
        // the composite `gin(owner_id, search_tsv)` needs, and it is what
        // The head restriction names `memory` and `memory_head` in an
        // EXISTS, so the property is asserted directly rather than by a
        // blanket string test for `proxima_core.memory`.
        assert!(
            !ranked.contains("m.owner_id"),
            "the ranked arm's owner is on the projection, never reached through memory"
        );
        assert!(
            ranked.contains("FROM proxima_core.projection p, q"),
            "the projection is the driving relation"
        );

        let substring = super::substring_sql(&[&note], &req).expect("substring");
        assert!(substring.contains("LIKE $1 ESCAPE"), "$1 is the pattern");
        assert!(substring.contains("LIMIT $2"), "$2 is the overfetch budget");
        assert!(substring.contains("c.tags && $3::text[]"), "$3 is the tags");
        assert!(
            substring.contains("TIMESTAMPTZ '1970-01-01') >= $4"),
            "$4 is `since` — a LOWER bound"
        );
        assert!(
            substring.contains("TIMESTAMPTZ '1970-01-01') <= $5"),
            "$5 is `until` — an UPPER bound"
        );
        assert!(
            substring.contains("($6::uuid IS NULL OR c.t < $6)"),
            "$6 is the recency cursor, and it pages DOWNWARD"
        );
        assert!(
            substring.contains("JOIN proxima_core.memory m ON m.t = c.t"),
            "a sidecar carries no owner; the leg must drive from memory"
        );
        assert!(
            substring.contains("m.owner_id = ANY($7::uuid[])"),
            "$7 is the owner set, at candidate time and not only at admit"
        );
        assert!(
            substring.contains("m.schema_id = ANY($8::text[])"),
            "$8 is the MISSING schema set"
        );
        assert!(
            !substring.contains("$9"),
            "the substring arm binds eight parameters; a ninth is a shift"
        );
        assert!(
            !substring.contains("websearch_to_tsquery"),
            "the substring arm referenced a query-side CTE it never read"
        );

        // Without tags the tag predicate is absent from both, and nothing
        // else moves: `$3` simply goes unmentioned.
        req.tags.clear();
        let untagged = super::ranked_projection_sql(&flavor, &req, true).expect("ranked");
        assert!(!untagged.contains("p.tag"));
        assert!(untagged.contains("p.owner_id = ANY($7::uuid[])"));
    }

    /// Admission's filters are on the CANDIDATE side, in both arms, and
    /// only when the request asked for them.
    ///
    /// `request_with_tags` is `HeadsOnly` with `kind = Fact` — the tool
    /// default and what `core_recall` sends. `IncludeSuperseded` must render
    /// no head restriction: that request wants the revisions, so restricting
    /// its window would be a different bug in the other direction. The two
    /// filters are INDEPENDENT: a kind-filtered `IncludeSuperseded` request
    /// is reachable
    /// (`core_search_memories` takes both parameters) and still needs its
    /// kind mirror.
    #[test]
    fn the_candidate_window_is_not_spent_on_rows_admit_will_drop() {
        let note = proxima_core::FlavorRegistry::new()
            .freeze_or_panic_for_tests()
            .search_projections()
            .iter()
            .find(|projection| projection.schema_id.as_str() == "core/agent-note-v1")
            .expect("core/agent-note-v1 is a search surface")
            .clone();
        let mut req = request_with_tags();
        let flavor = super::FlavorScan {
            schemas: vec![&note],
        };

        let ranked = super::ranked_projection_sql(&flavor, &req, true).expect("ranked");
        assert!(
            ranked.contains("JOIN proxima_core.memory_head hh")
                && ranked.contains("ON hh.handle = hm.handle AND hh.t = hm.t")
                && ranked.contains("WHERE hm.t = p.memory_id"),
            "HeadsOnly must reach the head through the memory row's handle, \
             not spend the window on superseded revisions"
        );
        let substring = super::substring_sql(&[&note], &req).expect("substring");
        assert!(
            substring.contains("FROM proxima_core.memory_head hh")
                && substring.contains("WHERE hh.handle = m.handle AND hh.t = m.t"),
            "the substring window is shared across the missing schemas now; \
             it must not be spent on superseded revisions either"
        );

        assert!(
            ranked.contains("AND hm.kind = 'fact'"),
            "the projection has no kind column, so the kind filter admit \
             applies has to be mirrored through the memory row"
        );
        assert!(
            substring.contains("AND m.kind = 'fact'"),
            "…and the substring leg already holds that row, so it is a plain \
             column predicate there"
        );

        req.supersession = SupersessionStatus::IncludeSuperseded;
        let ranked = super::ranked_projection_sql(&flavor, &req, true).expect("ranked");
        assert!(
            !ranked.contains("memory_head"),
            "IncludeSuperseded keeps the unfiltered window"
        );
        assert!(
            ranked.contains("AND hm.kind = 'fact'"),
            "…but the kind filter is independent of it: this request shape is \
             reachable and admit still applies the kind"
        );
        let substring = super::substring_sql(&[&note], &req).expect("substring");
        assert!(
            !substring.contains("memory_head"),
            "IncludeSuperseded keeps the unfiltered window"
        );

        req.kind = None;
        let ranked = super::ranked_projection_sql(&flavor, &req, true).expect("ranked");
        assert!(
            !ranked.contains("proxima_core.memory"),
            "with neither filter the ranked arm reads the projection ALONE, \
             as it always did"
        );

        // ONE schema id registered under TWO kinds: `schema_id` cannot
        // narrow it away, so only the kind predicate keeps the doomed rows
        // out of the window.
        req.kind = Some(EntityKind::Perspective);
        let ranked = super::ranked_projection_sql(&flavor, &req, true).expect("ranked");
        assert!(
            ranked.contains("FROM proxima_core.memory hm\n") && !ranked.contains("memory_head"),
            "a kind-only restriction probes the memory row and nothing else"
        );
        assert!(
            ranked.contains("AND hm.kind = 'perspective'"),
            "the literal is the REQUEST's kind, not a constant"
        );
    }

    /// How `search_admit_sql` spells a top-level conjunct: a newline, the
    /// WHERE body's indentation, and `AND `. The `OR` continuation lines
    /// inside a predicate are indented further, so they do not count.
    const ADMIT_PREDICATE_PREFIX: &str = "\n            AND ";

    /// Admit's filters: owner, kind, `schema_id`, since, until — plus the
    /// candidate id set itself, which is `m.t = ANY($1)` in the WHERE and
    /// therefore not one of these. Five, and supersession makes six by
    /// changing the FROM clause rather than adding a predicate.
    ///
    /// This number exists to be hard to raise by accident. Raising it is a
    /// claim that the candidate window already applies whatever was added.
    const ADMIT_PREDICATES: usize = 5;

    /// The class guard: every filter `search_admit_sql` applies is mirrored
    /// on the candidate side.
    ///
    /// Admit narrowing the hit set by something the candidate window
    /// already spent slots on is the defect class this closes, and a claim
    /// that the remaining filters are mirrored is exactly what cannot be
    /// trusted in prose. So: enumerate admit's binds, require each to be
    /// accounted for, and FAIL when admit grows one more.
    ///
    /// Counting BINDS is not enough — a filter needs no bind at all.
    /// `AND NOT EXISTS (SELECT 1 FROM proxima_core.cooled cl WHERE
    /// cl.t = m.t)` adds a predicate and no `$n`, so the PREDICATE count is
    /// pinned as well.
    #[test]
    fn every_admit_filter_is_mirrored_on_the_candidate_side() {
        // Admit's binds, and where the candidate side answers each.
        const MIRRORS: &[(&str, &str)] = &[
            ("$1", "the candidate id set itself — this IS the window"),
            ("$2", "m.owner_id — `p.owner_id = ANY($7::uuid[])`"),
            (
                "$3",
                "kind — `admit_side_restriction` renders `hm.kind`/`m.kind`",
            ),
            ("$4", "schema_id — `p.schema_id = ANY($8::text[])`"),
            (
                "$5",
                "since — a `uuid_extract_timestamp(...) >= $4` row predicate",
            ),
            (
                "$6",
                "until — a `uuid_extract_timestamp(...) <= $5` row predicate",
            ),
        ];
        for heads_only in [false, true] {
            let admit = super::search_admit_sql(heads_only);
            for (bind, why) in MIRRORS {
                assert!(
                    admit.contains(bind),
                    "admit no longer binds {bind} ({why}); re-derive the mirror"
                );
            }
            assert!(
                !admit.contains("$7"),
                "admit grew a seventh filter. Mirror it in \
                 `admit_side_restriction` or prove the candidate side already \
                 applies it, then extend MIRRORS. Do not delete this line."
            );
            // The bind-free door. `search_admit_sql` renders every top-level
            // conjunct at one indentation, so counting that spelling counts
            // the filters — including the ones that carry no `$n` and would
            // otherwise walk past MIRRORS unnoticed.
            assert_eq!(
                admit.matches(ADMIT_PREDICATE_PREFIX).count(),
                ADMIT_PREDICATES,
                "admit's predicate count moved. A filter with no bind is \
                 still a filter the candidate window has to know about: \
                 mirror it in `admit_side_restriction`, or prove the \
                 candidate side already applies it, then update this count \
                 and MIRRORS together. Do not delete this line."
            );
        }
        // …and supersession, which is not a bind but a shape.
        assert_ne!(
            super::search_admit_sql(true),
            super::search_admit_sql(false),
            "supersession is admit's seventh filter, spelled as a FROM clause"
        );
        assert!(
            super::search_admit_sql(true).contains("proxima_core.memory_head h"),
            "…and that is the shape `admit_side_restriction` mirrors"
        );
    }

    /// A request the builders can render every clause of.
    pub(super) fn request_with_tags() -> MemorySearchRequest {
        MemorySearchRequest {
            owner: OwnerRef::Personal(proxima_core::UserId::new(uuid::Uuid::nil())),
            read_owners: vec![OwnerRef::Personal(proxima_core::UserId::new(
                uuid::Uuid::nil(),
            ))],
            query: "needle".into(),
            mode: SearchMode::Lexical,
            supersession: SupersessionStatus::HeadsOnly,
            limit: 8,
            kind: Some(EntityKind::Fact),
            schema_id: None,
            tags: vec!["bucket-0".into()],
            tag_match: TagMatch::Any,
            since: None,
            until: None,
            order: SearchOrder::Relevance,
            min_score: None,
            semantic_weight: None,
            after: None,
            query_embedding: None,
            embedding_model_id: None,
        }
    }

    /// The bands the builder renders are the bands flavor #0 DECLARES,
    /// resolved by name.
    ///
    /// It has to pin that by calling the BUILDERS. Asserting only
    /// `Band::parts()` and `normalization_arg()` tests properties of the
    /// declaration, not of the renderer, and leaves a hardcoded
    /// `("0.50", "0.50")` inside `ranked_projection_sql` passing. The second
    /// half below renders from a PERTURBED band and requires the emitted SQL
    /// to move with it; a renderer holding a literal cannot pass both
    /// halves.
    #[test]
    // Three bands, each with a declared half and a rendered half, and
    // the perturbation that kills a hardcode. Splitting it would put
    // one band in a second function to forget the other two in — which
    // is the shape this test exists to correct.
    #[allow(clippy::too_many_lines)]
    fn the_renderer_reads_every_band_from_the_declaration() {
        use proxima_core::flavor::{
            BAND_NAME_EXACT, BAND_NAME_RESCUE, BAND_NAME_SUBSTRING, Band,
            TS_RANK_NORMALIZATION_LOG_LENGTH_SCALE, TS_RANK_NORMALIZATION_NONE,
            TS_RANK_NORMALIZATION_SCALE,
        };

        let projections = proxima_core::FlavorRegistry::new()
            .freeze_or_panic_for_tests()
            .search_projections()
            .iter()
            .filter(|projection| {
                proxima_core::FLAVOR_0.declares_sidecar_table(&projection.sidecar_table)
            })
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            !projections.is_empty(),
            "flavor #0 has projected schemas to render"
        );
        let req = request_with_tags();
        for projection in &projections {
            let exact = super::band(projection, BAND_NAME_EXACT).expect("core declares `exact`");
            let rescue = super::band(projection, BAND_NAME_RESCUE).expect("core declares `rescue`");
            let substring =
                super::band(projection, BAND_NAME_SUBSTRING).expect("core declares `substring`");

            assert_eq!(exact.parts(), ("0.50".to_owned(), "0.50".to_owned()));
            assert_eq!(rescue.parts(), ("0.25".to_owned(), "0.20".to_owned()));
            assert_eq!(
                substring.parts(),
                ("0.25".to_owned(), "0.00".to_owned()),
                "the substring arm admits, it does not rank: zero width"
            );
            // The normalization flag each arm renders, declared.
            assert_eq!(exact.normalization, TS_RANK_NORMALIZATION_SCALE);
            assert_eq!(exact.normalization_arg(), ", 32");
            assert_eq!(rescue.normalization, TS_RANK_NORMALIZATION_LOG_LENGTH_SCALE);
            assert_eq!(rescue.normalization_arg(), ", 33", "`1|32` is 33");
            assert_eq!(substring.normalization, TS_RANK_NORMALIZATION_NONE);
            assert_eq!(
                substring.normalization_arg(),
                "",
                "the substring arm calls no ts_rank to normalize"
            );

            // …and what the RENDERER did with them.
            let flavor = super::FlavorScan {
                schemas: vec![projection],
            };
            let ranked = super::ranked_projection_sql(&flavor, &req, true).expect("ranked");
            assert!(
                ranked.contains("THEN 0.50 + LEAST(COALESCE(ts_rank_cd("),
                "the exact arm opens at the declared floor"
            );
            assert!(
                ranked.contains(", 32), 0.0), 1.0) * 0.50"),
                "…normalizes with the declared flag and fills the declared width"
            );
            assert!(
                ranked.contains("THEN 0.25 + LEAST(COALESCE(ts_rank("),
                "the rescue arm opens at the declared floor"
            );
            assert!(
                ranked.contains(", 33), 0.0) * 100.0, 1.0) * 0.20"),
                "…normalizes with the declared flag and fills the declared width"
            );
            let substring_sql = super::substring_sql(&[projection], &req).expect("substring");
            assert!(
                substring_sql.contains("0.25::real AS lexical_score"),
                "the substring arm stamps the declared flat floor"
            );
        }

        // The mutant killer. Perturb ALL THREE bands in a projection the
        // renderer is handed, and require the SQL to follow. Perturbing only
        // `exact` left the rescue floor, the rescue width and the substring
        // floor hardcodable with the whole workspace green.
        let mut moved = projections[0].clone();
        moved.bands = Box::leak(Box::new([
            Band {
                name: BAND_NAME_EXACT,
                floor: 0.10,
                ceiling: 0.90,
                normalization: TS_RANK_NORMALIZATION_NONE,
            },
            Band {
                name: BAND_NAME_RESCUE,
                floor: 0.02,
                ceiling: 0.09,
                normalization: TS_RANK_NORMALIZATION_SCALE,
            },
            Band {
                name: BAND_NAME_SUBSTRING,
                floor: 0.01,
                ceiling: 0.01,
                normalization: TS_RANK_NORMALIZATION_NONE,
            },
        ]));
        let flavor = super::FlavorScan {
            schemas: vec![&moved],
        };
        let ranked = super::ranked_projection_sql(&flavor, &req, true).expect("ranked");
        assert!(
            ranked.contains("THEN 0.10 + LEAST(COALESCE(ts_rank_cd("),
            "the exact floor is READ, not written into the builder"
        );
        assert!(
            ranked.contains("), 0.0), 1.0) * 0.80"),
            "the exact width is READ, and a declared `NONE` normalization \
             renders no argument"
        );
        assert!(
            ranked.contains("THEN 0.02 + LEAST(COALESCE(ts_rank("),
            "the rescue floor is READ too"
        );
        assert!(
            ranked.contains(", 32), 0.0) * 100.0, 1.0) * 0.07"),
            "…and so are the rescue width and the rescue normalization flag"
        );
        assert!(
            !ranked.contains("THEN 0.50 +") && !ranked.contains("THEN 0.25 +"),
            "nothing in the builder may still carry core's literals"
        );
        assert!(
            !ranked.contains(", 33)"),
            "…including the normalization flags"
        );
        let substring_sql = super::substring_sql(&[&moved], &req).expect("substring");
        assert!(
            substring_sql.contains("0.01::real AS lexical_score"),
            "the substring floor is READ as well"
        );
        assert!(
            !substring_sql.contains("0.25::real"),
            "…and core's literal is gone from the substring arm too"
        );
    }

    /// The band-comparability gate has a consumer, and the consumer moves
    /// no score.
    ///
    /// `core_search_flavors` admits a non-core projection only under
    /// `CoreBands` and only under `RankSource::Projection`. The admitted set
    /// is unchanged by that gate — the code flavor declares no `tag_column`,
    /// so it is already excluded in every request shape — which is what this
    /// test pins.
    #[test]
    fn the_merge_admits_only_flavors_that_declare_a_comparable_shape() {
        let frozen = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        for projection in frozen.search_projections() {
            if proxima_core::FLAVOR_0.declares_sidecar_table(&projection.sidecar_table) {
                assert!(
                    matches!(
                        projection.band_comparability,
                        proxima_core::flavor::BandComparability::CoreBands
                    ) && projection.rank_source.is_projection(),
                    "flavor #0's own schemas must pass the gate they define"
                );
                continue;
            }
            // Every non-core projected schema in this tree is excluded by
            // the tag gate as well. If one ever declares a `tag_column`,
            // this assertion is what says whether the new gate or the old
            // one is doing the work.
            assert!(
                projection.tag_column.is_none(),
                "a foreign schema declaring a tag_column would reach the merge; \
                 re-derive the admitted-set-neutrality claim"
            );
        }
    }
}
