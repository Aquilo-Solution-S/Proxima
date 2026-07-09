//! `core/search_memories` — owner-scoped lexical/semantic/hybrid memory search.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;

use crate::engine::SearchReadRequest;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use crate::verbs::query::{
    EntityKind, MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use crate::{McpTool, SchemaId};

use super::memory::search::{NeighborEdge, neighbor_edges_from_rows};

const SEMANTIC_SEARCH_UNAVAILABLE: &str =
    "semantic search unavailable: no embedding client is configured for this host";
const DEFAULT_BODY_MAX_CHARS: usize = 8_000;

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
    true
}

fn default_supersession() -> SearchMemoriesSupersession {
    SearchMemoriesSupersession::HeadsOnly
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub enum SearchMemoriesKind {
    #[serde(rename = "Fact", alias = "fact")]
    Fact,
    #[serde(rename = "Abstraction", alias = "abstraction")]
    Abstraction,
    #[serde(rename = "Perspective", alias = "perspective")]
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
    #[schemars(description = "Search query over owner-visible memories. 1 to 512 chars.")]
    pub query: String,
    #[serde(default = "default_mode")]
    #[schemars(description = "Search mode: lexical, semantic, or hybrid. Defaults to hybrid.")]
    pub mode: SearchMemoriesMode,
    #[serde(default = "default_limit")]
    #[schemars(
        description = "Maximum number of memories to return. Defaults to 8; values are clamped to 1..=50."
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
    #[schemars(description = "Optional exact tag filter. Empty means no tag filter.")]
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
    #[serde(default = "default_include_neighbor_edges")]
    #[schemars(
        description = "Include neighbor edges touching matched memories. Defaults to true; set false for lean results."
    )]
    pub include_neighbor_edges: bool,
    #[serde(default)]
    #[schemars(description = "Include hydrated body text in each result. Defaults to false.")]
    pub include_body: bool,
    #[serde(default)]
    #[schemars(
        description = "Optional max character count for hydrated body text. Applies only when include_body=true."
    )]
    pub body_max_chars: Option<usize>,
    #[serde(default)]
    #[schemars(
        description = "Memory space keys from core_memory_spaces. Empty/omitted searches current owner."
    )]
    pub spaces: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchMemoriesOutput {
    pub mode: String,
    pub degraded_to_lexical: bool,
    pub memories: Vec<SearchMemoryOutput>,
    pub neighbor_edges: Vec<NeighborEdge>,
}

#[derive(Debug, Serialize)]
pub struct SearchMemoryOutput {
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
}

impl McpTool for SearchMemoriesTool {
    const NAME: &'static str = protocol_tool::CORE_SEARCH_MEMORIES;
    const DESCRIPTION: &'static str = "Search owner-scoped memories by lexical, semantic, or hybrid ranking. Defaults to current heads only; pass supersession=all for full history. Set include_body=true to hydrate body text in the same batched read.";
    type Args = SearchMemoriesArgs;
    type Output = SearchMemoriesOutput;

    fn call(
        ctx: McpToolCtx,
        args: SearchMemoriesArgs,
    ) -> BoxFuture<'static, Result<SearchMemoriesOutput, McpToolError>> {
        Box::pin(async move {
            let query = args.query.trim();
            if query.is_empty() || query.chars().count() > 512 {
                return Err(McpToolError::InvalidInput(
                    "query must be 1..=512 chars".into(),
                ));
            }

            let mode = SearchMode::from(args.mode);
            let embeddings_available = ctx
                .engine()
                .is_some_and(|engine| engine.embed_client().is_some());
            let (mut effective_mode, resolver_degraded) =
                resolve_effective_search_mode(mode, embeddings_available)?;
            let mut degraded_to_lexical = resolver_degraded;
            let since = parse_rfc3339(args.since.as_deref(), "since")?;
            let until = parse_rfc3339(args.until.as_deref(), "until")?;
            let (query_embedding, embedding_model_id) =
                if matches!(effective_mode, SearchMode::Semantic | SearchMode::Hybrid) {
                    let engine = ctx.engine().ok_or_else(|| {
                        McpToolError::Other("engine required for semantic search".into())
                    })?;
                    // The embed client can vanish (or its call can fail) between
                    // the availability probe above and this point. A pure
                    // Semantic request has no lexical fallback, so it hard-fails
                    // with an actionable precondition. A Hybrid request degrades
                    // to lexical-only ranking and flags `degraded_to_lexical`.
                    match embed_query_for_search(engine, query).await {
                        Ok((embedding, model_id)) => (Some(embedding), Some(model_id)),
                        Err(err) => {
                            if matches!(effective_mode, SearchMode::Hybrid) {
                                tracing::warn!(
                                    error = %err,
                                    "hybrid search query embedding unavailable; degrading to lexical",
                                );
                                effective_mode = SearchMode::Lexical;
                                degraded_to_lexical = true;
                                (None, None)
                            } else {
                                return Err(McpToolError::Unavailable(err));
                            }
                        }
                    }
                } else {
                    (None, None)
                };
            let prepared = PreparedSearch {
                query: query.to_string(),
                effective_mode,
                since,
                until,
                query_embedding,
                embedding_model_id,
                body_max_chars: effective_body_max_chars(args.body_max_chars),
                limit: args.limit.clamp(1, 50),
            };
            let spaces = resolve_search_spaces(&ctx, &args.spaces)?;
            let mut all_memories = Vec::new();
            let mut all_neighbor_edges = Vec::new();
            for space in spaces {
                let result = search_one_space(&ctx, &args, &prepared, space).await?;
                degraded_to_lexical |= result.degraded_to_lexical;
                all_memories.extend(result.memories);
                all_neighbor_edges.extend(result.neighbor_edges);
            }
            sort_search_outputs(&mut all_memories, args.order);
            all_memories.truncate(prepared.limit as usize);
            retain_surviving_neighbor_edges(&all_memories, &mut all_neighbor_edges);

            Ok(SearchMemoriesOutput {
                mode: format!("{mode:?}").to_lowercase(),
                degraded_to_lexical,
                memories: all_memories,
                neighbor_edges: all_neighbor_edges,
            })
        })
    }
}

