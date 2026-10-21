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
//! Every eligible declared projection participates in unscoped search.
//! Tags narrow rows and require the schema to declare a `tag_column`.
//! Participating flavors declare `BandComparability::CoreBands` and
//! `RankSource::Projection` — a score this merge cannot compare and a shape
//! this renderer cannot serve are both exclusions the contract states. The
//! semantic arm honours the same scope: its candidate scan requires one
//! of those flavors' projection rows to satisfy the schema set and the
//! request's optional tag predicate before a candidate is accepted.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::error::map_err;
use crate::pg_enums::PgMemoryKind;
use crate::pg_ident::PgIdent;
use crate::pgvector::set_hnsw_search_sql;
use crate::projection::{projection_key_ident, sidecar_text_sql};
use crate::tuning::PgTuning;
use proxima_core::flavor::{BandComparability, LanguagePolicy, SubstringArm};
use proxima_core::verbs::query::{
    EntityKind, MAX_SEARCH_PAGE_LIMIT, MemorySearchPage, MemorySearchRequest, MemorySearchResult,
    SearchCursor, SearchMode, SearchOrder, SupersessionStatus, TagMatch, like_pattern,
};
use proxima_core::verbs::schema::{MemorySearchProjection, PayloadKind, RenderBands};
use proxima_core::{MemoryId, OwnerId, SchemaId, StorageError};
use sqlx::{PgConnection, Postgres, Transaction};

use super::embedding_candidates::{SEMANTIC_SCAN_CAP, next_candidate_window};
use super::lineage::load_one_schema_snippets_on_connection;

mod hybrid;

use hybrid::{HYBRID_CANDIDATE_WINDOW, apply_hybrid_fusion, uses_hybrid_fusion};

/// How far past the caller's `limit` one statement fetches before the merge
/// trims. The CAP is declared — [`ProjectionSpec::overfetch_k`], a
/// shard-level property and therefore the flavor's — but the request
/// scaling has no contract home, because it is a function of the request
/// rather than of the shard.
const REQUEST_OVERFETCH_FACTOR: u32 = 20;

/// The tagged semantic scan's tag-array bind…
const SEMANTIC_TAGS_BIND: usize = 7;
/// …and the first of its per-flavor schema-set binds, one per participating
/// flavor in `flavors` order.
const SEMANTIC_SCHEMA_BIND_BASE: usize = 8;

#[derive(Debug, Clone)]
struct Hit {
    t: uuid::Uuid,
    // Membership is distinct from a zero raw score.
    lexical_score: Option<f32>,
    similarity_score: Option<f32>,
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
    by: uuid::Uuid,
    via: Option<uuid::Uuid>,
    t: uuid::Uuid,
    kind: PgMemoryKind,
    schema_id: String,
    created_at: time::OffsetDateTime,
    occurred_at: Option<time::OffsetDateTime>,
}

/// Request search on a caller-owned transaction connection. All candidate,
/// admission, and snippet statements are serialized on that connection.
pub(crate) async fn search_memories_on_connection(
    connection: &mut Transaction<'static, Postgres>,
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    tuning: &PgTuning,
    vectors: &crate::vector_index::StoreVectors,
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
    let mut hits = BTreeMap::<uuid::Uuid, Hit>::new();
    match req.mode {
        SearchMode::Lexical => merge_hits(
            &mut hits,
            scan_flavors_on_connection(connection.as_mut(), req, &flavors, limit, true).await?,
        ),
        SearchMode::Semantic => merge_hits(
            &mut hits,
            scan_embeddings_on_connection(
                connection,
                req,
                &flavors,
                tuning,
                candidate_overfetch(req, limit, SEMANTIC_SCAN_CAP),
                vectors,
            )
            .await?,
        ),
        SearchMode::Hybrid => {
            if req.semantic.is_some() {
                merge_hits(
                    &mut hits,
                    scan_flavors_on_connection(connection.as_mut(), req, &flavors, limit, true)
                        .await?,
                );
                merge_hits(
                    &mut hits,
                    scan_embeddings_on_connection(
                        connection,
                        req,
                        &flavors,
                        tuning,
                        candidate_overfetch(req, limit, SEMANTIC_SCAN_CAP),
                        vectors,
                    )
                    .await?,
                );
            } else {
                merge_hits(
                    &mut hits,
                    scan_flavors_on_connection(connection.as_mut(), req, &flavors, limit, true)
                        .await?,
                );
            }
        }
    }
    let mut admitted = admit_hits_on_connection(connection.as_mut(), req, &hits).await?;
    if uses_hybrid_fusion(req) {
        apply_hybrid_fusion(req, &mut admitted, &hits);
    }
    let mut page = page_hits(req, limit, admitted);
    hydrate_snippets_on_connection(connection.as_mut(), projections, &mut page.results).await?;
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
/// Very deep non-hybrid cursor pages can still exhaust the declared cap.
/// Hybrid uses a separate fixed candidate window on every page.
fn overfetch(limit: u32, overfetch_k: u32, after: Option<SearchCursor>) -> u32 {
    let base = limit.saturating_mul(REQUEST_OVERFETCH_FACTOR).max(limit);
    // Relevance candidates are ranked from the beginning before the cursor
    // is applied. Retain room for the prior rows, this page, and a has_more
    // witness even when the caller shrinks its page size. Recency keysets
    // already filter the lexical SQL and keep their existing scan budget.
    let needed = match after {
        Some(SearchCursor::Relevance { seen, .. }) => seen.saturating_add(limit).saturating_add(1),
        _ => base,
    };
    base.max(needed).min(overfetch_k)
}

/// Hybrid retains one fixed window regardless of page size, cursor depth,
/// or requested output order. Other modes retain their existing overfetch.
fn candidate_overfetch(req: &MemorySearchRequest, limit: u32, cap: u32) -> u32 {
    if uses_hybrid_fusion(req) {
        HYBRID_CANDIDATE_WINDOW.min(cap)
    } else {
        overfetch(limit, cap, req.after)
    }
}

fn candidate_order(req: &MemorySearchRequest) -> SearchOrder {
    if uses_hybrid_fusion(req) {
        SearchOrder::Relevance
    } else {
        req.order
    }
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
    /// The windows this flavor's statements render, taken off the head.
    ///
    /// Held rather than looked up: [`Self::new`] is the only way to build a
    /// scan and it refuses a head with no render bands, so every renderer
    /// below reads a value freeze proved instead of resolving three names
    /// by string on the query path.
    bands: RenderBands,
}

impl<'a> FlavorScan<'a> {
    /// A scan over schemas the selection has already admitted.
    ///
    /// `None` for an empty set or a head that declares no render bands —
    /// both of which `core_search_flavors` filters out before it gets here,
    /// which is what makes [`Self::bands`] total.
    fn new(schemas: Vec<&'a MemorySearchProjection>) -> Option<Self> {
        let bands = schemas.first()?.render_bands?;
        Some(Self { schemas, bands })
    }

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
        // A flavor's score enters this merge only if its flavor
        // says the score is comparable, and only if its read shape is one
        // this renderer can serve.
        if !matches!(projection.band_comparability, BandComparability::CoreBands) {
            continue;
        }
        // The same admission as before, spelled as the value it proves: a
        // projection-ranked schema is exactly one freeze resolved render
        // bands for.
        if projection.render_bands.is_none() {
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
        .filter_map(FlavorScan::new)
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
                existing.lexical_score = maximum_present(existing.lexical_score, hit.lexical_score);
                existing.similarity_score =
                    maximum_present(existing.similarity_score, hit.similarity_score);
            })
            .or_insert(hit);
    }
}

