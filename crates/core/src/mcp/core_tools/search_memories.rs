//! `core/search_memories` — owner-scoped lexical/semantic/hybrid memory search.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;

use crate::engine::SearchReadRequest;
use crate::llm::BoundEmbeddingClient;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use crate::verbs::query::{
    EntityKind, MemorySearchRequest, SearchCursor, SearchMode, SearchOrder, SupersessionStatus,
    TagMatch,
};
use crate::{EmbeddingSpace, McpTool, MemoryId, SchemaId};

use super::memory::search::{NeighborEdge, neighbor_edges_from_rows};
use super::memory_spaces::ResolvedMemorySpace;

const SEMANTIC_SEARCH_UNAVAILABLE: &str =
    "semantic search unavailable: no embedding client is configured for space";
const EMBEDDING_PROVIDER_UNAVAILABLE: &str =
    "semantic search unavailable: embedding provider error";
const DEFAULT_BODY_MAX_CHARS: usize = crate::MAX_TEXT_CAP_CHARS;
/// Each distinct space costs one full storage search, so the space list is
/// this tool's only per-request work multiplier; cap it like every other
/// list argument.
const MAX_SEARCH_SPACES: usize = 16;
/// Matches the write-side cap of 16 distinct tags per memory — a filter
/// larger than any storable tag set cannot narrow anything further.
const MAX_SEARCH_TAGS: usize = 16;

#[derive(Debug, Default)]
pub struct SearchMemoriesTool;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchMemoriesMode {
    #[serde(alias = "Lexical", alias = "LEXICAL")]
    Lexical,
    #[serde(alias = "Semantic", alias = "SEMANTIC")]
    Semantic,
    #[serde(alias = "Hybrid", alias = "HYBRID")]
    Hybrid,
}

impl From<SearchMemoriesMode> for SearchMode {
    fn from(value: SearchMemoriesMode) -> Self {
        match value {
            SearchMemoriesMode::Lexical => Self::Lexical,
            SearchMemoriesMode::Semantic => Self::Semantic,
            SearchMemoriesMode::Hybrid => Self::Hybrid,
        }
    }
}

fn default_mode() -> SearchMemoriesMode {
    SearchMemoriesMode::Hybrid
}

fn default_limit() -> u32 {
    8
}

fn default_include_neighbor_edges() -> bool {
    false
}

