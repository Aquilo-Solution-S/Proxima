use std::sync::LazyLock;

use proxima_core::flavor::{BAND_EXACT, BAND_RESCUE};
use proxima_core::verbs::query::like_pattern;
use proxima_core::{Tool, ToolCtx, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::contract::band_parts;
use crate::payloads::{CommitSummaryV1, CommitV1};

use super::CodeToolCtxExt;
use super::code_store;
use super::sql::{map_storage, resolve_repo_identifier};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeSearchCommitsArgs {
    #[schemars(
        length(max = proxima_core::MAX_QUERY_CHARS),
        description = "Lexical query string for Git commit and commit-summary search. Matches SHA, message, and summary text; 1 to 512 chars."
    )]
    pub query: String,
    #[schemars(
        range(min = 1),
        description = "Optional maximum number of commit and summary matches. Omit or null for 10; values above 50 are clamped, and 0 is rejected."
    )]
    pub limit: Option<u32>,
    #[schemars(
        description = "Optional repo handle filter, typically `R...`. Omit or null to search all visible repos."
    )]
    pub repo_handle: Option<String>,
    #[schemars(
        description = "Optional commit-summary change_kind filter such as `feature`, `fix`, or `docs`. Omit or null for all kinds."
    )]
    pub change_kind: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CodeSearchCommitsOutput {
    pub commits: Vec<CommitMatch>,
    pub summaries: Vec<SummaryMatch>,
    /// At least one further authorized commit match exists past `limit`
    /// in the scanned candidate window; retry with a higher limit
    /// (max 50) or narrow the query. Truncation is a signal, never
    /// silent.
    pub commits_has_more: bool,
    /// Same signal for the `summaries` list.
    pub summaries_has_more: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CommitMatch {
    pub handle: String,
    pub repo_handle: String,
    pub sha: String,
    pub author_name: String,
    // `time`'s own `Serialize` writes a timestamp string, so the schema says
    // string; schemars has no impl of its own for `OffsetDateTime`.
    #[schemars(with = "String")]
    pub committer_time: time::OffsetDateTime,
    pub message_snippet: String,
    pub score: f32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SummaryMatch {
    pub handle: String,
    pub repo_handle: String,
    pub commit_sha: String,
    pub change_kind: String,
    pub key_files: Vec<String>,
    pub summary: String,
    pub score: f32,
}

#[derive(Debug)]
pub struct CodeSearchCommitsTool;

impl Tool for CodeSearchCommitsTool {
    const NAME: &'static str = "proxima-code_search_commits";
    const DESCRIPTION: &'static str =
        "Search Git commit facts and operator-authored commit summaries.";
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> = Some(super::READ_ONLY);

    type Args = CodeSearchCommitsArgs;
    type Output = CodeSearchCommitsOutput;

    #[allow(clippy::too_many_lines)]
    fn call(
        ctx: ToolCtx,
        args: CodeSearchCommitsArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeSearchCommitsOutput, ToolError>> {
        Box::pin(async move {
            let query = proxima_core::validate_search_query(&args.query)?;
            proxima_core::reject_zero_limit(args.limit)?;
            let limit = args.limit.unwrap_or(10).min(50);
            let repo_id = match args.repo_handle.as_deref() {
                Some(handle) => Some(resolve_repo_identifier(&ctx, handle).await?),
                None => None,
            };
            let pool = code_store(&ctx)?;
            let engine = super::engine(&ctx)?;
            let candidate_limit = i64::from(limit.saturating_mul(4).max(limit).min(200));

            let commit_rows =
                search_commit_rows(pool.pool(), query, repo_id, candidate_limit).await?;
            let commit_ids = commit_rows
                .iter()
                .map(|row| row.memory_id)
                .collect::<Vec<_>>();
            let commit_scores = commit_rows
                .into_iter()
                .map(|row| (row.memory_id, row.score))
                .collect::<std::collections::HashMap<_, _>>();
            let page_len = usize::try_from(limit).unwrap_or(usize::MAX);
            let mut commit_payloads = pool
                .authorized_fact_payloads::<CommitV1>(
                    &engine,
                    ctx.authz(),
                    ctx.owner(),
                    &commit_ids,
                    page_len.saturating_add(1),
                )
                .await?;
            let commits_has_more = commit_payloads.len() > page_len;
            commit_payloads.truncate(page_len);
            let commits = commit_payloads
                .into_iter()
                .map(|(memory_id, row)| CommitMatch {
                    handle: ctx.format_fact_memory(memory_id),
                    repo_handle: ctx.format_flavor_object(
                        super::REPO_HANDLE_KIND,
                        row.repo_id,
                        super::REPO_HANDLE_PREFIX,
                    ),
                    sha: row.sha,
                    author_name: row.author_name,
                    committer_time: row.committer_time,
                    message_snippet: row.message.chars().take(480).collect(),
                    score: commit_scores
                        .get(&memory_id.into_inner())
                        .copied()
                        .unwrap_or_default(),
                })
                .collect();

            let summary_rows = search_summary_rows(
                pool.pool(),
                query,
                repo_id,
                args.change_kind.as_deref(),
                candidate_limit,
            )
            .await?;
            let summary_ids = summary_rows
                .iter()
                .map(|row| row.memory_id)
                .collect::<Vec<_>>();
            let summary_scores = summary_rows
                .into_iter()
                .map(|row| (row.memory_id, row.score))
                .collect::<std::collections::HashMap<_, _>>();
            let mut summary_payloads = pool
                .authorized_abstraction_payloads::<CommitSummaryV1>(
                    &engine,
                    ctx.authz(),
                    ctx.owner(),
                    &summary_ids,
                    page_len.saturating_add(1),
                )
                .await?;
            let summaries_has_more = summary_payloads.len() > page_len;
            summary_payloads.truncate(page_len);
            let summaries = summary_payloads
                .into_iter()
                .map(|(memory_id, row)| SummaryMatch {
                    // CommitSummaryV1 is an AbstractionPayload; emit an `A:` handle.
                    handle: ctx.format_abstraction_memory(memory_id),
                    repo_handle: ctx.format_flavor_object(
                        super::REPO_HANDLE_KIND,
                        row.repo_id,
                        super::REPO_HANDLE_PREFIX,
                    ),
                    commit_sha: row.commit_sha,
                    change_kind: row.change_kind,
                    key_files: row.key_files,
                    summary: row.summary,
                    score: summary_scores
                        .get(&memory_id.into_inner())
                        .copied()
                        .unwrap_or_default(),
                })
                .collect();

            Ok(CodeSearchCommitsOutput {
                commits,
                summaries,
                commits_has_more,
                summaries_has_more,
            })
        })
    }
}

/// The commit GIN arm, over `proxima_code.projection`.
///
/// The exact arm was RAW `ts_rank_cd` — an unbanded score in a result set
/// that is merged with banded ones. It is `BAND_EXACT` now, the same window
/// core's exact arm uses, so a commit hit and a note hit mean the same
/// thing at the same number. Scores in the exact arm therefore MOVE: a raw
/// `ts_rank_cd` of `r` becomes `0.50 + LEAST(r, 1.0) * 0.50`. That is
/// monotone in `r`, so the ORDER within this arm is unchanged.
static COMMIT_SEARCH_SQL: LazyLock<String> = LazyLock::new(|| {
    let (exact_floor, exact_width) = band_parts(BAND_EXACT);
    let (rescue_floor, rescue_width) = band_parts(BAND_RESCUE);
    format!(
        "
WITH q AS (
     SELECT proxima_code.commit_search_web_tsquery($1) AS tsq,
            proxima_code.commit_search_any_tsquery($1) AS any_tsq
)
SELECT c.t AS memory_id,
       GREATEST(
           CASE WHEN p.search_tsv @@ q.tsq
                THEN {exact_floor} + LEAST(ts_rank_cd(p.search_tsv, q.tsq), 1.0) * {exact_width}
                ELSE 0.0 END,
           CASE WHEN q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq
                THEN {rescue_floor} + LEAST(ts_rank(p.search_tsv, q.any_tsq, 1|32) * 100.0, 1.0) * {rescue_width}
                ELSE 0.0 END
       )::real AS score
FROM q, proxima_code.commit_v1 c
JOIN proxima_code.projection p
  ON p.memory_id = c.t
 AND p.schema_id = 'proxima-code/commit-v1'
WHERE ($2::uuid IS NULL OR c.repo_id = $2)
  AND (p.search_tsv @@ q.tsq
       OR (q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq))
ORDER BY score DESC, c.committer_time DESC
LIMIT $3
"
    )
});

/// The commit-summary GIN arm. Same rewrite, same band.
static SUMMARY_SEARCH_SQL: LazyLock<String> = LazyLock::new(|| {
    let (exact_floor, exact_width) = band_parts(BAND_EXACT);
    let (rescue_floor, rescue_width) = band_parts(BAND_RESCUE);
    format!(
        "
WITH q AS (
     SELECT proxima_code.commit_search_web_tsquery($1) AS tsq,
            proxima_code.commit_search_any_tsquery($1) AS any_tsq
)
SELECT s.t AS memory_id,
       GREATEST(
           CASE WHEN p.search_tsv @@ q.tsq
                THEN {exact_floor} + LEAST(ts_rank_cd(p.search_tsv, q.tsq), 1.0) * {exact_width}
                ELSE 0.0 END,
           CASE WHEN q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq
                THEN {rescue_floor} + LEAST(ts_rank(p.search_tsv, q.any_tsq, 1|32) * 100.0, 1.0) * {rescue_width}
                ELSE 0.0 END
       )::real AS score
FROM q, proxima_code.commit_summary_v1 s
JOIN proxima_code.projection p
  ON p.memory_id = s.t
 AND p.schema_id = 'proxima-code/commit-summary-v1'
WHERE ($2::uuid IS NULL OR s.repo_id = $2)
  AND ($3::text IS NULL OR s.change_kind = $3)
  AND (p.search_tsv @@ q.tsq
       OR (q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq))
ORDER BY score DESC, s.t DESC
LIMIT $4
"
    )
});

const COMMIT_LIKE_SQL: &str = "
SELECT c.t AS memory_id,
       0.25::real AS score
FROM proxima_code.commit_v1 c
WHERE ($2::uuid IS NULL OR c.repo_id = $2)
  AND (
        lower(c.sha) LIKE $1 ESCAPE '\\'
     OR lower(c.message) LIKE $1 ESCAPE '\\'
     OR lower(c.author_name) LIKE $1 ESCAPE '\\'
     OR lower(c.author_email) LIKE $1 ESCAPE '\\'
  )
ORDER BY score DESC, c.committer_time DESC
LIMIT $3
";

const SUMMARY_LIKE_SQL: &str = "
SELECT s.t AS memory_id,
       0.25::real AS score
FROM proxima_code.commit_summary_v1 s
WHERE ($2::uuid IS NULL OR s.repo_id = $2)
  AND ($3::text IS NULL OR s.change_kind = $3)
  AND (
        lower(s.commit_sha) LIKE $1 ESCAPE '\\'
     OR lower(s.summary) LIKE $1 ESCAPE '\\'
     OR EXISTS (
            SELECT 1 FROM unnest(s.key_files) AS f
             WHERE lower(f) LIKE $1 ESCAPE '\\'
        )
  )
ORDER BY score DESC, s.t DESC
LIMIT $4
";

async fn search_commit_rows(
    pool: &PgPool,
    query: &str,
    repo_id: Option<Uuid>,
    limit: i64,
) -> Result<Vec<ScoredMemoryRow>, ToolError> {
    let gin: Vec<ScoredMemoryRow> = // SQL-POLICY: fixed-fragment
    sqlx::query_as(sqlx::AssertSqlSafe(COMMIT_SEARCH_SQL.as_str()))
        .bind(query)
        .bind(repo_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_storage)?;
    if gin.is_empty() {
        sqlx::query_as(COMMIT_LIKE_SQL)
            .bind(like_pattern(query))
            .bind(repo_id)
            .bind(limit)
            .fetch_all(pool)
            .await
            .map_err(map_storage)
    } else {
        Ok(gin)
    }
}

async fn search_summary_rows(
    pool: &PgPool,
    query: &str,
    repo_id: Option<Uuid>,
    change_kind: Option<&str>,
    limit: i64,
) -> Result<Vec<ScoredMemoryRow>, ToolError> {
    let gin: Vec<ScoredMemoryRow> = // SQL-POLICY: fixed-fragment
    sqlx::query_as(sqlx::AssertSqlSafe(SUMMARY_SEARCH_SQL.as_str()))
        .bind(query)
        .bind(repo_id)
        .bind(change_kind)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_storage)?;
    if gin.is_empty() {
        sqlx::query_as(SUMMARY_LIKE_SQL)
            .bind(like_pattern(query))
            .bind(repo_id)
            .bind(change_kind)
            .bind(limit)
            .fetch_all(pool)
            .await
            .map_err(map_storage)
    } else {
        Ok(gin)
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ScoredMemoryRow {
    memory_id: uuid::Uuid,
    score: f32,
}

#[cfg(test)]
mod tests {
    #[test]
    fn commit_search_reads_the_projection_vector() {
        let needle = format!("{}{}", "to_ts", "vector(");
        assert!(
            !super::COMMIT_SEARCH_SQL.contains(&needle),
            "commit search must @@ the stored vector, not recompute to_tsvector"
        );
        assert!(super::COMMIT_SEARCH_SQL.contains("p.search_tsv @@"));
        assert!(
            super::COMMIT_SEARCH_SQL.contains("proxima_code.projection p"),
            "the vector lives on the projection now"
        );
        assert!(
            !super::SUMMARY_SEARCH_SQL.contains(&needle),
            "summary search must @@ the stored vector, not recompute to_tsvector"
        );
        assert!(super::SUMMARY_SEARCH_SQL.contains("p.search_tsv @@"));
    }

    /// The exact arm was raw `ts_rank_cd`, unbanded, merged with banded
    /// scores from the rescue arm and from core. It reads `BAND_EXACT` now.
    #[test]
    fn the_exact_arm_is_banded_like_cores() {
        use proxima_core::flavor::BAND_EXACT;
        let (floor, width) = crate::contract::band_parts(BAND_EXACT);
        assert_eq!((floor.as_str(), width.as_str()), ("0.50", "0.50"));
        assert!(
            super::COMMIT_SEARCH_SQL
                .contains("0.50 + LEAST(ts_rank_cd(p.search_tsv, q.tsq), 1.0) * 0.50"),
            "the exact arm renders BAND_EXACT"
        );
        assert!(
            super::SUMMARY_SEARCH_SQL
                .contains("0.50 + LEAST(ts_rank_cd(p.search_tsv, q.tsq), 1.0) * 0.50"),
        );
    }

    #[test]
    fn commit_search_matches_prose_tsv_and_like_on_gin_miss() {
        let src = include_str!("search_commits.rs");
        let prod = src.split("mod tests").next().expect("production");
        assert!(
            prod.contains("commit_search_web_tsquery")
                && prod.contains("commit_search_any_tsquery"),
            "query side must use the SQL prose query authorities"
        );
        assert!(
            super::COMMIT_LIKE_SQL.contains("LIKE") && super::SUMMARY_LIKE_SQL.contains("LIKE"),
            "GIN miss must have a LIKE arm"
        );
    }
}