fn maximum_present(left: Option<f32>, right: Option<f32>) -> Option<f32> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

/// One job per participating FLAVOR, not one per schema.
///
/// A flavor's ranked arm is one statement against its own shard, plus at
/// most one substring statement over the schemas that arm returned nothing
/// for. A request that reaches a second flavor runs the same
/// again against that flavor's shard.
async fn scan_flavors_on_connection(
    connection: &mut PgConnection,
    req: &MemorySearchRequest,
    flavors: &[FlavorScan<'_>],
    limit: u32,
    rescue: bool,
) -> Result<Vec<Hit>, StorageError> {
    let mut hits = Vec::new();
    for flavor in flavors {
        hits.extend(scan_one_flavor_on_connection(connection, req, flavor, limit, rescue).await?);
    }
    Ok(hits)
}

async fn scan_one_flavor_on_connection(
    connection: &mut PgConnection,
    req: &MemorySearchRequest,
    flavor: &FlavorScan<'_>,
    limit: u32,
    rescue: bool,
) -> Result<Vec<Hit>, StorageError> {
    let recency_t = match req.after {
        Some(SearchCursor::Recency { memory_id, .. }) if !uses_hybrid_fusion(req) => {
            Some(memory_id.into_inner())
        }
        _ => None,
    };
    let owner_ids: Vec<uuid::Uuid> = req
        .read_owners
        .iter()
        .copied()
        .map(OwnerId::into_inner)
        .collect();
    let schema_ids: Vec<&str> = flavor
        .schemas
        .iter()
        .map(|projection| projection.schema_id.as_str())
        .collect();
    let budget = candidate_overfetch(req, limit, flavor.head().overfetch_k);
    let overfetch = i64::from(budget);
    let tags = (!req.tags.is_empty()).then_some(req.tags.as_slice());
    let sql = crate::search_candidates::call_sql(&flavor.head().projection_table, false)?;
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
        .bind(rescue)
        .bind(matches!(candidate_order(req), SearchOrder::Recency))
        .bind(matches!(req.supersession, SupersessionStatus::HeadsOnly))
        .bind(kind_literal(req.kind))
        .bind(matches!(req.tag_match, TagMatch::All))
        .fetch_all(&mut *connection)
        .await
        .map_err(map_err)?;
    let present: BTreeSet<&str> = rows.iter().map(|row| row.schema_id.as_str()).collect();
    let mut hits: Vec<Hit> = rows
        .iter()
        .map(|row| Hit {
            t: row.t,
            lexical_score: Some(row.lexical_score.max(0.0)),
            similarity_score: None,
        })
        .collect();
    let missing: Vec<&MemorySearchProjection> = flavor
        .schemas
        .iter()
        .copied()
        .filter(|projection| {
            !present.contains(projection.schema_id.as_str())
                && projection.substring != SubstringArm::Off
        })
        .collect();
    if !missing.is_empty() {
        let missing_ids: Vec<&str> = missing.iter().map(|p| p.schema_id.as_str()).collect();
        let sql = crate::search_candidates::call_sql(&flavor.head().projection_table, true)?;
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
            .bind(rescue)
            .bind(matches!(candidate_order(req), SearchOrder::Recency))
            .bind(matches!(req.supersession, SupersessionStatus::HeadsOnly))
            .bind(kind_literal(req.kind))
            .bind(matches!(req.tag_match, TagMatch::All))
            .fetch_all(&mut *connection)
            .await
            .map_err(map_err)?;
        hits.extend(rows.into_iter().map(|row| Hit {
            t: row.t,
            lexical_score: Some(row.lexical_score.max(0.0)),
            similarity_score: None,
        }));
    }
    if uses_hybrid_fusion(req) {
        // Ranked and optional substring statements share the flavor's cap.
        hits.sort_by(|a, b| {
            b.lexical_score
                .unwrap_or_default()
                .total_cmp(&a.lexical_score.unwrap_or_default())
                .then_with(|| b.t.cmp(&a.t))
        });
        hits.truncate(usize::try_from(budget).unwrap_or(usize::MAX));
    }
    Ok(hits)
}

/// One leg of the substring arm: a sidecar table and every schema stored in
/// it. Two schemas sharing a sidecar (core's agent-derivation registers as
/// Abstraction and Perspective) are one leg, so the sidecar is scanned once
/// for one answer.
struct SidecarLeg<'a> {
    /// The first projection declared over the sidecar; the leg reads the
    /// sidecar's text and key columns through it.
    projection: &'a MemorySearchProjection,
    /// The schemas stored in the sidecar, each once.
    schema_ids: BTreeSet<&'a str>,
}