fn default_supersession() -> SearchMemoriesSupersession {
    SearchMemoriesSupersession::HeadsOnly
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub enum SearchMemoriesKind {
    #[serde(rename = "Fact", alias = "fact", alias = "FACT")]
    Fact,
    #[serde(rename = "Abstraction", alias = "abstraction", alias = "ABSTRACTION")]
    Abstraction,
    #[serde(rename = "Perspective", alias = "perspective", alias = "PERSPECTIVE")]
    Perspective,
}

impl From<SearchMemoriesKind> for EntityKind {
    fn from(value: SearchMemoriesKind) -> Self {
        match value {
            SearchMemoriesKind::Fact => Self::Fact,
            SearchMemoriesKind::Abstraction => Self::Abstraction,
            SearchMemoriesKind::Perspective => Self::Perspective,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchMemoriesSupersession {
    #[serde(alias = "HeadsOnly", alias = "headsOnly", alias = "HEADS_ONLY")]
    HeadsOnly,
    #[serde(alias = "All", alias = "ALL")]
    All,
}

impl From<SearchMemoriesSupersession> for SupersessionStatus {
    fn from(value: SearchMemoriesSupersession) -> Self {
        match value {
            SearchMemoriesSupersession::HeadsOnly => Self::HeadsOnly,
            SearchMemoriesSupersession::All => Self::IncludeSuperseded,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchMemoriesArgs {
    #[schemars(
        length(max = 512),
        description = "Search query over owner-visible memories. 1 to 512 chars."
    )]
    pub query: String,
    #[serde(default = "default_mode")]
    #[schemars(description = "Search mode: lexical, semantic, or hybrid. Defaults to hybrid.")]
    pub mode: SearchMemoriesMode,
    #[serde(default = "default_limit")]
    #[schemars(
        range(min = 1),
        description = "Maximum number of memories to return. Defaults to 8; values above 50 are clamped, and 0 is rejected."
    )]
    pub limit: u32,
    #[serde(default = "default_supersession")]
    #[schemars(
        description = "Supersession filter: heads_only returns only current heads by default; all includes superseded history."
    )]
    pub supersession: SearchMemoriesSupersession,
    #[serde(default)]
    #[schemars(
        description = "Optional memory kind filter: Fact, Abstraction, or Perspective. Omit or null for all kinds."
    )]
    pub kind: Option<SearchMemoriesKind>,
    #[serde(default)]
    #[schemars(description = "Optional schema_id filter. Omit or null for all schemas.")]
    pub schema_id: Option<String>,
    #[serde(default)]
    #[schemars(
        length(max = 16),
        description = "Optional tag filter, at most 16 tags. Tags are matched in stored form — trimmed and lowercased — so `Rust` and `rust` filter alike. Empty means no tag filter."
    )]
    pub tags: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Tag filter mode: any or all. Defaults to any.")]
    pub tag_match: TagMatch,
    #[serde(default)]
    #[schemars(description = "Optional inclusive lower created_at bound as an RFC3339 timestamp.")]
    pub since: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional inclusive upper created_at bound as an RFC3339 timestamp.")]
    pub until: Option<String>,
    #[serde(default)]
    #[schemars(description = "Result ordering: relevance or recency. Defaults to relevance.")]
    pub order: SearchOrder,
    #[serde(default)]
    #[schemars(
        description = "Minimum fused relevance score in 0..=1; results scoring below it are dropped. Omit or null for no floor."
    )]
    pub min_score: Option<f32>,
    #[serde(default)]
    #[schemars(
        description = "Hybrid fusion weight on the semantic component in 0..=1; the lexical component gets the complement. Defaults to 0.6. Only valid with mode=hybrid."
    )]
    pub semantic_weight: Option<f32>,
    #[serde(default = "default_include_neighbor_edges")]
    #[schemars(
        description = "Include neighbor edges touching matched memories. Defaults to false."
    )]
    pub include_neighbor_edges: bool,
    #[serde(default)]
    #[schemars(description = "Include hydrated body text in each result. Defaults to false.")]
    pub include_body: bool,
    #[serde(default)]
    #[schemars(
        range(min = 1),
        description = "Optional max character count for hydrated body text, at least 1; values above 8000 are clamped to 8000 (also the default). Applies only when include_body=true. When a body is cut to this cap the result carries body_truncated=true; fetch the full text via proxima://memory/{id}."
    )]
    pub body_max_chars: Option<usize>,
    #[serde(default)]
    #[schemars(
        length(max = 16),
        description = "Memory space keys from core_memory_spaces, at most 16. Empty/omitted searches current owner."
    )]
    pub spaces: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Opaque pagination cursor from a previous response's next_cursor. Returns the page after it. Query, mode, filters, order, spaces, and score knobs must stay unchanged; limit, include_body, and include_neighbor_edges may vary."
    )]
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchMemoriesOutput {
    pub mode: String,
    pub degraded_to_lexical: bool,
    /// `score`: every result's `score` is on one scale. `rank`: the
    /// searched spaces were scored on different scales — Owners embedded
    /// by different models, or some searched lexically — so a `score`
    /// compares only within its `space`, and relevance order interleaves
    /// the spaces by rank.
    pub ranking: SearchRanking,
    pub memories: Vec<SearchMemoryOutput>,
    pub neighbor_edges: Vec<NeighborEdge>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchRanking {
    Score,
    Rank,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchMemoryOutput {
    /// The stable UUID of this Memory admission. The typed `memory` handle
    /// remains the generic wire reference used by other MCP surfaces.
    pub memory_id: uuid::Uuid,
    pub memory: String,
    pub space: String,
    pub kind: String,
    pub schema_id: String,
    pub created_at: String,
    pub snippet: String,
    pub score: f32,
    pub lexical_score: f32,
    pub similarity_score: f32,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// `Some(true)` when the hydrated `body` was cut to `body_max_chars`;
    /// the untruncated text is available via `proxima://memory/{id}`.
    /// Present only when a body was hydrated (`include_body=true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_truncated: Option<bool>,
}

impl McpTool for SearchMemoriesTool {
    const NAME: &'static str = protocol_tool::CORE_SEARCH_MEMORIES;
    const DESCRIPTION: &'static str = "Search owner-scoped memories by lexical, semantic, or hybrid ranking. Defaults to current heads only; pass supersession=all for full history. Set include_body=true to hydrate body text in the same batched read; a body cut to body_max_chars is flagged with body_truncated=true. Drop weak hits with min_score, tune hybrid fusion with semantic_weight, and page past the 50-result cap by passing next_cursor back as cursor.";
    type Args = SearchMemoriesArgs;
    type Output = SearchMemoriesOutput;

    fn call(
        ctx: McpToolCtx,
        mut args: SearchMemoriesArgs,
    ) -> BoxFuture<'static, Result<SearchMemoriesOutput, McpToolError>> {
        Box::pin(async move {
            let query = crate::validate_search_query(&args.query)?;
            validate_score_args(&args)?;
            validate_body_max_chars(args.body_max_chars)?;
            crate::reject_zero_limit(Some(args.limit))?;
            validate_list_caps(&args)?;
            // Fold the filter the way the write side folds a stored tag, and
            // before anything reads `args.tags`: both the storage request and
            // the cursor fingerprint consume it, and a cursor must rebind to
            // the same query whichever spelling the caller sent.
            fold_tag_filter(&mut args.tags);

            let mode = SearchMode::from(args.mode);
            let since = parse_rfc3339(args.since.as_deref(), "since")?;
            let until = parse_rfc3339(args.until.as_deref(), "until")?;
            let spaces = resolve_search_spaces(&ctx, &args.spaces)?;
            let lanes = plan_owner_lanes(&ctx, mode, query, spaces).await?;
            let mut degraded_to_lexical = lanes
                .iter()
                .any(|lane| matches!(lane.arm, LaneArm::Degraded));
            let ranking = ranking_for(&lanes);
            // The fingerprint binds a cursor to everything that shapes the
            // result set, so page N+1 provably continues the same query —
            // including each Owner's scoring lane, so a page that would be
            // ranked differently (a route moved, a provider went down)
            // rejects the cursor instead of skipping or repeating rows.
            let fingerprint = query_fingerprint(query, &args, since, until, &lanes);
            let after = args
                .cursor
                .as_deref()
                .map(|raw| decode_cursor(raw, &fingerprint))
                .transpose()?;
            let merge = MergeKind::of(ranking, args.order);
            let positions = cursor_positions(after.as_ref(), merge, &lanes)?;
            let prepared = PreparedSearch {
                query: query.to_string(),
                since,
                until,
                body_max_chars: effective_body_max_chars(args.body_max_chars),
                limit: args.limit.min(50),
            };
            let mut lists = Vec::with_capacity(lanes.len());
            let mut all_neighbor_edges = Vec::new();
            let mut any_space_has_more = false;
            for (lane, position) in lanes.into_iter().zip(positions) {
                let result =
                    search_one_space(&ctx, &args, &prepared, &lane, position.after).await?;
                degraded_to_lexical |= result.degraded_to_lexical;
                any_space_has_more |= result.has_more;
                all_neighbor_edges.extend(result.neighbor_edges);
                lists.push(RankedList {
                    owner: lane.space.owner,
                    position,
                    rows: result.memories,
                });
            }
            let (memories, has_more, next_cursor) = match merge {
                MergeKind::Keyset => paginate_merged_outputs(
                    lists.into_iter().flat_map(|list| list.rows).collect(),
                    args.order,
                    prepared.limit as usize,
                    any_space_has_more,
                    after.as_ref().and_then(PageCursor::keyset),
                    &fingerprint,
                ),
                MergeKind::Rank => paginate_rank_fused(
                    lists,
                    prepared.limit as usize,
                    any_space_has_more,
                    &fingerprint,
                ),
            };
            retain_surviving_neighbor_edges(&memories, &mut all_neighbor_edges);

            Ok(SearchMemoriesOutput {
                mode: format!("{mode:?}").to_lowercase(),
                degraded_to_lexical,
                ranking,
                memories,
                neighbor_edges: all_neighbor_edges,
                next_cursor,
                has_more,
            })
        })
    }
}

struct PreparedSearch {
    query: String,
    since: Option<time::OffsetDateTime>,
    until: Option<time::OffsetDateTime>,
    body_max_chars: usize,
    limit: u32,
}

/// One searched Owner and how its list is scored.
struct OwnerLane {
    space: ResolvedMemorySpace,
    arm: LaneArm,
}

/// The mode an Owner's search runs in, with the query embedded in that
/// Owner's space whenever the mode reads vectors.
enum LaneArm {
    Lexical,
    /// Hybrid was requested and this Owner runs lexically: no route, or its
    /// provider failed.
    Degraded,
    Semantic(crate::SpaceVector),
    Hybrid(crate::SpaceVector),
}

impl LaneArm {
    const fn mode(&self) -> SearchMode {
        match self {
            Self::Lexical | Self::Degraded => SearchMode::Lexical,
            Self::Semantic(_) => SearchMode::Semantic,
            Self::Hybrid(_) => SearchMode::Hybrid,
        }
    }

    const fn semantic(&self) -> Option<&crate::SpaceVector> {
        match self {
            Self::Lexical | Self::Degraded => None,
            Self::Semantic(query) | Self::Hybrid(query) => Some(query),
        }
    }
}

impl OwnerLane {
    /// The scale this list's scores are on; lists with equal keys merge by
    /// score.
    fn scoring_key(&self) -> Option<&EmbeddingSpace> {
        self.arm.semantic().map(crate::SpaceVector::space)
    }
}

/// Route every searched Owner and embed the query once per distinct
/// client. Each Owner's query goes only to its own route's endpoint.
///
/// A pure Semantic request fails when any Owner cannot be served — there
/// is no lexical fallback to degrade to. Hybrid degrades just that Owner.
async fn plan_owner_lanes(
    ctx: &McpToolCtx,
    requested: SearchMode,
    query: &str,
    spaces: Vec<ResolvedMemorySpace>,
) -> Result<Vec<OwnerLane>, McpToolError> {
    if matches!(requested, SearchMode::Lexical) {
        return Ok(spaces
            .into_iter()
            .map(|space| OwnerLane {
                space,
                arm: LaneArm::Lexical,
            })
            .collect());
    }
    let engine = ctx.require_engine()?;
    let mut embedded: Vec<(BoundEmbeddingClient, Result<crate::SpaceVector, String>)> = Vec::new();
    let mut lanes = Vec::with_capacity(spaces.len());
    for space in spaces {
        let route = engine.search_route(&space.owner).await;
        let semantic = match route.current_client() {
            None => Err(format!("{SEMANTIC_SEARCH_UNAVAILABLE} {}", space.key)),
            Some(client) => {
                let cached = embedded
                    .iter()
                    .find(|(seen, _)| seen.same_client(client) && seen.space() == client.space());
                if let Some((_, result)) = cached {
                    result.clone()
                } else {
                    let result = embed_query_for_search(client, query).await;
                    embedded.push((client.clone(), result.clone()));
                    result
                }
            }
        };
        let arm = match (semantic, requested) {
            (Ok(semantic), SearchMode::Hybrid) => LaneArm::Hybrid(semantic),
            (Ok(semantic), _) => LaneArm::Semantic(semantic),
            (Err(err), SearchMode::Hybrid) => {
                tracing::warn!(
                    space = %space.key,
                    error = %err,
                    "hybrid search degrading the space to lexical",
                );
                LaneArm::Degraded
            }
            (Err(err), _) => return Err(McpToolError::Unavailable(err)),
        };
        lanes.push(OwnerLane { space, arm });
    }
    Ok(lanes)
}

fn ranking_for(lanes: &[OwnerLane]) -> SearchRanking {
    let mut keys = lanes.iter().map(OwnerLane::scoring_key);
    let first = keys.next();
    if keys.all(|key| Some(key) == first) {
        SearchRanking::Score
    } else {
        SearchRanking::Rank
    }
}

/// How the per-Owner lists become one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeKind {
    /// One sort key across every list, one shared keyset cursor.
    Keyset,
    /// Interleave by per-list rank; one keyset per list.
    Rank,
}

