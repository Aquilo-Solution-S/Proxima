use proxima_core::MemoryId;
use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::sql::{map_storage, owner_principal, resolve_repo_identifier};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeSearchCommitsArgs {
    pub query: String,
    pub limit: Option<u32>,
    pub repo_handle: Option<String>,
    pub change_kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CodeSearchCommitsOutput {
    pub commits: Vec<CommitMatch>,
    pub summaries: Vec<SummaryMatch>,
}

#[derive(Debug, Serialize)]
pub struct CommitMatch {
    pub handle: String,
    pub repo_handle: String,
    pub sha: String,
    pub author_name: String,
    pub committer_time: time::OffsetDateTime,
    pub message_snippet: String,
    pub score: f32,
}

#[derive(Debug, Serialize)]
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

impl McpTool for CodeSearchCommitsTool {
    const NAME: &'static str = "proxima-code/code_search_commits";
    const DESCRIPTION: &'static str =
        "Search Git commit facts and operator-authored commit summaries.";

    type Args = CodeSearchCommitsArgs;
    type Output = CodeSearchCommitsOutput;

    fn call(
        ctx: McpToolCtx,
        args: CodeSearchCommitsArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeSearchCommitsOutput, McpToolError>> {
        Box::pin(async move {
            let query = args.query.trim();
            if query.is_empty() || query.chars().count() > 512 {
                return Err(McpToolError::InvalidInput(
                    "query must be 1..=512 chars".into(),
                ));
            }
            let limit = args.limit.unwrap_or(10).min(50);
            let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
            let repo_id = match args.repo_handle.as_deref() {
                Some(handle) => Some(resolve_repo_identifier(&ctx, handle).await?),
                None => None,
            };

            let commit_rows: Vec<CommitRow> = sqlx::query_as(COMMIT_SEARCH_SQL)
                .bind(owner_kind)
                .bind(owner_principal_id)
                .bind(query)
                .bind(repo_id)
                .bind(i64::from(limit))
                .fetch_all(&ctx.pool)
                .await
                .map_err(map_storage)?;
            let commits = commit_rows
                .into_iter()
                .map(|row| {
                    let handle = ctx.handles.assign_memory(MemoryId::new(row.memory_id));
                    let repo_handle = ctx.handles.assign_flavor_object(
                        super::REPO_HANDLE_KIND,
                        row.repo_id,
                        super::REPO_HANDLE_PREFIX,
                    );
                    CommitMatch {
                        handle: handle.as_str().to_string(),
                        repo_handle: repo_handle.as_str().to_string(),
                        sha: row.sha,
                        author_name: row.author_name,
                        committer_time: row.committer_time,
                        message_snippet: row.message_snippet,
                        score: row.score,
                    }
                })
                .collect();

            let summary_rows: Vec<SummaryRow> = sqlx::query_as(SUMMARY_SEARCH_SQL)
                .bind(owner_kind)
                .bind(owner_principal_id)
                .bind(query)
                .bind(repo_id)
                .bind(args.change_kind.as_deref())
                .bind(i64::from(limit))
                .fetch_all(&ctx.pool)
                .await
                .map_err(map_storage)?;
            let summaries = summary_rows
                .into_iter()
                .map(|row| {
                    let handle = ctx.handles.assign_memory(MemoryId::new(row.memory_id));
                    let repo_handle = ctx.handles.assign_flavor_object(
                        super::REPO_HANDLE_KIND,
                        row.repo_id,
                        super::REPO_HANDLE_PREFIX,
                    );
                    SummaryMatch {
                        handle: handle.as_str().to_string(),
                        repo_handle: repo_handle.as_str().to_string(),
                        commit_sha: row.commit_sha,
                        change_kind: row.change_kind,
                        key_files: row.key_files,
                        summary: row.summary,
                        score: row.score,
                    }
                })
                .collect();

            Ok(CodeSearchCommitsOutput { commits, summaries })
        })
    }
}

const COMMIT_SEARCH_SQL: &str = r"
WITH q AS (SELECT websearch_to_tsquery('pg_catalog.simple'::regconfig, $3) AS tsq)
SELECT c.memory_id, c.repo_id, c.sha, c.author_name, c.committer_time,
       left(c.message, 480) AS message_snippet,
       ts_rank_cd(to_tsvector('pg_catalog.simple'::regconfig, c.sha || ' ' || c.message), q.tsq) AS score
FROM q, proxima_core.memories m
JOIN proxima_code.commit_v1 c USING (memory_id)
WHERE m.owner_principal_kind = $1
  AND m.owner_principal_id = $2
  AND ($4::uuid IS NULL OR c.repo_id = $4)
  AND to_tsvector('pg_catalog.simple'::regconfig, c.sha || ' ' || c.message) @@ q.tsq
ORDER BY score DESC, c.committer_time DESC
LIMIT $5
";

const SUMMARY_SEARCH_SQL: &str = r"
WITH q AS (SELECT websearch_to_tsquery('pg_catalog.simple'::regconfig, $3) AS tsq)
SELECT s.memory_id, s.repo_id, s.commit_sha, s.summary,
       s.key_files, s.change_kind,
       ts_rank_cd(to_tsvector(
           'pg_catalog.simple'::regconfig,
           s.commit_sha || ' ' || s.summary || ' ' || proxima_code.text_array_search(s.key_files)
       ), q.tsq) AS score
FROM q, proxima_core.memories m
JOIN proxima_code.commit_summary_v1 s USING (memory_id)
WHERE m.owner_principal_kind = $1
  AND m.owner_principal_id = $2
  AND ($4::uuid IS NULL OR s.repo_id = $4)
  AND ($5::text IS NULL OR s.change_kind = $5)
  AND to_tsvector(
      'pg_catalog.simple'::regconfig,
      s.commit_sha || ' ' || s.summary || ' ' || proxima_code.text_array_search(s.key_files)
  ) @@ q.tsq
ORDER BY score DESC, s.memory_id DESC
LIMIT $6
";

#[derive(Debug, sqlx::FromRow)]
struct CommitRow {
    memory_id: uuid::Uuid,
    repo_id: uuid::Uuid,
    sha: String,
    author_name: String,
    committer_time: time::OffsetDateTime,
    message_snippet: String,
    score: f32,
}

#[derive(Debug, sqlx::FromRow)]
struct SummaryRow {
    memory_id: uuid::Uuid,
    repo_id: uuid::Uuid,
    commit_sha: String,
    summary: String,
    key_files: Vec<String>,
    change_kind: String,
    score: f32,
}