impl SidecarLeg<'_> {
    /// The leg's own schema ids as SQL text literals. The leg joins
    /// `memory`, and only a memory of one of these schemas can have a row in
    /// its sidecar, so naming them lets the planner reach the owner's
    /// memories of just those schemas through `memory_owner_schema_t_idx`
    /// instead of reading every memory the request's `$8` names (#490).
    fn schema_literals(&self) -> String {
        self.schema_ids
            .iter()
            .map(|schema| format!("'{}'", schema.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn sidecar_legs<'a>(
    schemas: impl IntoIterator<Item = &'a MemorySearchProjection>,
) -> Vec<SidecarLeg<'a>> {
    let mut by_table = BTreeMap::<&str, SidecarLeg<'a>>::new();
    for projection in schemas {
        by_table
            .entry(projection.sidecar_table.as_str())
            .or_insert_with(|| SidecarLeg {
                projection,
                schema_ids: BTreeSet::new(),
            })
            .schema_ids
            .insert(projection.schema_id.as_str());
    }
    by_table.into_values().collect()
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
/// bound schema-id array, which narrows rather than selects: a leg also names
/// the schemas stored in its own sidecar ([`SidecarLeg`]), so after a query
/// that matches nothing, when the array holds every schema, a leg still reads
/// only the memories its sidecar can have a row for.
///
/// The text expression is the DECLARED search fields
/// (`sidecar_text_sql`) — the same expression the stored vector and the
/// snippet are built from, so the three cannot disagree about what a row's
/// searchable text is.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
fn substring_sql(
    schemas: &[&MemorySearchProjection],
    bands: RenderBands,
    req: &MemorySearchRequest,
) -> Result<String, StorageError> {
    let scanned = sidecar_legs(schemas.iter().copied());
    let legs = scanned
        .iter()
        .map(|leg| substring_leg_sql(leg, bands, req))
        .collect::<Result<Vec<_>, StorageError>>()?;
    // Every leg projects its memory id as `t` whatever its sidecar calls
    // the column, so the union's own ordering stays one spelling.
    if let ([only], [leg]) = (legs.as_slice(), scanned.as_slice()) {
        let key = projection_key_ident(leg.projection)?;
        let order_by = match candidate_order(req) {
            SearchOrder::Relevance => format!("lexical_score DESC, c.{} DESC", key.as_str()),
            SearchOrder::Recency => format!("c.{} DESC", key.as_str()),
        };
        // SQL-POLICY: PgIdent
        return Ok(format!(
            "{only}
          ORDER BY {order_by}
          LIMIT $2"
        ));
    }
    let order_by = match candidate_order(req) {
        SearchOrder::Relevance => "s.lexical_score DESC, s.t DESC",
        SearchOrder::Recency => "s.t DESC",
    };
    let union = legs.join("\n          UNION ALL\n");
    // SQL-POLICY: PgIdent
    Ok(format!(
        "SELECT s.t, s.lexical_score
               FROM (
          {union}
                    ) s
              ORDER BY {order_by}
              LIMIT $2"
    ))
}

/// One sidecar's leg of the substring arm.
///
/// There is no query-side CTE here: the `LIKE` predicate reads no tsquery,
/// so computing a `websearch_to_tsquery` and a `plainto_tsquery` per
/// statement would be paid for and never read.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
fn substring_leg_sql(
    leg: &SidecarLeg<'_>,
    bands: RenderBands,
    req: &MemorySearchRequest,
) -> Result<String, StorageError> {
    let projection = leg.projection;
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
    substring_leg_sql_parts(
        leg,
        bands,
        &tag_pred,
        &admit_side_restriction(req, SUBSTRING_MEMORY_ID),
    )
}

fn substring_leg_sql_parts(
    leg: &SidecarLeg<'_>,
    bands: RenderBands,
    tag_pred: &str,
    admit_pred: &str,
) -> Result<String, StorageError> {
    let projection = leg.projection;
    let own_schemas = leg.schema_literals();
    let table = PgIdent::table(&projection.sidecar_table)?;
    let key = projection_key_ident(projection)?;
    let search_text = sidecar_text_sql(projection)?;
    // A flat band: the substring arm ranks nothing, it only admits — hence
    // zero width, and no `ts_rank` call to normalize.
    let (floor, _) = bands.substring.parts();
    // Same reason as the ranked arm's: this window is shared across the
    // missing schemas now, so a row admission will drop must not spend a
    // slot in it. See `admit_side_restriction`.
    // SQL-POLICY: PgIdent
    // `c.{key}` is the sidecar's DECLARED memory-key column and `m.t` the
    // kernel memory table's own; the leg aliases its projection back to `t`
    // so the row shape and the union's ordering are one spelling whatever
    // a flavor names its column.
    Ok(format!(
        "SELECT c.{key} AS t,
                {floor}::real AS lexical_score
           FROM {table} c
           JOIN proxima_core.memory m ON m.t = c.{key} /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
          WHERE m.owner_id = ANY($7::uuid[])
            AND m.schema_id = ANY($8::text[])
            AND m.schema_id = ANY(ARRAY[{own_schemas}]::text[])
            AND (lower({search_text}) LIKE $1 ESCAPE '\\')
            {tag_pred}{admit_pred}
            AND ($4::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.{key}) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.{key}) <= $5)
            AND ($6::uuid IS NULL OR c.{key} < $6)",
        table = table.as_str(),
        key = key.as_str(),
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
/// `LIKE` pattern, and `every_bind_is_where_the_scan_binds_it` pins the
/// emitted placeholder numbering against the order the scan binds.
///
/// `schema_id` is a row predicate — `= ANY($8)` — rather than a
/// projection-selection criterion, which is what lets one statement serve
/// the whole participating set.
///
/// The admit-side restriction ([`admit_side_restriction`]) is the third
/// predicate that has to be here rather than only at admit. See that
/// function.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
fn ranked_projection_sql(
    flavor: &FlavorScan<'_>,
    req: &MemorySearchRequest,
    rescue: bool,
) -> Result<String, StorageError> {
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
    ranked_projection_sql_parts(
        flavor,
        rescue,
        &tag_pred,
        &admit_side_restriction(req, RANKED_MEMORY_ID),
        candidate_order(req),
        None,
    )
}

fn ranked_projection_sql_parts(
    flavor: &FlavorScan<'_>,
    rescue: bool,
    tag_pred: &str,
    admit_pred: &str,
    order: SearchOrder,
    rescue_switch: Option<&str>,
) -> Result<String, StorageError> {
    let head = flavor.head();
    let table = PgIdent::table(&head.projection_table)?;
    let tsv = "p.search_tsv";
    let rescue_guard = rescue_switch.map_or_else(String::new, |flag| format!("{flag} AND "));
    let multilingual = multilingual(head);
    let rank_tsq = rank_tsquery_expr(multilingual);
    let weights = rank_weight_array(head);
    // The score windows and the `ts_rank` normalization flags are READ OFF
    // the declaration, resolved by name. Raw `ts_rank` is not comparable
    // across corpora; a band is, which is what makes a cross-flavor merge
    // meaningful — and a band whose normalization is undeclared is a window
    // two renderers can fill differently while claiming the same number.
    let exact = flavor.bands.exact;
    let rescue_band = flavor.bands.rescue;
    let (rescue_floor, rescue_width) = rescue_band.parts();
    let rescue_norm = rescue_band.normalization_arg();
    let rescue_score = if rescue {
        format!(
            ", CASE WHEN {rescue_guard}q.any_tsq IS NOT NULL AND {tsv} @@ q.any_tsq
                    THEN {rescue_floor} + LEAST(COALESCE(ts_rank({weights}{tsv}, q.any_tsq{rescue_norm}), 0.0) * 100.0, 1.0) * {rescue_width}
                    ELSE 0.0 END"
        )
    } else {
        String::new()
    };
    let rescue_where = if rescue {
        format!(" OR ({rescue_guard}q.any_tsq IS NOT NULL AND {tsv} @@ q.any_tsq)")
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
    let order_by = match order {
        SearchOrder::Relevance => "lexical_score DESC, p.memory_id DESC",
        SearchOrder::Recency => "p.memory_id DESC",
    };
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
                 OR proxima_core.memory_created_at(p.memory_id) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(p.memory_id) <= $5)
            AND ($6::uuid IS NULL OR p.memory_id < $6)
          ORDER BY {order_by}
          LIMIT $2",
        q_cte = query_side_cte(multilingual),
        table = table.as_str(),
    ))
}

/// The candidate-side spelling of "this row's memory" in the ranked arm.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
const RANKED_MEMORY_ID: &str = "p.memory_id";
/// …and in a substring leg, which already drives `proxima_core.memory m`.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
const SUBSTRING_MEMORY_ID: &str = "m.t";

/// Fixed candidate statements emitted into a projection's migration.
/// Request switches are values; table names and scoring remain contract-owned.
pub(crate) fn projection_candidate_statements(
    projections: &[MemorySearchProjection],
    substring: bool,
    recency: bool,
) -> Result<Option<String>, StorageError> {
    if substring {
        return Ok(projection_substring_candidates(projections, recency)?
            .map(|candidates| candidates.statement(candidates.legs.iter())));
    }
    let Some(flavor) = core_band_scan(projections) else {
        return Ok(None);
    };
    let admit = candidate_authorization("p.memory_id", "");
    ranked_projection_sql_parts(
        &flavor,
        true,
        &format!(
            " AND p.owner_id = ANY(proxima_core.search_owner_ids()){}",
            candidate_tags("p.tag")
        ),
        &admit,
        candidate_order_of(recency),
        Some("$9::boolean"),
    )
    .map(Some)
}

/// The participating schemas the candidate functions render: those on core
/// bands, which the merge can compare.
fn core_band_scan(projections: &[MemorySearchProjection]) -> Option<FlavorScan<'_>> {
    FlavorScan::new(
        projections
            .iter()
            .filter(|projection| {
                matches!(projection.band_comparability, BandComparability::CoreBands)
                    && projection.render_bands.is_some()
            })
            .collect(),
    )
}