impl MergeKind {
    /// Recency is comparable across any lanes; relevance only on one scale.
    fn of(ranking: SearchRanking, order: SearchOrder) -> Self {
        match (ranking, order) {
            (SearchRanking::Rank, SearchOrder::Relevance) => Self::Rank,
            _ => Self::Keyset,
        }
    }
}

/// Where one Owner's list resumes: its own keyset and how many of its rows
/// earlier pages emitted, which is the rank its next row continues from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ListPosition {
    after: Option<SearchCursor>,
    consumed: u32,
}

fn cursor_positions(
    after: Option<&PageCursor>,
    merge: MergeKind,
    lanes: &[OwnerLane],
) -> Result<Vec<ListPosition>, McpToolError> {
    match (after, merge) {
        (None, _) => Ok(vec![ListPosition::default(); lanes.len()]),
        // A keyset is a per-row predicate, so every list takes the same one
        // and the re-merge is exactly the next page of the merged order.
        (Some(PageCursor::Keyset(cursor)), MergeKind::Keyset) => Ok(vec![
            ListPosition {
                after: Some(*cursor),
                consumed: 0,
            };
            lanes.len()
        ]),
        (Some(PageCursor::Ranked(ranked)), MergeKind::Rank) => Ok(lanes
            .iter()
            .map(|lane| {
                let owner = lane.space.owner.external_key();
                ranked
                    .lists
                    .iter()
                    .find(|list| list.owner == owner)
                    .map_or_else(ListPosition::default, |list| ListPosition {
                        after: Some(list.after),
                        consumed: list.consumed,
                    })
            })
            .collect()),
        _ => Err(McpToolError::InvalidInput(
            "malformed cursor: pass next_cursor from a previous core_search_memories response"
                .into(),
        )),
    }
}