struct PreparedSearch {
    query: String,
    effective_mode: SearchMode,
    since: Option<time::OffsetDateTime>,
    until: Option<time::OffsetDateTime>,
    query_embedding: Option<Vec<f32>>,
    embedding_model_id: Option<String>,
    body_max_chars: usize,
    limit: u32,
}

struct SpaceSearchResult {
    degraded_to_lexical: bool,
    memories: Vec<SearchMemoryOutput>,
    neighbor_edges: Vec<NeighborEdge>,
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
    let mut seen = std::collections::HashSet::with_capacity(raw_spaces.len());
    let mut out = Vec::with_capacity(raw_spaces.len());
    for key in raw_spaces {
        if seen.insert(key.as_str()) {
            out.push(super::memory_spaces::resolve_space_owner(
                ctx,
                Some(key.as_str()),
                super::memory_spaces::SpaceDefault::Current,
            )?);
        }
    }
    Ok(out)
}

async fn search_one_space(
    ctx: &McpToolCtx,
    args: &SearchMemoriesArgs,
    prepared: &PreparedSearch,
    space: super::memory_spaces::ResolvedMemorySpace,
) -> Result<SpaceSearchResult, McpToolError> {
    let req = MemorySearchRequest {
        owner: space.owner,
        read_owners: Vec::new(),
        query: prepared.query.clone(),
        mode: prepared.effective_mode,
        supersession: args.supersession.into(),
        limit: prepared.limit,
        kind: args.kind.map(EntityKind::from),
        schema_id: args.schema_id.clone().map(SchemaId::new),
        tags: args.tags.clone(),
        tag_match: args.tag_match,
        since: prepared.since,
        until: prepared.until,
        order: args.order,
        query_embedding: prepared.query_embedding.clone(),
        embedding_model_id: prepared.embedding_model_id.clone(),
    };
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
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
    let degraded_to_lexical = semantic_search_degraded_to_lexical(prepared.effective_mode, &rows);
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
            let tags = payloads
                .get(&mid)
                .and_then(|payload| payload.tags.clone())
                .unwrap_or_default();
            let body = args
                .include_body
                .then(|| {
                    payloads
                        .get(&mid)
                        .and_then(|payload| payload.body.clone())
                        .map(|body| truncate_body(&body, prepared.body_max_chars))
                })
                .flatten();
            search_memory_output(ctx, &space.key, row, tags, body)
        })
        .collect::<Result<Vec<_>, McpToolError>>()?;
    Ok(SpaceSearchResult {
        degraded_to_lexical,
        memories,
        neighbor_edges,
    })
}

fn sort_search_outputs(memories: &mut [SearchMemoryOutput], order: SearchOrder) {
    match order {
        SearchOrder::Relevance => memories.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.created_at.cmp(&a.created_at))
        }),
        SearchOrder::Recency => memories.sort_by(|a, b| {
            b.created_at.cmp(&a.created_at).then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        }),
    }
}