const fn candidate_order_of(recency: bool) -> SearchOrder {
    if recency {
        SearchOrder::Recency
    } else {
        SearchOrder::Relevance
    }
}

/// One sidecar's leg of the substring candidate statement and the schemas it
/// reads. A leg whose schemas the request does not name matches no row, so
/// the function leaves it out of the statement it plans.
pub(crate) struct CandidateLeg {
    pub(crate) schemas: Vec<String>,
    pub(crate) sql: String,
}

/// The substring candidate statement as its legs and the ordering that
/// merges them: ONE author of both the whole statement the plan guards read
/// and the per-request statement the migrated function assembles.
pub(crate) struct SubstringCandidates {
    pub(crate) legs: Vec<CandidateLeg>,
    order_by: &'static str,
}

impl SubstringCandidates {
    /// The text a statement over some of the legs is made of: what comes
    /// before the legs, between them and after them. The whole statement the
    /// plan guards read and the one the migrated function assembles per
    /// request are both this frame around their legs.
    pub(crate) fn frame(&self) -> (&'static str, &'static str, String) {
        (
            "SELECT s.t, s.lexical_score FROM (",
            " UNION ALL ",
            format!(") s ORDER BY {} LIMIT $2", self.order_by),
        )
    }

    fn statement<'a>(&self, legs: impl Iterator<Item = &'a CandidateLeg>) -> String {
        let (open, between, close) = self.frame();
        let legs = legs.map(|leg| leg.sql.as_str()).collect::<Vec<_>>();
        format!("{open}{}{close}", legs.join(between))
    }
}