/// One Owner's rows in storage order, with where that list stood.
struct RankedList {
    owner: crate::OwnerRef,
    position: ListPosition,
    rows: Vec<RankedMemoryOutput>,
}

/// A wire output paired with the typed sort/cursor keys it was ranked
/// by. The wire `memory` field is a prefixed id whose prefix would
/// distort cross-kind uuid tiebreaks, and `created_at` is a formatted
/// string — the merge across spaces must sort on the raw values that
/// storage sorted on.
struct RankedMemoryOutput {
    memory_id: uuid::Uuid,
    created_at: time::OffsetDateTime,
    output: SearchMemoryOutput,
}

struct SpaceSearchResult {
    degraded_to_lexical: bool,
    has_more: bool,
    memories: Vec<RankedMemoryOutput>,
    neighbor_edges: Vec<NeighborEdge>,
}

fn validate_score_args(args: &SearchMemoriesArgs) -> Result<(), McpToolError> {
    if let Some(floor) = args.min_score
        && !(floor.is_finite() && (0.0..=1.0).contains(&floor))
    {
        return Err(McpToolError::InvalidInput(
            "min_score must be within 0.0..=1.0".into(),
        ));
    }
    if let Some(weight) = args.semantic_weight {
        if !(weight.is_finite() && (0.0..=1.0).contains(&weight)) {
            return Err(McpToolError::InvalidInput(
                "semantic_weight must be within 0.0..=1.0".into(),
            ));
        }
        if !matches!(args.mode, SearchMemoriesMode::Hybrid) {
            return Err(McpToolError::InvalidInput(
                "semantic_weight applies only to mode=hybrid".into(),
            ));
        }
    }
    Ok(())
}

