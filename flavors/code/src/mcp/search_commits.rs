use std::sync::LazyLock;

use proxima_core::flavor::{BAND_NAME_EXACT, BAND_NAME_RESCUE, BAND_NAME_SUBSTRING, SubstringArm};
use proxima_core::verbs::query::like_pattern;
use proxima_core::{Tool, ToolCtx, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::contract::{COMMIT_SCHEMA_ID, COMMIT_SUMMARY_SCHEMA_ID, band, substring_arm};
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

            let read_owner_ids = super::read_owner_ids(&engine, &ctx).await?;
            let commit_rows = search_commit_rows(
                pool.pool(),
                query,
                repo_id,
                candidate_limit,
                &read_owner_ids,
            )
            .await?;
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
                &read_owner_ids,
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
/// The exact arm scores into `BAND_EXACT`, the same window core's exact arm
/// uses, so a commit hit and a note hit mean the same thing at the same
/// number in a merged result set. A `ts_rank_cd` of `r` renders as
/// `0.50 + LEAST(r, 1.0) * 0.50`, monotone in `r`, so banding does not
/// reorder the arm.
///
/// `p.owner_id = ANY($4)` carries the caller's resolved read set. Without
/// it the composite `gin(owner_id, search_tsv)` is unreachable — the
/// planner sees one indexed column on `p` and the other nowhere — and the
/// scan degrades to driving from `commit_v1` and probing the projection by
/// primary key, once per candidate. Both commit arms drive from the
/// sidecar rather than ranking the projection alone
/// (`RankSource::SidecarWithProjectionOwner`, declared on
/// `CODE_PROJECTION`) because `repo_id` and `change_kind` are the selective
/// predicates and applying them after a projection-side `LIMIT` would
/// answer a repo-scoped search with the wrong repository's rows. See
/// `search_chunks::CHUNK_GIN_SQL` for the full argument.
///
/// The exact arm passes NO `ts_rank_cd` normalization flag where core's
/// passes `32`. The divergence is declared, not implicit:
/// `BAND_EXACT.with_normalization(TS_RANK_NORMALIZATION_NONE)` in
/// `COMMIT_BANDS`, and `Band::normalization_arg` renders the absence — so
/// declaring it moved no score.
static COMMIT_SEARCH_SQL: LazyLock<String> = LazyLock::new(|| {
    let exact = band(COMMIT_SCHEMA_ID, BAND_NAME_EXACT);
    let rescue = band(COMMIT_SCHEMA_ID, BAND_NAME_RESCUE);
    let (exact_floor, exact_width) = exact.parts();
    let (rescue_floor, rescue_width) = rescue.parts();
    let exact_norm = exact.normalization_arg();
    let rescue_norm = rescue.normalization_arg();
    format!(
        "
WITH q AS (
     SELECT proxima_code.commit_search_web_tsquery($1) AS tsq,
            proxima_code.commit_search_any_tsquery($1) AS any_tsq
)
SELECT c.t AS memory_id,
       GREATEST(
           CASE WHEN p.search_tsv @@ q.tsq
                THEN {exact_floor} + LEAST(ts_rank_cd(p.search_tsv, q.tsq{exact_norm}), 1.0) * {exact_width}
                ELSE 0.0 END,
           CASE WHEN q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq
                THEN {rescue_floor} + LEAST(ts_rank(p.search_tsv, q.any_tsq{rescue_norm}) * 100.0, 1.0) * {rescue_width}
                ELSE 0.0 END
       )::real AS score
FROM q, proxima_code.commit_v1 c
JOIN proxima_code.projection p
  ON p.memory_id = c.t
 AND p.schema_id = '{COMMIT_SCHEMA_ID}'
 AND p.owner_id = ANY($4::uuid[])
WHERE ($2::uuid IS NULL OR c.repo_id = $2)
  AND (p.search_tsv @@ q.tsq
       OR (q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq))
ORDER BY score DESC, c.committer_time DESC
LIMIT $3
"
    )
});

/// The commit-summary GIN arm. Same shape and same bands as
/// [`COMMIT_SEARCH_SQL`], read off the summary schema's own declaration.
static SUMMARY_SEARCH_SQL: LazyLock<String> = LazyLock::new(|| {
    let exact = band(COMMIT_SUMMARY_SCHEMA_ID, BAND_NAME_EXACT);
    let rescue = band(COMMIT_SUMMARY_SCHEMA_ID, BAND_NAME_RESCUE);
    let (exact_floor, exact_width) = exact.parts();
    let (rescue_floor, rescue_width) = rescue.parts();
    let exact_norm = exact.normalization_arg();
    let rescue_norm = rescue.normalization_arg();
    format!(
        "
WITH q AS (
     SELECT proxima_code.commit_search_web_tsquery($1) AS tsq,
            proxima_code.commit_search_any_tsquery($1) AS any_tsq
)
SELECT s.t AS memory_id,
       GREATEST(
           CASE WHEN p.search_tsv @@ q.tsq
                THEN {exact_floor} + LEAST(ts_rank_cd(p.search_tsv, q.tsq{exact_norm}), 1.0) * {exact_width}
                ELSE 0.0 END,
           CASE WHEN q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq
                THEN {rescue_floor} + LEAST(ts_rank(p.search_tsv, q.any_tsq{rescue_norm}) * 100.0, 1.0) * {rescue_width}
                ELSE 0.0 END
       )::real AS score
FROM q, proxima_code.commit_summary_v1 s
JOIN proxima_code.projection p
  ON p.memory_id = s.t
 AND p.schema_id = '{COMMIT_SUMMARY_SCHEMA_ID}'
 AND p.owner_id = ANY($5::uuid[])
WHERE ($2::uuid IS NULL OR s.repo_id = $2)
  AND ($3::text IS NULL OR s.change_kind = $3)
  AND (p.search_tsv @@ q.tsq
       OR (q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq))
ORDER BY score DESC, s.t DESC
LIMIT $4
"
    )
});

/// The commit substring arm, `SameTableLike` as declared.
///
/// - **The score is the declared band**, not a bare `0.25::real`. Commit
///   search's substring window is `flavor0::BAND_SUBSTRING`, referenced —
///   which is the band-comparability claim for this schema.
/// - **`p.owner_id = ANY($4)`** keeps candidate generation owner-scoped, so
///   a neighbour's repository cannot consume the whole budget before
///   authorization runs. The owner reaches a code sidecar through the
///   Memory, and the join is to this flavor's OWN projection — never
///   `proxima_core.memory`, which flavor SQL may not name.
static COMMIT_LIKE_SQL: LazyLock<String> = LazyLock::new(|| {
    let (floor, _) = band(COMMIT_SCHEMA_ID, BAND_NAME_SUBSTRING).parts();
    format!(
        "
SELECT c.t AS memory_id,
       {floor}::real AS score
FROM proxima_code.commit_v1 c
JOIN proxima_code.projection p
  ON p.memory_id = c.t
 AND p.schema_id = '{COMMIT_SCHEMA_ID}'
 AND p.owner_id = ANY($4::uuid[])
WHERE ($2::uuid IS NULL OR c.repo_id = $2)
  AND (
        lower(c.sha) LIKE $1 ESCAPE '\\'
     OR lower(c.message) LIKE $1 ESCAPE '\\'
     OR lower(c.author_name) LIKE $1 ESCAPE '\\'
     OR lower(c.author_email) LIKE $1 ESCAPE '\\'
  )
ORDER BY score DESC, c.committer_time DESC
LIMIT $3
"
    )
});

/// The commit-summary substring arm. Declared band and owner predicate as
/// in [`COMMIT_LIKE_SQL`].
static SUMMARY_LIKE_SQL: LazyLock<String> = LazyLock::new(|| {
    let (floor, _) = band(COMMIT_SUMMARY_SCHEMA_ID, BAND_NAME_SUBSTRING).parts();
    format!(
        "
SELECT s.t AS memory_id,
       {floor}::real AS score
FROM proxima_code.commit_summary_v1 s
JOIN proxima_code.projection p
  ON p.memory_id = s.t
 AND p.schema_id = '{COMMIT_SUMMARY_SCHEMA_ID}'
 AND p.owner_id = ANY($5::uuid[])
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
"
    )
});

/// Whether `schema_id` opts into the substring shape this module renders.
///
/// A schema that declares no `SameTableLike` arm contributes no `LIKE`
/// statement. Each of these tools ranks exactly one schema, so "the ranked
/// arm returned nothing for this schema" and "the ranked arm returned
/// nothing" are the same sentence — the gate is per schema and the trigger
/// is the empty ranked result.
fn same_table_like_is_declared(schema_id: &str) -> bool {
    matches!(substring_arm(schema_id), Some(SubstringArm::SameTableLike))
}

async fn search_commit_rows(
    pool: &PgPool,
    query: &str,
    repo_id: Option<Uuid>,
    limit: i64,
    read_owner_ids: &[Uuid],
) -> Result<Vec<ScoredMemoryRow>, ToolError> {
    let gin: Vec<ScoredMemoryRow> = // SQL-POLICY: fixed-fragment
    sqlx::query_as(sqlx::AssertSqlSafe(COMMIT_SEARCH_SQL.as_str()))
        .bind(query)
        .bind(repo_id)
        .bind(limit)
        .bind(read_owner_ids)
        .fetch_all(pool)
        .await
        .map_err(map_storage)?;
    if gin.is_empty() && same_table_like_is_declared(COMMIT_SCHEMA_ID) {
        // SQL-POLICY: fixed-fragment
        sqlx::query_as(sqlx::AssertSqlSafe(COMMIT_LIKE_SQL.as_str()))
            .bind(like_pattern(query))
            .bind(repo_id)
            .bind(limit)
            .bind(read_owner_ids)
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
    read_owner_ids: &[Uuid],
) -> Result<Vec<ScoredMemoryRow>, ToolError> {
    let gin: Vec<ScoredMemoryRow> = // SQL-POLICY: fixed-fragment
    sqlx::query_as(sqlx::AssertSqlSafe(SUMMARY_SEARCH_SQL.as_str()))
        .bind(query)
        .bind(repo_id)
        .bind(change_kind)
        .bind(limit)
        .bind(read_owner_ids)
        .fetch_all(pool)
        .await
        .map_err(map_storage)?;
    if gin.is_empty() && same_table_like_is_declared(COMMIT_SUMMARY_SCHEMA_ID) {
        // SQL-POLICY: fixed-fragment
        sqlx::query_as(sqlx::AssertSqlSafe(SUMMARY_LIKE_SQL.as_str()))
            .bind(like_pattern(query))
            .bind(repo_id)
            .bind(change_kind)
            .bind(limit)
            .bind(read_owner_ids)
            .fetch_all(pool)
            .await
            .map_err(map_storage)
    } else {
        Ok(gin)
    }
}

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn commit_like_sql_for_tests() -> &'static str {
    COMMIT_LIKE_SQL.as_str()
}

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn summary_like_sql_for_tests() -> &'static str {
    SUMMARY_LIKE_SQL.as_str()
}

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn commit_search_sql_for_tests() -> &'static str {
    COMMIT_SEARCH_SQL.as_str()
}

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn summary_search_sql_for_tests() -> &'static str {
    SUMMARY_SEARCH_SQL.as_str()
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
            "the vector is read from the projection"
        );
        assert!(
            !super::SUMMARY_SEARCH_SQL.contains(&needle),
            "summary search must @@ the stored vector, not recompute to_tsvector"
        );
        assert!(super::SUMMARY_SEARCH_SQL.contains("p.search_tsv @@"));
    }

    /// The RANKED arms bind the owner too, and nothing pinned it.
    ///
    /// `AND $4::uuid[] IS NOT NULL` binds the parameter and narrows nothing,
    /// which would leave candidate generation owner-blind while the whole
    /// workspace stayed green. The claim covers all four arms, not only the
    /// three `LIKE` ones.
    #[test]
    fn the_ranked_arms_bind_the_owner_as_a_predicate() {
        assert!(
            super::COMMIT_SEARCH_SQL.contains("AND p.owner_id = ANY($4::uuid[])"),
            "the commit arm narrows on the owner, on the projection's own column"
        );
        assert!(
            super::SUMMARY_SEARCH_SQL.contains("AND p.owner_id = ANY($5::uuid[])"),
            "the summary arm narrows on the owner; its bind is $5, one later than \
             the commit arm's, because it also binds a kind"
        );
        // `IS NOT NULL` on the same bind reads as a use and narrows
        // nothing. Naming the shape is what makes this assertion outlive
        // the one spelling it currently rejects.
        for sql in [
            super::COMMIT_SEARCH_SQL.as_str(),
            super::SUMMARY_SEARCH_SQL.as_str(),
        ] {
            assert!(
                !sql.contains("::uuid[] IS NOT NULL"),
                "a bind that is only checked for NULL is not an owner predicate"
            );
        }
    }

    /// The exact arm renders the window it DECLARES, and the declaration is
    /// `flavor0::BAND_EXACT` with one property changed — so the window is
    /// core's and the normalization divergence is a declared value rather
    /// than an accident, comparable with the rescue arm's scores and core's.
    #[test]
    fn the_exact_arm_is_banded_like_cores() {
        use proxima_core::flavor::{BAND_NAME_EXACT, TS_RANK_NORMALIZATION_NONE};

        let declared = crate::contract::band(crate::contract::COMMIT_SCHEMA_ID, BAND_NAME_EXACT);
        assert_eq!(
            (declared.floor, declared.ceiling),
            (
                proxima_core::flavor0::BAND_EXACT.floor,
                proxima_core::flavor0::BAND_EXACT.ceiling
            ),
            "the window is core's, referenced rather than respelled"
        );
        assert_eq!(
            declared.normalization, TS_RANK_NORMALIZATION_NONE,
            "this arm passes no normalization flag; declaring that must not add one"
        );
        let (floor, width) = declared.parts();
        assert_eq!((floor.as_str(), width.as_str()), ("0.50", "0.50"));
        assert!(
            super::COMMIT_SEARCH_SQL
                .contains("0.50 + LEAST(ts_rank_cd(p.search_tsv, q.tsq), 1.0) * 0.50"),
            "the exact arm renders the declared window and no normalization argument"
        );
        assert!(
            super::SUMMARY_SEARCH_SQL
                .contains("0.50 + LEAST(ts_rank_cd(p.search_tsv, q.tsq), 1.0) * 0.50"),
        );
    }

    /// The substring arms are DECLARED and OWNER-SCOPED.
    ///
    /// Authorization admits later, so an owner-blind candidate scan leaks
    /// nothing — but a neighbour's corpus could consume the whole candidate
    /// budget. The owner predicate rides a join to this flavor's own
    /// projection, never `proxima_core.memory`.
    #[test]
    fn the_substring_arms_are_declared_and_owner_scoped() {
        use proxima_core::flavor::SubstringArm;

        for schema_id in [
            crate::contract::COMMIT_SCHEMA_ID,
            crate::contract::COMMIT_SUMMARY_SCHEMA_ID,
        ] {
            assert_eq!(
                crate::contract::substring_arm(schema_id),
                Some(SubstringArm::SameTableLike),
                "{schema_id} declares the arm this module renders"
            );
            assert!(super::same_table_like_is_declared(schema_id));
        }
        // Spelled in two halves so this assertion is not itself a flavor
        // literal naming a core table — see
        // `scripts/check-architecture-guardrails.py`.
        let core_memory = format!("{}{}", "proxima_core", ".memory");
        for sql in [
            super::COMMIT_LIKE_SQL.as_str(),
            super::SUMMARY_LIKE_SQL.as_str(),
        ] {
            assert!(sql.contains("LIKE"), "the substring arm still LIKEs");
            assert!(
                sql.contains("JOIN proxima_code.projection p"),
                "the owner reaches a code sidecar through this flavor's own projection"
            );
            assert!(
                sql.contains("p.owner_id = ANY("),
                "candidate generation must not be owner-blind"
            );
            assert!(
                !sql.contains(&core_memory),
                "flavor SQL may not name a core table for this"
            );
            assert!(
                sql.contains("0.25::real AS score"),
                "the flat score is the declared substring band"
            );
        }
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
    }
}