pub(crate) fn projection_substring_candidates(
    projections: &[MemorySearchProjection],
    recency: bool,
) -> Result<Option<SubstringCandidates>, StorageError> {
    let Some(flavor) = core_band_scan(projections) else {
        return Ok(None);
    };
    substring_candidate_legs(&flavor, candidate_order_of(recency))
}

fn substring_candidate_legs(
    flavor: &FlavorScan<'_>,
    order: SearchOrder,
) -> Result<Option<SubstringCandidates>, StorageError> {
    let legs = sidecar_legs(
        flavor
            .schemas
            .iter()
            .copied()
            .filter(|projection| projection.substring != SubstringArm::Off),
    )
    .iter()
    .map(|leg| {
        let tags = leg.projection.tag_column.as_deref().map_or_else(
            || Ok::<_, StorageError>(" AND $3::text[] IS NULL".to_owned()),
            |column| {
                let column = PgIdent::column(column)?;
                Ok(candidate_tags(&format!("c.{}", column.as_str())))
            },
        )?;
        // A head carries its memory's schema (`memory_align_head`), so the
        // leg's own schemas bound the heads it checks as well: without
        // them the head check reads every head the owner has.
        let heads_of_leg = format!(
            " AND authorized_head.schema_id = ANY(ARRAY[{}]::text[])",
            leg.schema_literals()
        );
        let sql = substring_leg_sql_parts(
            leg,
            flavor.bands,
            &tags,
            &candidate_authorization("m.t", &heads_of_leg),
        )?;
        Ok(CandidateLeg {
            schemas: leg
                .schema_ids
                .iter()
                .map(|schema| (*schema).to_owned())
                .collect(),
            sql,
        })
    })
    .collect::<Result<Vec<_>, StorageError>>()?;
    if legs.is_empty() {
        return Ok(None);
    }
    let order_by = match order {
        SearchOrder::Relevance => "s.lexical_score DESC, s.t DESC",
        SearchOrder::Recency => "s.t DESC",
    };
    Ok(Some(SubstringCandidates { legs, order_by }))
}

fn candidate_tags(column: &str) -> String {
    format!(
        " AND ($3::text[] IS NULL OR CASE WHEN $12::boolean \
         THEN {column} @> $3::text[] ELSE {column} && $3::text[] END)"
    )
}

/// The candidate statements' authorization of `memory_id`; `head_filter` is
/// an extra predicate on `authorized_head`, or empty.
fn candidate_authorization(memory_id: &str, head_filter: &str) -> String {
    format!(
        " AND EXISTS (SELECT 1 FROM proxima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ \
         WHERE authorized_memory.t = {memory_id} \
         AND authorized_memory.owner_id = ANY($7::uuid[]) \
         AND authorized_memory.forgotten_at IS NULL \
         AND ($11::text IS NULL OR authorized_memory.kind::text = $11) \
         AND (NOT $10::boolean OR EXISTS (SELECT 1 FROM proxima_core.memory_head authorized_head \
         WHERE authorized_head.handle = authorized_memory.handle \
         AND authorized_head.t = authorized_memory.t{head_filter})))"
    )
}

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
    kind.and_then(PgMemoryKind::of).map(PgMemoryKind::label)
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
/// [`overfetch`] guarantees. owner, `since` and `until` are already row
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
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
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
async fn hydrate_snippets_on_connection(
    connection: &mut PgConnection,
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
    let mut snippets = HashMap::<uuid::Uuid, String>::new();
    for (schema_id, ts) in by_schema {
        let Some(projection) = projections
            .iter()
            .find(|p| p.schema_id.as_str() == schema_id)
        else {
            continue;
        };
        let rows = load_one_schema_snippets_on_connection(connection, projection, ts).await?;
        snippets.extend(rows);
    }
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
                m.kind,
                m.schema_id,
                proxima_core.memory_created_at(m.t) AS created_at,
                m.occurred_at, m.\"by\", m.via
           {from}
          WHERE m.t = ANY($1::uuid[])
            AND m.owner_id = ANY($2::uuid[])
            AND ($3::text IS NULL OR m.kind::text = $3)
            AND ($4::text IS NULL OR m.schema_id = $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(m.t) >= $5)
            AND ($6::timestamptz IS NULL
                 OR proxima_core.memory_created_at(m.t) <= $6)"
    )
}

/// Match-side tsquery. When the schema declares `LanguagePolicy::PerRow`
/// (agent memory tables), OR one `websearch_to_tsquery` per
/// `lexical_languages` row — the query cannot know its language. Schemas
/// that pin one config stay on `lexical_config()`.
fn query_side_cte(multilingual: bool) -> &'static str {
    if multilingual {
        "WITH analyzed_query AS MATERIALIZED (
             SELECT proxima_core.lexical_scrub($1) AS scrubbed,
                    proxima_core.lexical_query_text(proxima_core.lexical_config(),
                        proxima_core.lexical_scrub($1)) AS analyzed
         ), q AS MATERIALIZED (
             SELECT s.scrubbed, s.analyzed,
                    COALESCE(
                        (SELECT proxima_core.tsquery_or_agg(
                                    websearch_to_tsquery(l.config, s.analyzed)
                                    ORDER BY l.config)
                           FROM proxima_core.lexical_languages l),
                        websearch_to_tsquery(proxima_core.lexical_config(), s.analyzed)
                    ) AS tsq,
                    COALESCE(
                        (SELECT proxima_core.tsquery_or_agg(
                                    NULLIF(
                                        replace(
                                            plainto_tsquery(l.config, s.analyzed)::text,
                                            ' & ', ' | '),
                                        '')::tsquery
                                    ORDER BY l.config)
                           FROM proxima_core.lexical_languages l),
                        NULLIF(
                            replace(
                                plainto_tsquery(proxima_core.lexical_config(), s.analyzed)::text,
                                ' & ', ' | '),
                            '')::tsquery
                    ) AS any_tsq
               FROM analyzed_query s
         )"
    } else {
        "WITH q AS MATERIALIZED (
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
        "websearch_to_tsquery(p.lexical_language, q.analyzed)"
    } else {
        "q.tsq"
    }
}