fn resolve_search_spaces(
    ctx: &McpToolCtx,
    raw_spaces: &[String],
) -> Result<Vec<super::memory_spaces::ResolvedMemorySpace>, McpToolError> {
    if raw_spaces.is_empty() {
        return Ok(vec![super::memory_spaces::resolve_space_owner(
            ctx,
            None,
            super::memory_spaces::SpaceDefault::Current,
        )?]);
    }
    // Dedup by the *resolved* owner, not the raw key: `current` and the
    // explicit `personal:<uuid>` spelling of the caller's own space both
    // name the same owner, and searching it twice would return every hit
    // (and its neighbor edges) twice in the merged page.
    let mut seen = std::collections::HashSet::with_capacity(raw_spaces.len());
    let mut out = Vec::with_capacity(raw_spaces.len());
    for key in raw_spaces {
        let resolved = super::memory_spaces::resolve_space_owner(
            ctx,
            Some(key.as_str()),
            super::memory_spaces::SpaceDefault::Current,
        )?;
        if seen.insert(resolved.owner) {
            out.push(resolved);
        }
    }
    Ok(out)
}

async fn search_one_space(
    ctx: &McpToolCtx,
    args: &SearchMemoriesArgs,
    prepared: &PreparedSearch,
    lane: &OwnerLane,
    after: Option<SearchCursor>,
) -> Result<SpaceSearchResult, McpToolError> {
    let space = &lane.space;
    let mode = lane.arm.mode();
    let req = MemorySearchRequest {
        owner: space.owner,
        read_owners: Vec::new(),
        query: prepared.query.clone(),
        mode,
        supersession: args.supersession.into(),
        limit: prepared.limit,
        kind: args.kind.map(EntityKind::from),
        schema_id: args.schema_id.clone().map(SchemaId::new),
        tags: args.tags.clone(),
        tag_match: args.tag_match,
        since: prepared.since,
        until: prepared.until,
        order: args.order,
        min_score: args.min_score,
        // The caller's weight, dropped when this lane degraded away from
        // hybrid. Degradation is an availability fact, not a caller error,
        // so the knob the run can no longer honor is discarded here rather
        // than carried into a verb that rejects it.
        semantic_weight: weight_for_effective_mode(args.semantic_weight, mode),
        after,
        semantic: lane.arm.semantic().cloned(),
    };
    let engine = ctx.require_engine()?;
    let response = engine
        .search(
            &ctx.authz,
            &SearchReadRequest {
                search: req,
                include_body: args.include_body,
                include_neighbor_edges: args.include_neighbor_edges,
            },
        )
        .await?;
    let rows = response.memories;
    let degraded_to_lexical = semantic_search_degraded_to_lexical(mode, &rows);
    let payloads = response
        .payloads
        .into_iter()
        .map(|payload| (payload.memory_id.into_inner(), payload))
        .collect::<std::collections::BTreeMap<_, _>>();
    let neighbor_edges = neighbor_edges_from_rows(ctx, response.neighbor_edges);
    let memories = rows
        .into_iter()
        .map(|row| {
            let mid = row.memory_id.into_inner();
            let created_at = row.created_at;
            let tags = payloads
                .get(&mid)
                .and_then(|payload| payload.tags.clone())
                .unwrap_or_default();
            let hydrated = args
                .include_body
                .then(|| {
                    payloads
                        .get(&mid)
                        .and_then(|payload| payload.body.clone())
                        .map(|body| truncate_body(&body, prepared.body_max_chars))
                })
                .flatten();
            let (body, body_truncated) = match hydrated {
                Some((text, truncated)) => (Some(text), Some(truncated)),
                None => (None, None),
            };
            search_memory_output(ctx, &space.key, row, tags, body, body_truncated).map(|output| {
                RankedMemoryOutput {
                    memory_id: mid,
                    created_at,
                    output,
                }
            })
        })
        .collect::<Result<Vec<_>, McpToolError>>()?;
    Ok(SpaceSearchResult {
        degraded_to_lexical,
        has_more: response.has_more,
        memories,
        neighbor_edges,
    })
}