fn truncate_body(body: &str, max_chars: usize) -> String {
    body.chars().take(max_chars).collect()
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
) -> Result<SearchMemoryOutput, McpToolError> {
    let class = super::get_memory::memory_class(row.kind.as_str())?;
    Ok(SearchMemoryOutput {
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

fn resolve_effective_search_mode(
    requested: SearchMode,
    embeddings_available: bool,
) -> Result<(SearchMode, bool), McpToolError> {
    match (requested, embeddings_available) {
        (SearchMode::Semantic, false) => Err(McpToolError::Unavailable(
            SEMANTIC_SEARCH_UNAVAILABLE.to_string(),
        )),
        (SearchMode::Hybrid, false) => Ok((SearchMode::Lexical, true)),
        (SearchMode::Semantic | SearchMode::Hybrid, true) => Ok((requested, false)),
        (SearchMode::Lexical, _) => Ok((SearchMode::Lexical, false)),
    }
}

/// Compute the query embedding + active embedding-model id, mapping absence or
/// provider failure to a caller-actionable message. The caller decides whether
/// that message hard-fails (pure Semantic) or degrades to lexical (Hybrid).
async fn embed_query_for_search(
    engine: &crate::Engine,
    query: &str,
) -> Result<(Vec<f32>, String), String> {
    let embed = engine
        .embed_client()
        .ok_or_else(|| SEMANTIC_SEARCH_UNAVAILABLE.to_string())?;
    let embedding = embed
        .embed(query)
        .await
        .map_err(|err| format!("semantic search unavailable: embedding provider error: {err}"))?;
    Ok((embedding, embed.model_id().to_string()))
}

/// Drop neighbor edges that no longer touch a surviving (post-truncation)
/// memory, and dedupe by edge handle. Per-space searches over-fetch edges
/// against their own candidate sets; after the merged set is sorted and
/// truncated, edges to hits that were truncated out are dangling references.
fn retain_surviving_neighbor_edges(
    memories: &[SearchMemoryOutput],
    edges: &mut Vec<NeighborEdge>,
) {
    let surviving: std::collections::HashSet<&str> =
        memories.iter().map(|memory| memory.memory.as_str()).collect();
    let mut seen_edges = std::collections::HashSet::new();
    edges.retain(|edge| {
        let touches = edge
            .source
            .as_deref()
            .is_some_and(|source| surviving.contains(source))
            || edge
                .target
                .as_deref()
                .is_some_and(|target| surviving.contains(target));
        // `&&` short-circuits: a non-touching edge is never marked seen, so a
        // later touching duplicate is still evaluated on its own merits.
        touches && seen_edges.insert(edge.handle.clone())
    });
}

/// A `Hybrid` search has silently fallen back to lexical-only ranking when it
/// returned results but none carry a positive semantic similarity — the symptom
/// of an empty or unavailable embedding store (Aquilo FJ#3674). Restricted to
/// `Hybrid`: pure `Semantic` runs no lexical branch (storage gates the lexical
/// query to `Lexical`/`Hybrid`), and an empty result set is a genuine no-match,
/// not a degradation.
fn degraded_to_lexical(mode: SearchMode, no_rows: bool, any_semantic_score: bool) -> bool {
    matches!(mode, SearchMode::Hybrid) && !no_rows && !any_semantic_score
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
mod tests {
    use super::{
        DEFAULT_BODY_MAX_CHARS, NeighborEdge, SEMANTIC_SEARCH_UNAVAILABLE, SearchMemoriesMode,
        SearchMemoriesSupersession, SearchMemoryOutput, degraded_to_lexical,
        effective_body_max_chars, resolve_effective_search_mode,
        retain_surviving_neighbor_edges, truncate_body,
    };
    use crate::mcp::McpToolError;
    use crate::verbs::query::SearchMode;

    fn memory_output(handle: &str) -> SearchMemoryOutput {
        SearchMemoryOutput {
            memory: handle.to_string(),
            space: "current".into(),
            kind: "Fact".into(),
            schema_id: "core/agent-note".into(),
            created_at: "2026-07-05T00:00:00Z".into(),
            snippet: String::new(),
            score: 1.0,
            lexical_score: 1.0,
            similarity_score: 0.0,
            tags: Vec::new(),
            body: None,
        }
    }

    fn neighbor_edge(handle: &str, source: Option<&str>, target: Option<&str>) -> NeighborEdge {
        NeighborEdge {
            handle: handle.to_string(),
            relation: "core/derived-from".into(),
            source: source.map(str::to_string),
            target: target.map(str::to_string),
        }
    }

    #[test]
    fn semantic_unavailable_message_is_provider_neutral() {
        assert!(
            !SEMANTIC_SEARCH_UNAVAILABLE.contains("MISTRAL"),
            "the actionable message must not hardcode a provider env var: {SEMANTIC_SEARCH_UNAVAILABLE}",
        );
        assert!(SEMANTIC_SEARCH_UNAVAILABLE.contains("no embedding client is configured"));
    }

    #[test]
    fn search_mode_and_supersession_accept_mixed_case() {
        assert!(matches!(
            serde_json::from_value::<SearchMemoriesMode>(serde_json::json!("Hybrid")).unwrap(),
            SearchMemoriesMode::Hybrid
        ));
        assert!(matches!(
            serde_json::from_value::<SearchMemoriesMode>(serde_json::json!("SEMANTIC")).unwrap(),
            SearchMemoriesMode::Semantic
        ));
        assert!(matches!(
            serde_json::from_value::<SearchMemoriesMode>(serde_json::json!("lexical")).unwrap(),
            SearchMemoriesMode::Lexical
        ));
        assert!(matches!(
            serde_json::from_value::<SearchMemoriesSupersession>(serde_json::json!("HeadsOnly"))
                .unwrap(),
            SearchMemoriesSupersession::HeadsOnly
        ));
        assert!(matches!(
            serde_json::from_value::<SearchMemoriesSupersession>(serde_json::json!("all")).unwrap(),
            SearchMemoriesSupersession::All
        ));
    }

    #[test]
    fn neighbor_edges_to_truncated_hits_are_dropped_and_deduped() {
        let memories = [memory_output("F:1"), memory_output("A:2")];
        let mut edges = vec![
            // Touches a surviving hit via source.
            neighbor_edge("E:keep", Some("A:2"), Some("F:99")),
            // Both endpoints truncated out — dropped.
            neighbor_edge("E:drop", Some("F:98"), Some("F:97")),
            // Duplicate of E:keep — deduped.
            neighbor_edge("E:keep", Some("A:2"), Some("F:99")),
            // Touches a surviving hit via target.
            neighbor_edge("E:target", Some("F:96"), Some("F:1")),
        ];
        retain_surviving_neighbor_edges(&memories, &mut edges);
        let handles: Vec<_> = edges.iter().map(|edge| edge.handle.as_str()).collect();
        assert_eq!(handles, ["E:keep", "E:target"]);
    }

    #[test]
    fn degraded_flag_only_fires_for_hybrid_with_results_and_no_semantic() {
        // Hybrid returned rows but none carried a semantic score → degraded.
        assert!(degraded_to_lexical(SearchMode::Hybrid, false, false));
        // Hybrid with a real semantic score → healthy.
        assert!(!degraded_to_lexical(SearchMode::Hybrid, false, true));
        // Hybrid with no results at all → a genuine no-match, not degradation.
        assert!(!degraded_to_lexical(SearchMode::Hybrid, true, false));
        // Pure Semantic never reports lexical degradation (no lexical branch runs).
        assert!(!degraded_to_lexical(SearchMode::Semantic, false, false));
        // Lexical is never degraded.
        assert!(!degraded_to_lexical(SearchMode::Lexical, false, false));
    }

    #[test]
    fn resolve_effective_search_mode_degrades_only_implicit_semantic_search() {
        assert_eq!(
            resolve_effective_search_mode(SearchMode::Lexical, false).unwrap(),
            (SearchMode::Lexical, false)
        );
        assert_eq!(
            resolve_effective_search_mode(SearchMode::Lexical, true).unwrap(),
            (SearchMode::Lexical, false)
        );
        assert_eq!(
            resolve_effective_search_mode(SearchMode::Hybrid, false).unwrap(),
            (SearchMode::Lexical, true)
        );
        assert_eq!(
            resolve_effective_search_mode(SearchMode::Hybrid, true).unwrap(),
            (SearchMode::Hybrid, false)
        );
        assert_eq!(
            resolve_effective_search_mode(SearchMode::Semantic, true).unwrap(),
            (SearchMode::Semantic, false)
        );

        // A pure Semantic request with no embedding client is a caller-actionable
        // precondition (`Unavailable`), not an opaque internal fault (`Other`).
        match resolve_effective_search_mode(SearchMode::Semantic, false) {
            Err(McpToolError::Unavailable(message)) => {
                assert_eq!(message, SEMANTIC_SEARCH_UNAVAILABLE);
            }
            other => panic!("expected semantic unavailable error, got {other:?}"),
        }
    }

    #[test]
    fn truncate_body_applies_default_hydration_cap() {
        let body = "x".repeat(DEFAULT_BODY_MAX_CHARS + 1);
        assert_eq!(
            truncate_body(&body, DEFAULT_BODY_MAX_CHARS).chars().count(),
            DEFAULT_BODY_MAX_CHARS
        );
    }

    #[test]
    fn truncate_body_respects_smaller_caller_cap() {
        assert_eq!(truncate_body("abcdef", 3), "abc");
    }

    #[test]
    fn effective_body_max_chars_keeps_server_ceiling() {
        assert_eq!(effective_body_max_chars(None), DEFAULT_BODY_MAX_CHARS);
        assert_eq!(effective_body_max_chars(Some(12)), 12);
        assert_eq!(
            effective_body_max_chars(Some(DEFAULT_BODY_MAX_CHARS + 1)),
            DEFAULT_BODY_MAX_CHARS
        );
    }
}