/// Admit vector-port candidates through the ordinary memory read and the
/// participating projections. The same memory predicate is used for final
/// admission, so authorization and request filters have one author.
fn semantic_candidate_admission_sql(
    flavors: &[FlavorScan<'_>],
    req: &MemorySearchRequest,
) -> Result<String, StorageError> {
    let op = tag_operator(req.tag_match);
    let schema_bind_base = if req.tags.is_empty() {
        SEMANTIC_TAGS_BIND
    } else {
        SEMANTIC_SCHEMA_BIND_BASE
    };
    let probes = flavors
        .iter()
        .enumerate()
        .map(|(index, flavor)| {
            let table = PgIdent::table(&flavor.head().projection_table)?;
            let tag_pred = if req.tags.is_empty() {
                String::new()
            } else {
                format!(" AND p.tag {op} ${SEMANTIC_TAGS_BIND}::text[]")
            };
            // SQL-POLICY: PgIdent
            Ok(format!(
                "EXISTS (SELECT 1 FROM {table} p
                          WHERE p.memory_id = m.t
                            AND p.owner_id = ANY($2::uuid[])
                            AND p.schema_id = ANY(${schema_bind}::text[]){tag_pred})",
                table = table.as_str(),
                schema_bind = schema_bind_base + index,
            ))
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    let projection_pred = if probes.is_empty() {
        "FALSE".to_owned()
    } else {
        probes.join(" OR ")
    };
    // SQL-POLICY: PgIdent — each projection relation above is checked.
    Ok(format!(
        "{} AND ({projection_pred})",
        search_admit_sql(matches!(req.supersession, SupersessionStatus::HeadsOnly))
    ))
}

async fn scan_embeddings_on_connection(
    connection: &mut Transaction<'static, Postgres>,
    req: &MemorySearchRequest,
    flavors: &[FlavorScan<'_>],
    tuning: &PgTuning,
    overfetch: u32,
    vectors: &crate::vector_index::StoreVectors,
) -> Result<Vec<Hit>, StorageError> {
    let Some(semantic) = req.semantic.as_ref() else {
        return Err(StorageError::ConstraintViolation(
            "semantic search requires a query embedding".into(),
        ));
    };
    if flavors.is_empty() {
        return Ok(Vec::new());
    }
    let owner_ids: Vec<uuid::Uuid> = req
        .read_owners
        .iter()
        .copied()
        .map(OwnerId::into_inner)
        .collect();
    let schema_sets: Vec<Vec<&str>> = flavors
        .iter()
        .map(|f| f.schemas.iter().map(|p| p.schema_id.as_str()).collect())
        .collect();
    let sql = semantic_candidate_admission_sql(flavors, req)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(set_hnsw_search_sql(tuning)))
        .execute(connection.as_mut())
        .await
        .map_err(map_err)?;
    let mut window = overfetch;
    let mut admitted = BTreeMap::new();
    loop {
        let candidates = match vectors {
            crate::vector_index::StoreVectors::Local => {
                crate::vector_index::local_nearest(connection, &req.read_owners, semantic, window)
                    .await?
            }
            crate::vector_index::StoreVectors::Remote(index) => {
                index.nearest(&req.read_owners, semantic, window).await?
            }
        };
        let fetched = candidates.len();
        let candidate_ids: Vec<_> = candidates
            .iter()
            .map(|candidate| candidate.memory_id.into_inner())
            .collect();
        // SQL-POLICY: PgIdent — the shared memory read plus checked projection probes.
        let mut query = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(&candidate_ids)
            .bind(&owner_ids)
            .bind(kind_literal(req.kind))
            .bind(req.schema_id.as_ref().map(SchemaId::as_str))
            .bind(req.since)
            .bind(req.until);
        if !req.tags.is_empty() {
            query = query.bind(&req.tags);
        }
        for schema_ids in &schema_sets {
            query = query.bind(schema_ids);
        }
        let rows: Vec<AdmitRow> = query
            .fetch_all(connection.as_mut())
            .await
            .map_err(map_err)?;
        let allowed: BTreeSet<_> = rows.into_iter().map(|row| row.t).collect();
        for candidate in candidates {
            let t = candidate.memory_id.into_inner();
            if allowed.contains(&t) {
                admitted.insert(
                    t,
                    Hit {
                        t,
                        lexical_score: None,
                        similarity_score: Some(candidate.similarity_score),
                    },
                );
            }
        }
        let Some(next) = next_candidate_window(window, fetched, admitted.len(), overfetch) else {
            break;
        };
        window = next;
    }
    Ok(admitted.into_values().collect())
}

async fn admit_hits_on_connection(
    connection: &mut PgConnection,
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
        .map(OwnerId::into_inner)
        .collect();
    let hit_ts: Vec<uuid::Uuid> = hits.keys().copied().collect();
    let sql = search_admit_sql(matches!(req.supersession, SupersessionStatus::HeadsOnly));
    // SQL-POLICY: PgIdent
    let rows: Vec<AdmitRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(&hit_ts)
        .bind(&owner_ids)
        .bind(kind_literal(req.kind))
        .bind(req.schema_id.as_ref().map(SchemaId::as_str))
        .bind(req.since)
        .bind(req.until)
        .fetch_all(&mut *connection)
        .await
        .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let hit = hits.get(&row.t)?;
            let kind = EntityKind::from(row.kind);
            let score = match req.mode {
                SearchMode::Semantic => hit.similarity_score.unwrap_or_default(),
                // Hybrid with an embedding is fused after admission. Without
                // an embedding its effective ranking remains lexical.
                SearchMode::Lexical | SearchMode::Hybrid => hit.lexical_score.unwrap_or_default(),
            };
            Some(MemorySearchResult {
                memory_id: MemoryId::new(row.t),
                writer: proxima_core::WriterAttribution {
                    by: row.by,
                    via: row.via,
                },
                kind,
                schema_id: SchemaId::new(row.schema_id),
                created_at: row.created_at,
                occurred_at: row.occurred_at,
                snippet: String::new(),
                score,
                lexical_score: hit.lexical_score.unwrap_or_default(),
                similarity_score: hit.similarity_score.unwrap_or_default(),
            })
        })
        .collect())
}