/// Sort the merged cross-space rows, cut the page, and mint the next
/// opaque cursor from the last emitted row. `seen` accumulates across
/// pages so storage can widen its relevance overfetch window.
fn paginate_merged_outputs(
    mut all_memories: Vec<RankedMemoryOutput>,
    order: SearchOrder,
    page_len: usize,
    any_space_has_more: bool,
    after: Option<SearchCursor>,
    fingerprint: &str,
) -> (Vec<SearchMemoryOutput>, bool, Option<String>) {
    sort_ranked_outputs(&mut all_memories, order);
    let has_more = any_space_has_more || all_memories.len() > page_len;
    all_memories.truncate(page_len);
    let next_cursor = (has_more && !all_memories.is_empty()).then(|| {
        let last = all_memories.last().expect("non-empty page");
        let seen = after
            .map_or(0, |cursor| cursor.seen())
            .saturating_add(u32::try_from(all_memories.len()).unwrap_or(u32::MAX));
        let cursor = match order {
            SearchOrder::Relevance => SearchCursor::Relevance {
                score_bits: last.output.score.to_bits(),
                memory_id: MemoryId::new(last.memory_id),
                seen,
            },
            SearchOrder::Recency => SearchCursor::Recency {
                created_at: last.created_at,
                memory_id: MemoryId::new(last.memory_id),
                seen,
            },
        };
        encode_cursor(&PageCursor::Keyset(cursor), fingerprint)
    });
    let memories = all_memories
        .into_iter()
        .map(|ranked| ranked.output)
        .collect();
    (memories, has_more, next_cursor)
}

/// Reciprocal-rank fusion over lists whose scores are on different scales.
///
/// Every memory sits in exactly one Owner's list, so its fused score
/// `1 / (k + rank)` orders exactly as its rank does, for any `k`: fusion is
/// interleaving the lists by rank, ties to the higher `memory_id`.
///
/// A row's rank is its list's `consumed` plus its place in this fetch, so
/// the merged order is `(rank asc, memory_id desc)` across all pages. Each
/// list's emitted rows are a prefix of that list, so the next page resumes
/// every list from its own last emitted row.
fn paginate_rank_fused(
    lists: Vec<RankedList>,
    page_len: usize,
    any_space_has_more: bool,
    fingerprint: &str,
) -> (Vec<SearchMemoryOutput>, bool, Option<String>) {
    let mut ranked: Vec<(u32, usize, RankedMemoryOutput)> = Vec::new();
    let mut positions: Vec<(crate::OwnerRef, ListPosition)> = Vec::with_capacity(lists.len());
    for (index, list) in lists.into_iter().enumerate() {
        let mut rank = list.position.consumed;
        for row in list.rows {
            rank = rank.saturating_add(1);
            ranked.push((rank, index, row));
        }
        positions.push((list.owner, list.position));
    }
    ranked.sort_by(|(a_rank, _, a), (b_rank, _, b)| {
        a_rank
            .cmp(b_rank)
            .then_with(|| b.memory_id.cmp(&a.memory_id))
    });
    let has_more = any_space_has_more || ranked.len() > page_len;
    ranked.truncate(page_len);
    let next_cursor = (has_more && !ranked.is_empty()).then(|| {
        for (_, index, row) in &ranked {
            let position = &mut positions[*index].1;
            position.consumed = position.consumed.saturating_add(1);
            position.after = Some(SearchCursor::Relevance {
                score_bits: row.output.score.to_bits(),
                memory_id: MemoryId::new(row.memory_id),
                seen: position.consumed,
            });
        }
        let lists = positions
            .iter()
            .filter_map(|(owner, position)| {
                position.after.map(|after| RankedListCursor {
                    owner: owner.external_key(),
                    after,
                    consumed: position.consumed,
                })
            })
            .collect();
        encode_cursor(&PageCursor::Ranked(RankedCursor { lists }), fingerprint)
    });
    let memories = ranked.into_iter().map(|(_, _, row)| row.output).collect();
    (memories, has_more, next_cursor)
}

/// Merged cross-space sort with the exact comparator storage pages by:
/// `(score desc, memory_id desc)` / `(created_at desc, memory_id desc)`.
/// A cursor built from the last row of this order resumes correctly in
/// every space at once.
fn sort_ranked_outputs(memories: &mut [RankedMemoryOutput], order: SearchOrder) {
    match order {
        SearchOrder::Relevance => memories.sort_by(|a, b| {
            b.output
                .score
                .total_cmp(&a.output.score)
                .then_with(|| b.memory_id.cmp(&a.memory_id))
        }),
        SearchOrder::Recency => memories.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.memory_id.cmp(&a.memory_id))
        }),
    }
}

/// The resume point under the cursor envelope: one shared keyset when every
/// list is on one scale, else one keyset per Owner list. Untagged so a
/// keyset cursor keeps the wire shape it always had.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum PageCursor {
    Keyset(SearchCursor),
    Ranked(RankedCursor),
}

impl PageCursor {
    const fn keyset(&self) -> Option<SearchCursor> {
        match self {
            Self::Keyset(cursor) => Some(*cursor),
            Self::Ranked(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RankedCursor {
    lists: Vec<RankedListCursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RankedListCursor {
    owner: String,
    after: SearchCursor,
    consumed: u32,
}

/// Opaque cursor codec: the shared `{v, fp, c}` envelope with the typed
/// resume point under `c`. Clients must treat the token as opaque; the
/// fingerprint rejects replay against a different query shape, and `v`
/// gates format evolution.
const SEARCH_CURSOR: crate::mcp::cursor::FingerprintedCursor =
    crate::mcp::cursor::FingerprintedCursor {
        version: 1,
        source: "core_search_memories response",
        rebind_hint: "repeat the query, mode, filters, order, and spaces that produced it",
    };

fn encode_cursor(cursor: &PageCursor, fingerprint: &str) -> String {
    SEARCH_CURSOR.encode(fingerprint, cursor)
}

fn decode_cursor(raw: &str, fingerprint: &str) -> Result<PageCursor, McpToolError> {
    SEARCH_CURSOR.decode(fingerprint, raw).map_err(Into::into)
}

/// Canonical fingerprint over everything that shapes the result set or
/// its order. Page size (`limit`) and presentation flags (bodies,
/// neighbor edges) stay out so they may vary between pages. Tags and
/// resolved space owners are sorted first, making equivalent filter
/// sets fingerprint identically. Each Owner carries its scoring lane —
/// the embedding space its arm searched, or none when it ran lexically.
fn query_fingerprint(
    query: &str,
    args: &SearchMemoriesArgs,
    since: Option<time::OffsetDateTime>,
    until: Option<time::OffsetDateTime>,
    lanes: &[OwnerLane],
) -> String {
    let mut tags = args.tags.clone();
    tags.sort_unstable();
    let mut space_keys: Vec<(String, Option<String>)> = lanes
        .iter()
        .map(|lane| {
            (
                lane.space.owner.external_key(),
                lane.scoring_key().map(ToString::to_string),
            )
        })
        .collect();
    space_keys.sort_unstable();
    let canon = serde_json::to_string(&(
        query,
        SearchMode::from(args.mode),
        SupersessionStatus::from(args.supersession),
        args.kind.map(|kind| EntityKind::from(kind).as_str()),
        args.schema_id.as_deref(),
        &tags,
        args.tag_match,
        since.map(time::OffsetDateTime::unix_timestamp_nanos),
        until.map(time::OffsetDateTime::unix_timestamp_nanos),
        args.order,
        args.min_score.map(f32::to_bits),
        args.semantic_weight.map(f32::to_bits),
        &space_keys,
    ))
    .expect("fingerprint canon serializes");
    crate::mcp::cursor::fingerprint(&canon)
}

/// Cut `body` to at most `max_chars` characters, returning the (possibly
/// shortened) text and whether anything was dropped. The truncated string
/// is a char-boundary prefix, so comparing byte lengths is an O(1) exact
/// "did we cut" test.
fn truncate_body(body: &str, max_chars: usize) -> (String, bool) {
    let truncated: String = body.chars().take(max_chars).collect();
    let did_truncate = truncated.len() < body.len();
    (truncated, did_truncate)
}

/// Reject `body_max_chars: 0` explicitly. Letting it through would hydrate
/// every body to `""` with `body_truncated=true` — a well-formed page that
/// carries no text, which reads as data loss rather than the caller's own
/// cap. A caller who wants no bodies has `include_body=false` for that.
fn validate_body_max_chars(requested: Option<usize>) -> Result<(), McpToolError> {
    if requested == Some(0) {
        return Err(McpToolError::InvalidInput(
            "body_max_chars must be >= 1 when provided; use include_body=false to skip bodies"
                .into(),
        ));
    }
    Ok(())
}

/// Cap the raw list arguments before any per-item work. `spaces` and
/// `tags` were the only uncapped list inputs on the tool surface; without
/// a cap a single request could fan out into one storage search per
/// listed space.
/// Fold every filter tag to its stored form, then sort and dedup so
/// `["Rust", "rust"]` is one predicate rather than two.
///
/// Blank tags are dropped rather than rejected: the write side refuses to
/// store one, so a blank filter entry can never match, and silently ignoring
/// it keeps an empty-string element from turning a good filter into a
/// guaranteed-empty result.
fn fold_tag_filter(tags: &mut Vec<String>) {
    for tag in tags.iter_mut() {
        *tag = super::memory::util::fold_tag(tag);
    }
    tags.retain(|tag| !tag.is_empty());
    tags.sort_unstable();
    tags.dedup();
}

fn validate_list_caps(args: &SearchMemoriesArgs) -> Result<(), McpToolError> {
    if args.spaces.len() > MAX_SEARCH_SPACES {
        return Err(McpToolError::InvalidInput(format!(
            "at most {MAX_SEARCH_SPACES} spaces per search"
        )));
    }
    if args.tags.len() > MAX_SEARCH_TAGS {
        return Err(McpToolError::InvalidInput(format!(
            "at most {MAX_SEARCH_TAGS} tags in the tags filter"
        )));
    }
    Ok(())
}

fn effective_body_max_chars(requested: Option<usize>) -> usize {
    requested.map_or(DEFAULT_BODY_MAX_CHARS, |max| {
        max.min(DEFAULT_BODY_MAX_CHARS)
    })
}

fn search_memory_output(
    ctx: &McpToolCtx,
    space: &str,
    row: crate::verbs::query::MemorySearchResult,
    tags: Vec<String>,
    body: Option<String>,
    body_truncated: Option<bool>,
) -> Result<SearchMemoryOutput, McpToolError> {
    let class = super::get_memory::memory_class(row.kind)?;
    Ok(SearchMemoryOutput {
        memory_id: row.memory_id.into_inner(),
        memory: ctx.format_memory_with_class(row.memory_id, class),
        space: space.to_string(),
        kind: row.kind.as_str().to_string(),
        schema_id: row.schema_id.as_str().to_string(),
        created_at: format_rfc3339(row.created_at)?,
        snippet: row.snippet,
        score: row.score,
        lexical_score: row.lexical_score,
        similarity_score: row.similarity_score,
        tags,
        body,
        body_truncated,
    })
}

fn semantic_search_degraded_to_lexical(
    mode: SearchMode,
    rows: &[crate::verbs::query::MemorySearchResult],
) -> bool {
    degraded_to_lexical(
        mode,
        rows.is_empty(),
        rows.iter().any(|row| row.similarity_score > 0.0),
    )
}

/// The fusion weight to hand the search verb, given the mode the run can
/// actually serve.
///
/// A caller who pairs a weight with a non-hybrid mode is rejected up front
/// by [`validate_score_args`]. Degradation is the other case and not a
/// caller error: a hybrid request on a deployment with no embeddings ranks
/// lexically, and there is no semantic component left to weight. Dropping
/// the knob here is what keeps that request servable, since the verb
/// rejects a weight the mode cannot use.
fn weight_for_effective_mode(requested: Option<f32>, effective: SearchMode) -> Option<f32> {
    requested.filter(|_| matches!(effective, SearchMode::Hybrid))
}

/// Embed the query in `embed`'s space, mapping provider failure to a
/// caller-actionable message. The caller decides whether that message
/// hard-fails (pure Semantic) or degrades the Owner to lexical (Hybrid).
async fn embed_query_for_search(
    embed: &BoundEmbeddingClient,
    query: &str,
) -> Result<crate::SpaceVector, String> {
    embed.embed_vector(query).await.map_err(|err| {
        tracing::warn!(error = %err, "embedding provider failed");
        EMBEDDING_PROVIDER_UNAVAILABLE.to_string()
    })
}

/// Drop neighbor edges that no longer touch a surviving (post-truncation)
/// memory, and dedupe by the edge's own content. Per-space searches
/// over-fetch edges against their own candidate sets; after the merged
/// set is sorted and truncated, edges to hits that were truncated out are
/// dangling references.
///
/// The dedupe key is `(source, target, kind)` because that IS the edge —
/// there is no handle to key on, and none is needed.
fn retain_surviving_neighbor_edges(memories: &[SearchMemoryOutput], edges: &mut Vec<NeighborEdge>) {
    let surviving: std::collections::HashSet<&str> = memories
        .iter()
        .map(|memory| memory.memory.as_str())
        .collect();
    let mut seen_edges = std::collections::HashSet::new();
    edges.retain(|edge| {
        let touches =
            surviving.contains(edge.source.as_str()) || surviving.contains(edge.target.as_str());
        // `&&` short-circuits: a non-touching edge is never marked seen, so a
        // later touching duplicate is still evaluated on its own merits.
        touches && seen_edges.insert((edge.source.clone(), edge.target.clone(), edge.kind.clone()))
    });
}

fn degraded_to_lexical(mode: SearchMode, no_rows: bool, any_semantic_score: bool) -> bool {
    crate::verbs::query::hybrid_degraded_to_lexical(mode, no_rows, any_semantic_score)
}

fn parse_rfc3339(
    raw: Option<&str>,
    field: &str,
) -> Result<Option<time::OffsetDateTime>, McpToolError> {
    raw.map(|value| {
        time::OffsetDateTime::parse(value, &Rfc3339).map_err(|err| {
            McpToolError::InvalidInput(format!("{field} must be an RFC3339 timestamp: {err}"))
        })
    })
    .transpose()
}

fn format_rfc3339(value: time::OffsetDateTime) -> Result<String, McpToolError> {
    value
        .format(&Rfc3339)
        .map_err(|err| McpToolError::Other(format!("format created_at: {err}")))
}

#[cfg(test)]
mod tests;