fn page_hits(
    req: &MemorySearchRequest,
    limit: u32,
    mut results: Vec<MemorySearchResult>,
) -> MemorySearchPage {
    if !uses_hybrid_fusion(req)
        && let Some(floor) = req.min_score
    {
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
        &FlavorScan::new(schemas.to_vec())
            .ok_or_else(|| StorageError::Internal("no render bands on head".into()))?,
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
    let bands = schemas
        .first()
        .and_then(|projection| projection.render_bands)
        .ok_or_else(|| StorageError::Internal("fixture declares no render bands".into()))?;
    substring_sql(schemas, bands, req)
}

/// The substring candidate statement the migrated `search_substring_candidates`
/// function runs, for the plan pins. It binds the function's twelve
/// parameters in order.
///
/// # Errors
///
/// Propagates the builder's; refuses projections that declare no substring leg.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn substring_candidate_sql_for_tests(
    projections: &[MemorySearchProjection],
    recency: bool,
) -> Result<String, StorageError> {
    projection_candidate_statements(projections, true, recency)?
        .ok_or_else(|| StorageError::Internal("no substring leg in the projections".into()))
}

/// The store's RLS admission query for vector-port candidates.
///
/// # Errors
/// Refuses invalid projection identifiers.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn semantic_candidate_admission_sql_for_tests(
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
) -> Result<String, StorageError> {
    semantic_candidate_admission_sql(&core_search_flavors(req, projections), req)
}

#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn search_admit_sql_for_tests(heads_only: bool) -> String {
    search_admit_sql(heads_only)
}

#[cfg(test)]
mod tests {
    use super::{
        EntityKind, MemorySearchRequest, OwnerId, SearchMode, SearchOrder, SupersessionStatus,
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
            prod.contains("search_candidates::call_sql(&flavor.head().projection_table, false)"),
            "the ranked scan must call the fixed candidate function"
        );
        assert!(
            prod.contains("search_candidates::call_sql(&flavor.head().projection_table, true)"),
            "the substring scan must call the fixed candidate function"
        );
        let admit = format!("{}{}", "search_admit_sql(matches!(", "");
        assert!(prod.contains(&admit), "admit must run search_admit_sql");
    }

    /// The core contract's own projected sidecars stay declared alongside
    /// the other flavors an unscoped request can reach.
    #[test]
    fn the_core_contract_declares_its_projected_sidecars() {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        for projection in registry.search_projections().iter().filter(|projection| {
            proxima_core::FLAVOR_0.declares_sidecar_table(&projection.sidecar_table)
        }) {
            assert!(
                matches!(
                    projection.band_comparability,
                    proxima_core::flavor::BandComparability::CoreBands
                ) && projection.rank_source.is_projection(),
                "flavor #0's own schemas must pass the gate they define"
            );
        }
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
    /// The numbering is the mechanical hazard: `$7` is the owner set and
    /// `$8` the participating-schema array, and a shift by one puts each
    /// where the other belongs. This asserts the EMITTED SQL, so a shift
    /// fails here.
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
        let flavor =
            super::FlavorScan::new(vec![&note]).expect("fixture head declares render bands");

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
            ranked.contains("proxima_core.memory_created_at(p.memory_id) >= $4"),
            "$4 is `since` — a LOWER bound"
        );
        assert!(
            ranked.contains("proxima_core.memory_created_at(p.memory_id) <= $5"),
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
        // the composite `gin(owner_id, search_tsv)` needs. The head
        // restriction names `memory` and `memory_head` in an EXISTS, so the
        // property is asserted directly rather than by a blanket string
        // test for `proxima_core.memory`.
        assert!(
            !ranked.contains("m.owner_id"),
            "the ranked arm's owner is on the projection, never reached through memory"
        );
        assert!(
            ranked.contains("FROM proxima_core.projection p, q"),
            "the projection is the driving relation"
        );

        let substring = super::substring_sql_for_tests(&[&note], &req).expect("substring");
        assert!(substring.contains("LIKE $1 ESCAPE"), "$1 is the pattern");
        assert!(substring.contains("LIMIT $2"), "$2 is the overfetch budget");
        assert!(substring.contains("c.tags && $3::text[]"), "$3 is the tags");
        assert!(
            substring.contains("proxima_core.memory_created_at(c.t) >= $4"),
            "$4 is `since` — a LOWER bound"
        );
        assert!(
            substring.contains("proxima_core.memory_created_at(c.t) <= $5"),
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

    /// One `EXISTS` probe per participating flavor, against THAT flavor's
    /// projection table with THAT flavor's schema set, OR'ed — and no
    /// table name reaches the SQL except through `PgIdent`.
    #[test]
    fn the_semantic_arm_probes_each_participating_flavor() {
        let note = proxima_core::FlavorRegistry::new()
            .freeze_or_panic_for_tests()
            .search_projections()
            .iter()
            .find(|projection| projection.schema_id.as_str() == "core/agent-note-v1")
            .expect("core/agent-note-v1 is a search surface")
            .clone();
        let mut req = request_with_tags();

        // Two flavors: one probe each, OR'ed, each with its own schema set.
        let mut docs = note.clone();
        docs.projection_table = "proxima_docs.projection".into();
        let flavors = [
            super::FlavorScan::new(vec![&note]).expect("fixture head declares render bands"),
            super::FlavorScan::new(vec![&docs]).expect("fixture head declares render bands"),
        ];
        let two = super::semantic_candidate_admission_sql(&flavors, &req).expect("semantic");
        let core = two
            .find("FROM proxima_core.projection p")
            .expect("core probe");
        let foreign = two
            .find("FROM proxima_docs.projection p")
            .expect("docs probe");
        assert!(core < foreign, "probes render in `flavors` order");
        assert!(two.contains(") OR EXISTS ("), "one probe per flavor, OR'ed");
        let first = two.find("p.schema_id = ANY($8::text[])").expect("$8");
        let second = two.find("p.schema_id = ANY($9::text[])").expect("$9");
        assert!(
            core < first && first < foreign && foreign < second,
            "$8 and $9 are the schema sets of the first and second flavor, in that order"
        );
        assert!(
            !two.contains("$10"),
            "two flavors bind nine parameters; a tenth is a shift"
        );

        // The table name reaches the SQL only through `PgIdent`.
        let mut spliced = note.clone();
        spliced.projection_table = "proxima_docs.projection p; DROP TABLE x".into();
        let hostile =
            [super::FlavorScan::new(vec![&spliced]).expect("fixture head declares render bands")];
        assert!(
            super::semantic_candidate_admission_sql(&hostile, &req).is_err(),
            "an invalid projection table identifier is refused, not spliced"
        );

        // Without tags the same per-flavor probes remain, with schema sets
        // starting at `$7`.
        req.tags.clear();
        let untagged = super::semantic_candidate_admission_sql(&flavors, &req).expect("semantic");
        assert!(untagged.contains("FROM proxima_core.projection p"));
        assert!(untagged.contains("p.schema_id = ANY($7::text[])"));
        assert!(untagged.contains("FROM proxima_docs.projection p"));
        assert!(untagged.contains("p.schema_id = ANY($8::text[])"));
        assert!(!untagged.contains("p.tag"));
        assert!(super::semantic_candidate_admission_sql(&hostile, &req).is_err());
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
        let flavor =
            super::FlavorScan::new(vec![&note]).expect("fixture head declares render bands");

        let ranked = super::ranked_projection_sql(&flavor, &req, true).expect("ranked");
        assert!(
            ranked.contains("JOIN proxima_core.memory_head hh")
                && ranked.contains("ON hh.handle = hm.handle AND hh.t = hm.t")
                && ranked.contains("WHERE hm.t = p.memory_id"),
            "HeadsOnly must reach the head through the memory row's handle, \
             not spend the window on superseded revisions"
        );
        let substring = super::substring_sql_for_tests(&[&note], &req).expect("substring");
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
        let substring = super::substring_sql_for_tests(&[&note], &req).expect("substring");
        assert!(
            !substring.contains("memory_head"),
            "IncludeSuperseded keeps the unfiltered window"
        );

        req.kind = None;
        let ranked = super::ranked_projection_sql(&flavor, &req, true).expect("ranked");
        assert!(
            !ranked
                .split_whitespace()
                .any(|token| token == "proxima_core.memory"),
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
    /// `AND m.forgotten_at IS NULL` adds a predicate and no `$n`, so the
    /// PREDICATE count is pinned as well.
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
                "since — a `proxima_core.memory_created_at(...) >= $4` row predicate",
            ),
            (
                "$6",
                "until — a `proxima_core.memory_created_at(...) <= $5` row predicate",
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
            owner: OwnerId::from(proxima_core::UserId::new(uuid::Uuid::nil())),
            read_owners: vec![OwnerId::from(proxima_core::UserId::new(uuid::Uuid::nil()))],
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
            semantic: None,
        }
    }

    /// Reachability follows the projection contract, including without tags.
    /// Comparable scores and the supported renderer remain separate gates.
    #[test]
    fn the_merge_admits_only_flavors_that_declare_a_comparable_shape() {
        let frozen = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let note = frozen
            .search_projections()
            .iter()
            .find(|projection| projection.schema_id.as_str() == "core/agent-note-v1")
            .expect("the core note projection")
            .clone();
        let mut foreign = note.clone();
        foreign.schema_id = proxima_core::SchemaId::new("search-notes/note-v1".into());
        foreign.sidecar_table = "search_notes.note_v1".into();
        foreign.projection_table = "search_notes.projection".into();
        foreign.tag_column = None;
        let mut divergent = foreign.clone();
        divergent.band_comparability = proxima_core::flavor::BandComparability::Divergent {
            why: "scores have another scale",
        };
        let mut divergent_core = note.clone();
        divergent_core.band_comparability = divergent.band_comparability;
        let mut unsupported = foreign.clone();
        unsupported.rank_source = proxima_core::flavor::RankSource::SidecarWithProjectionOwner {
            why: "the flavor supplies its own renderer",
        };
        unsupported.render_bands = None;
        let mut request = request_with_tags();
        request.tags.clear();
        let reaches =
            |request: &MemorySearchRequest,
             projection: &proxima_core::verbs::schema::MemorySearchProjection| {
                !super::core_search_flavors(request, std::slice::from_ref(projection)).is_empty()
            };
        for (shape, projection, expected) in [
            ("core", &note, true),
            ("comparable foreign without tags", &foreign, true),
            ("divergent core", &divergent_core, false),
            ("divergent", &divergent, false),
            ("unsupported renderer", &unsupported, false),
        ] {
            assert_eq!(reaches(&request, projection), expected, "{shape}");
        }
        let projections = [note.clone(), foreign.clone()];
        assert_eq!(super::core_search_flavors(&request, &projections).len(), 2);
        request.schema_id = Some(foreign.schema_id.clone());
        assert!(reaches(&request, &foreign));
        request.schema_id = Some(note.schema_id.clone());
        assert!(
            !reaches(&request, &foreign),
            "schema selection still narrows"
        );
        request.schema_id = None;
        request.kind = Some(EntityKind::Abstraction);
        assert!(!reaches(&request, &foreign), "kind selection still narrows");
        request.kind = None;
        assert!(reaches(&request, &foreign));
        request.tags = vec!["bucket-0".into()];
        assert!(
            !reaches(&request, &foreign),
            "a tag filter needs a tag column"
        );
        foreign.tag_column = Some("tags".into());
        assert!(reaches(&request, &foreign));
    }
}
